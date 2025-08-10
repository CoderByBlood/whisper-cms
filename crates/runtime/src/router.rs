use axum::{routing::get, Router};
use tower_http::services::ServeDir;

use crate::home::get_home;
use crate::state::RunState;

#[tracing::instrument(skip_all)]
pub fn build(state: RunState) -> Router {
    Router::new()
        .nest_service("/static", ServeDir::new("static")) // served by site repo
        .route("/", get(get_home))
        .fallback(get(get_home)) // or 404 handler
        .with_state(state)
}