use crate::state::AppState;
use askama::Template;
use askama_web::WebTemplate;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};

use infra::install::resume;

#[derive(Template, WebTemplate)]
#[template(path = "page/install_config.html")]
struct InstallConfig;

#[derive(Template, WebTemplate)]
#[template(path = "page/install_run.html")]
struct InstallRun;

#[derive(Template, WebTemplate)]
#[template(path = "page/install_done.html")]
struct InstallDone;

#[derive(Template, WebTemplate)]
#[template(path = "page/maint.html")]
struct Maint;

/// We no longer render a welcome placeholder:
/// - If a run is resumable/active -> jump to /install/run
/// - Else -> start at /install/config
#[tracing::instrument(skip_all)]
pub async fn get_welcome(State(app): State<AppState>) -> Response {
    tracing::debug!("welcome");
    let has_active = app.progress.read().unwrap().is_some();
    let has_resume = resume::load().ok().flatten().is_some();

    if has_active || has_resume {
        return Redirect::to("/install/run").into_response();
    }
    Redirect::to("/install/config").into_response()
}

/// Maintenance page used during the Install phase.
/// Always returns 503 so proxies/CDNs don’t cache it as a success.
#[tracing::instrument(skip_all)]
pub async fn get_maint() -> Response {
    tracing::debug!("maintain");
    (StatusCode::SERVICE_UNAVAILABLE, Maint).into_response()
}

#[tracing::instrument(skip_all)]
pub async fn get_config(State(_): State<AppState>) -> Response {
    tracing::debug!("configure");
    InstallConfig.into_response()
}

#[tracing::instrument(skip_all)]
pub async fn get_run(State(_): State<AppState>) -> Response {
    tracing::debug!("run");
    InstallRun.into_response()
}

#[tracing::instrument(skip_all)]
pub async fn get_done(State(_): State<AppState>) -> Response {
    tracing::debug!("done");
    // This page is reachable only during the Install phase; once we transition to Serve,
    // the entire install router is unmounted (no extra guards here).
    InstallDone.into_response()
}

#[tracing::instrument(skip_all)]
pub async fn get_home() -> Response {
    tracing::debug!("home");
    "WhisperCMS is running.".into_response()
}
