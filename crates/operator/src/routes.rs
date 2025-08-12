use askama::Template;
 // keep for Redirect.into_response(), etc.
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse as _, Redirect, Response},
};

use infra::install::resume;
use crate::state::OperState;

// ---------- Templates ----------

#[derive(Template)]
#[template(path = "page/install_lang.html")]
struct InstallLang;

#[derive(Template)]
#[template(path = "page/install_db.html")]
struct InstallDb;

#[derive(Template)]
#[template(path = "page/install_site.html")]
struct InstallSite;

#[derive(Template)]
#[template(path = "page/install_run.html")]
struct InstallRun;

#[derive(Template)]
#[template(path = "page/install_done.html")]
struct InstallDone;

#[derive(Template)]
#[template(path = "page/maint.html")]
struct Maint;

// ---------- Handlers ----------

/// Welcome/entry:
/// - If an install is active or resumable, jump to /install/run
/// - Otherwise, start at /install/lang
#[tracing::instrument(skip_all)]
pub async fn get_welcome(State(app): State<OperState>) -> Response {
    let has_active = app.progress.read().unwrap().is_some();
    let has_resume = resume::load().ok().flatten().is_some();

    if has_active || has_resume {
        return Redirect::to("/install/run").into_response();
    }
    Redirect::to("/install/lang").into_response()
}

/// Maintenance page (503 so proxies/CDNs don’t cache as success).
#[tracing::instrument(skip_all)]
pub async fn get_maint() -> Response {
    let html = Maint.render().unwrap_or_else(|e| {
        format!("<h1>Maintenance</h1><p>Template error: {e}</p>")
    });
    let mut resp = Html(html).into_response();
    *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    resp
}

/// Step 1: language selection
#[tracing::instrument(skip_all)]
pub async fn get_lang(State(_): State<OperState>) -> Response {
    let html = InstallLang.render().unwrap_or_else(|e| {
        format!("<h1>Install · Language</h1><p>Template error: {e}</p>")
    });
    Html(html).into_response()
}

/// Step 2: database configuration
#[tracing::instrument(skip_all)]
pub async fn get_db(State(_): State<OperState>) -> Response {
    let html = InstallDb.render().unwrap_or_else(|e| {
        format!("<h1>Install · Database</h1><p>Template error: {e}</p>")
    });
    Html(html).into_response()
}

/// Step 3: site information (+ admin)
#[tracing::instrument(skip_all)]
pub async fn get_site(State(_): State<OperState>) -> Response {
    let html = InstallSite.render().unwrap_or_else(|e| {
        format!("<h1>Install · Site</h1><p>Template error: {e}</p>")
    });
    Html(html).into_response()
}

/// Run/observe the installation
#[tracing::instrument(skip_all)]
pub async fn get_run(State(_): State<OperState>) -> Response {
    let html = InstallRun.render().unwrap_or_else(|e| {
        format!("<h1>Install · Run</h1><p>Template error: {e}</p>")
    });
    Html(html).into_response()
}

#[tracing::instrument(skip_all)]
pub async fn get_done(State(_): State<OperState>) -> Response {
    let html = InstallDone.render().unwrap_or_else(|e| {
        format!("<h1>Install · Done</h1><p>Template error: {e}</p>")
    });
    Html(html).into_response()
}

#[tracing::instrument(skip_all)]
pub async fn get_home() -> Response {
    Html("WhisperCMS is running.".to_string()).into_response()
}