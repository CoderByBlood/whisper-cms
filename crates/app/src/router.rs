use axum::body::Body;
use axum::http::Request;
use axum::response::IntoResponse;
use axum::{extract::State, Router};
use std::sync::Arc;

use crate::state::AppState;

/// A minimal router that forwards everything to the current phase handler.
/// NormalizePath is still applied in `main.rs` (as you had it working).
#[tracing::instrument(skip_all)]
pub fn build(app_state: AppState) -> Router {
    Router::new()
        .fallback(dispatch)
        .with_state(Arc::new(app_state)) // axum State wants Arc for cheap clones
}

#[tracing::instrument(skip_all)]
async fn dispatch(State(app): State<Arc<AppState>>, req: Request<Body>) -> impl IntoResponse {
    app.phase.dispatch((*app).clone(), req).await
}
