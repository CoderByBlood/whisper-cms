use axum::{
    extract::{Form, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use time::format_description::well_known::Rfc3339;

use crate::{
    phase::Phase,
    plan::InstallForm,
    progress::Msg,
    state::OperState,
    steps::{parse_step, run_all_from, step_name},
};
use infra::install::resume;

/// Accept form input, validate, and stage the plan (no plaintext persisted beyond memory).
#[tracing::instrument(skip_all)]
pub async fn post_config(State(app): State<OperState>, Form(form): Form<InstallForm>) -> Response {
    match form.validate_into_plan() {
        Ok(plan) => {
            // Overwrite any previous plan; avoid cloning secrets.
            app.plan.write().unwrap().replace(plan);
            Redirect::to("/install/run").into_response()
        }
        Err(errs) => {
            // Keep it simple for now; you can render a template with errors later.
            let body = errs
                .into_iter()
                .map(|e| format!("- {e}"))
                .collect::<Vec<_>>()
                .join("\n");
            (
                StatusCode::BAD_REQUEST,
                format!("Configuration errors:\n{body}"),
            )
                .into_response()
        }
    }
}

/// Start (or resume) the installation run.
/// Assumes we're in the Install phase (router is only mounted then).
#[tracing::instrument(skip_all)]
pub async fn post_run(State(app): State<OperState>) -> Response {
    // Single-run guard
    if app.progress.read().unwrap().is_some() {
        return (StatusCode::CONFLICT, "install already running").into_response();
    }

    // Move the plan out (preserve secret ownership; do not clone).
    let plan = {
        let mut slot = app.plan.write().unwrap();
        match slot.take() {
            Some(p) => p,
            None => return (StatusCode::BAD_REQUEST, "no plan set").into_response(),
        }
    };

    // Determine resume point (after a crash/restart)
    let resume_from = match resume::load() {
        Ok(Some(r)) => r.last_step.as_deref().and_then(parse_step),
        _ => None,
    };

    // Fresh broadcast channel for this run
    let (tx, _rx) = tokio::sync::broadcast::channel(64);
    app.progress.write().unwrap().replace(tx.clone());

    // Seed/overwrite resume file with a start marker (no sensitive data)
    let started_at = time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "now".into());

    let start = resume::Resume {
        last_step: resume_from
            .map(|s| step_name(s).into())
            .or(Some("Start".into())),
        started_at,
        // lightweight fingerprint; avoids secrets
        plan_fingerprint: format!("{}|{}|{}", plan.site_name, plan.base_url, plan.timezone),
    };
    let _ = resume::save(&start);

    // Kick off steps (resuming if needed)
    let app_for_task = app.clone();
    tokio::spawn(async move {
        match run_all_from(plan, tx.clone(), resume_from).await {
            Ok(()) => {
                let _ = tx.send(Msg::Success("Install"));
                let _ = tx.send(Msg::Done);

                // 🔁 One-way, no-branch swap to the Serving router
                let _ = app_for_task
                    .phase
                    .transition_to(&app_for_task, Phase::Serve)
                    .await;
            }
            Err(e) => {
                let _ = tx.send(Msg::Fail("Install", format!("{e}")));
                let _ = tx.send(Msg::Done);
            }
        }

        // Allow future runs only if we ever reintroduce them (kept for symmetry)
        let _ = app_for_task.progress.write().unwrap().take();
    });

    StatusCode::NO_CONTENT.into_response()
}
