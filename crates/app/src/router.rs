use axum::{middleware::from_fn_with_state, routing::get, Router};
use tower_http::services::ServeDir;

use crate::install::actions::{post_config, post_run};
use crate::install::progress::sse_progress;
use crate::install::routes::{get_config, get_done, get_home, get_run, get_welcome};
use crate::middleware::maint::gate;
use crate::state::AppState;

pub fn build(app_state: AppState) -> Router {
    Router::new()
        .nest_service("/static", ServeDir::new("crates/app/static"))
        .route("/", get(get_home))
        .route("/install", get(get_welcome).post(post_run))
        .route("/install/config", get(get_config).post(post_config))
        .route("/install/run", get(get_run).post(post_run))
        .route("/install/progress", get(sse_progress))
        .route("/install/done", get(get_done))
        .fallback(get(get_home))
        // maintenance gate stays here; it’s fine if it runs *after* normalization
        .layer(from_fn_with_state(app_state.clone(), gate))
        .with_state(app_state)
}