use axum::{
    extract::{Form, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use time::format_description::well_known::Rfc3339;
use url::Url;

use crate::{
    forms::{DbForm, LangForm, SiteForm},
    phase::Phase,
    progress::Msg,
    state::OperState,
    steps::{parse_step, run_all_from, step_name},
};
use infra::{
    config::paths::{with_paths, Paths},
    install::resume,
};
use secrecy::SecretString;
use types::{DbKind, InstallPlan};

/// Step 1: language selection
#[tracing::instrument(skip_all)]
pub async fn post_lang(State(app): State<OperState>, Form(form): Form<LangForm>) -> Response {
    {
        let mut slot = app.plan.write().unwrap();
        let plan = slot.get_or_insert_with(default_plan);
        plan.language = form.language;
    }
    Redirect::to("/install/db").into_response()
}

/// Step 2: database config
#[tracing::instrument(skip_all)]
pub async fn post_db(State(app): State<OperState>, Form(f): Form<DbForm>) -> Response {
    // Build/validate DB URLs and tokens into the staged plan.
    let mut errs = Vec::new();

    // Prepare values based on selection
    let db_kind = match f.db_kind.as_str() {
        "remote" => DbKind::Remote,
        _ => DbKind::Embedded,
    };

    // Resolve ops/content URLs
    let (ops_url, content_url) = match db_kind {
        DbKind::Embedded => {
            // Defaults if not provided
            let ops_path = f.ops_path.unwrap_or_else(|| "data/ops.db".to_string());
            let content_path = f
                .content_path
                .unwrap_or_else(|| "data/content.db".to_string());

            // Always store full URLs
            let ops = Url::parse(&format!("sqlite://{}", ops_path))
                .map_err(|e| errs.push(format!("invalid ops path: {e}")))
                .ok();
            let content = if f.split_content {
                Url::parse(&format!("sqlite://{}", content_path))
                    .map_err(|e| errs.push(format!("invalid content path: {e}")))
                    .ok()
            } else {
                ops.clone()
            };
            (ops, content)
        }
        DbKind::Remote => {
            let ops = f
                .ops_url
                .as_deref()
                .ok_or_else(|| {
                    errs.push("ops_url is required for remote DB".into());
                })
                .and_then(|s| Url::parse(s).map_err(|e| errs.push(format!("ops_url: {e}"))))
                .ok();
            let content = if f.split_content {
                f.content_url
                    .as_deref()
                    .ok_or_else(|| {
                        errs.push("content_url is required when split_content=true".into());
                    })
                    .and_then(|s| Url::parse(s).map_err(|e| errs.push(format!("content_url: {e}"))))
                    .ok()
            } else {
                ops.clone()
            };
            (ops, content)
        }
    };

    if !errs.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            format!("Configuration errors:\n{}", errs.join("\n")),
        )
            .into_response();
    }

    {
        let mut slot = app.plan.write().unwrap();
        let plan = slot.get_or_insert_with(default_plan);
        plan.db_kind = db_kind;
        plan.split_content = f.split_content;
        plan.db_ops_url = ops_url.unwrap();
        plan.db_content_url = content_url.unwrap();

        // Tokens (only kept in-memory; written later to secrets/ by steps)
        plan.db_ops_token = f
            .ops_token
            .filter(|s| !s.is_empty())
            .map(SecretString::from);
        plan.db_content_token = f
            .content_token
            .filter(|s| !s.is_empty())
            .map(SecretString::from);
    }

    Redirect::to("/install/site").into_response()
}

/// Step 3: site info (+ admin)
#[tracing::instrument(skip_all)]
pub async fn post_site(State(app): State<OperState>, Form(f): Form<SiteForm>) -> Response {
    {
        let mut slot = app.plan.write().unwrap();
        let plan = slot.get_or_insert_with(default_plan);

        // Populate fields
        plan.site_name = f.site_name;
        match Url::parse(&f.base_url) {
            Ok(u) => plan.base_url = u,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("invalid base_url: {e}")).into_response()
            }
        }
        plan.timezone = f.timezone;

        // Admin password in-memory only (adjust if your plan uses SecretBox<str>)
        plan.admin_password = Some(SecretString::from(f.admin_password));

        // Validate the plan now that all fields exist
        if let Err(e) = domain::validate::install::validate_plan(plan) {
            return (
                StatusCode::BAD_REQUEST,
                format!("Configuration errors:\n- {e}"),
            )
                .into_response();
        }
    }

    Redirect::to("/install/run").into_response()
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

    // Fail loudly if we can’t write config/install.json
    if let Err(e) = resume::save(&start) {
        tracing::error!("failed to seed resume file: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "resume init failed").into_response();
    }

    // Kick off steps (resuming if needed)
    let app_for_task = app.clone();
    // capture the site root for this run
    let paths = Paths::new(app.site_dir().to_path_buf());
    tokio::spawn(with_paths(paths, async move {
        match run_all_from(plan, tx.clone(), resume_from).await {
            Ok(()) => {
                let _ = tx.send(Msg::Success("Install"));
                let _ = tx.send(Msg::Done);

                // One-way swap to Serving router
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
        let _ = app_for_task.progress.write().unwrap().take();
    }));

    StatusCode::NO_CONTENT.into_response()
}

// ------------- helpers -------------

fn default_plan() -> InstallPlan {
    InstallPlan {
        language: "en-US".into(),
        db_kind: DbKind::Embedded,
        split_content: true,
        db_ops_url: Url::parse("sqlite://data/ops.db").unwrap(),
        db_content_url: Url::parse("sqlite://data/content.db").unwrap(),
        db_ops_token: None,
        db_content_token: None,
        site_name: String::new(),
        base_url: Url::parse("http://localhost").unwrap(),
        timezone: "UTC".into(),
        admin_password: None,
    }
}
