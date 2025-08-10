use axum::{
    extract::{Form, State},
    response::{IntoResponse, Redirect, Response},
};
use crate::state::AppState;
use super::plan::InstallForm;
use std::sync::RwLockWriteGuard;

use infra::install::resume;
use types::InstallStep;

pub async fn post_config(
    State(app): State<AppState>,
    Form(form): Form<InstallForm>,
) -> Response {
    match form.validate_into_plan() {
        Ok(plan) => {
            let mut slot: RwLockWriteGuard<'_, Option<_>> = app.plan.write().unwrap();
            *slot = Some(plan);
            Redirect::to("/install/run").into_response()
        }
        Err(errs) => {
            let body = format!(
                "Configuration errors:\n{}",
                errs.into_iter().map(|e| format!("- {e}")).collect::<Vec<_>>().join("\n")
            );
            (axum::http::StatusCode::BAD_REQUEST, body).into_response()
        }
    }
}

pub async fn post_run(State(app): State<AppState>) -> Response {
    // Optional: already installed? Nothing to run.
    if app.is_installed() {
        return (axum::http::StatusCode::GONE, "already installed").into_response();
    }

    // Run guard: only one install at a time.
    if app.progress.read().unwrap().is_some() {
        return (axum::http::StatusCode::CONFLICT, "install already running").into_response();
    }

    // Get the plan (move it out; don't clone secrets)
    let plan = {
        let mut slot = app.plan.write().unwrap();
        match slot.take() {
            Some(p) => p,
            None => return (axum::http::StatusCode::BAD_REQUEST, "no plan set").into_response(),
        }
    };

    // Determine resume point (after a crash/restart)
    let resume_from = match resume::load() {
        Ok(Some(r)) => r.last_step.as_deref().and_then(parse_step),
        _ => None,
    };

    // Fresh broadcast channel for this run
    let (tx, _rx) = tokio::sync::broadcast::channel(64);
    {
        let mut cell = app.progress.write().unwrap();
        *cell = Some(tx.clone());
    }

    // Seed/overwrite resume file with a start marker (no sensitive data)
    let start = resume::Resume {
        last_step: resume_from.map(step_name_str).or(Some("Start".into())),
        started_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "now".into()),
        plan_fingerprint: format!("{}|{}|{}", plan.site_name, plan.base_url, plan.timezone),
    };
    let _ = resume::save(&start);

    // Spawn the run; capture app so we can flip to Serving on success
    let app_for_task = app.clone();
    tokio::spawn(async move {
        match crate::install::steps::run_all_from(plan, tx.clone(), resume_from).await {
            Ok(()) => {
                // Flip runtime state to Serving (meets acceptance #3)
                app_for_task.set_installed(true);

                // Signal SSE clients
                let _ = tx.send(crate::install::progress::Msg::Success("Install"));
                let _ = tx.send(crate::install::progress::Msg::Done);
            }
            Err(e) => {
                let _ = tx.send(crate::install::progress::Msg::Fail("Install", format!("{e}")));
                let _ = tx.send(crate::install::progress::Msg::Done);
            }
        }
        // Allow future runs: drop the sender from shared state
        let _ = app_for_task.progress.write().unwrap().take();
    });

    axum::http::StatusCode::NO_CONTENT.into_response()
}

fn parse_step(s: &str) -> Option<InstallStep> {
    use InstallStep::*;
    match s {
        "GenerateSecrets"   => Some(GenerateSecrets),
        "WriteCoreConfigs"  => Some(WriteCoreConfigs),
        "MigrateDb"         => Some(MigrateDb),
        "SeedBaseline"      => Some(SeedBaseline),
        "WriteAdminConfig"  => Some(WriteAdminConfig),
        "FlipInstalledTrue" => Some(FlipInstalledTrue),
        _ => None,
    }
}
fn step_name_str(s: InstallStep) -> String {
    match s {
        InstallStep::GenerateSecrets   => "GenerateSecrets",
        InstallStep::WriteCoreConfigs  => "WriteCoreConfigs",
        InstallStep::MigrateDb         => "MigrateDb",
        InstallStep::SeedBaseline      => "SeedBaseline",
        InstallStep::WriteAdminConfig  => "WriteAdminConfig",
        InstallStep::FlipInstalledTrue => "FlipInstalledTrue",
    }.to_string()
}