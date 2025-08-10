use askama::Template;
use askama_web::WebTemplate;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};

use crate::state::AppState;
use infra::install::resume;

/// We no longer render a welcome placeholder:
/// - If a run is resumable/active -> jump to /install/run
/// - Else -> start at /install/config
pub async fn get_welcome(State(app): State<AppState>) -> Response {
    let has_active = app.progress.read().unwrap().is_some();
    let has_resume = resume::load().ok().flatten().is_some();

    if has_active || has_resume {
        return Redirect::to("/install/run").into_response();
    }
    Redirect::to("/install/config").into_response()
}

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

/// Maintenance page used during the Install phase.
/// Always returns 503 so proxies/CDNs don’t cache it as a success.
pub async fn get_maint() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, Maint).into_response()
}

pub async fn get_config(State(_): State<AppState>) -> Response {
    InstallConfig.into_response()
}

pub async fn get_run(State(_): State<AppState>) -> Response {
    InstallRun.into_response()
}

pub async fn get_done(State(_): State<AppState>) -> Response {
    // This page is reachable only during the Install phase; once we transition to Serve,
    // the entire install router is unmounted (no extra guards here).
    InstallDone.into_response()
}

pub async fn get_home() -> Response {
    "WhisperCMS is running.".into_response()
}
