use crate::state::AppState;
use askama::Template;
use askama_web::WebTemplate;
use axum::{
    body::Body,
    http::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

#[derive(Template, WebTemplate)]
#[template(path = "page/maint.html")]
struct Maint;

/// If not installed, only allow /install/** and /static/**. Everything else => maintenance (503).
pub async fn gate(
    axum::extract::State(app): axum::extract::State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if app.is_installed() {
        return next.run(req).await;
    }
    let path = req.uri().path();
    tracing::debug!("MAINT Gate - gate path={}", path);
    if path.starts_with("/install") || path.starts_with("/static") {
        let a = next.run(req).await;
        tracing::debug!("MAINT Gate - Passing it onto={:?}", a);
        return a;//next.run(req).await;
    }
    tracing::debug!("MAINT Gate - Erroring={}", path);
    (StatusCode::SERVICE_UNAVAILABLE, Maint).into_response()
}
