use askama::Template;
use askama_web::WebTemplate;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use infra::install::resume;

use crate::state::AppState;

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

pub async fn get_welcome(State(app): State<AppState>) -> Response {
    tracing::debug!("GET_WELCOME");
    if app.is_installed() {
        return Redirect::to("/").into_response();
    }

    // If a run is active or a resume file exists, continue the run
    if app.progress.read().unwrap().is_some() || resume::load().ok().flatten().is_some() {
        return Redirect::to("/install/run").into_response();
    }

    // Otherwise, start the wizard at the config step
    Redirect::to("/install/config").into_response()
}
pub async fn get_config() -> Response {
    tracing::debug!("GET_CONFIG");
    InstallConfig.into_response()
}
pub async fn get_run() -> Response {
    tracing::debug!("GET_RUN");
    InstallRun.into_response()
}
pub async fn get_done() -> Response {
    tracing::debug!("GET_DONE");
    InstallDone.into_response()
}

pub async fn get_home(State(app): State<AppState>) -> Response {
    tracing::debug!("GET_HOME");
    if app.is_installed() {
        // Serving mode – swap this out for your real homepage later.
        "WhisperCMS is running.".into_response()
    } else {
        // Installing – show maintenance page with 503.
        (StatusCode::SERVICE_UNAVAILABLE, Maint).into_response()
    }
}
