use axum::{routing::get, Router};
use tower_http::services::ServeDir;
use types::InstallState;

use crate::install::routes::{get_config, get_done, get_run, get_welcome};
use crate::install::actions::{post_config, post_run};
use crate::install::progress::sse_progress;
use crate::state::AppState;

pub fn build(state: InstallState) -> Router {
    let static_dir = ServeDir::new("crates/app/static");
    let app_state = AppState::default();

    let base = Router::new().nest_service("/static", static_dir);

    let app = match state {
        InstallState::Complete => {
            base.route("/", get(|| async { "WhisperCMS is running." }))
                .fallback(|| async { axum::http::StatusCode::NOT_FOUND })
        }
        _ => {
            base.route("/install", get(get_welcome))
                .route("/install/config", get(get_config).post(post_config))
                .route("/install/run", get(get_run).post(post_run))     // start + page
                .route("/install/progress", get(sse_progress))          // SSE stream
                .route("/install/done", get(get_done))
        }
    };

    app.with_state(app_state) // set state at the end
}