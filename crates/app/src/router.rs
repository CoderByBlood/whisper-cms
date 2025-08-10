use std::sync::Arc;
use axum::{Router, extract::State};
use axum::response::IntoResponse;
use axum::http::Request;
use axum::body::Body;

use crate::state::AppState;

/// A minimal router that forwards everything to the current phase handler.
/// NormalizePath is still applied in `main.rs` (as you had it working).
pub fn build(app_state: AppState) -> Router {
    Router::new()
        .fallback(dispatch)
        .with_state(Arc::new(app_state)) // axum State wants Arc for cheap clones
}

async fn dispatch(
    State(app): State<Arc<AppState>>,
    req: Request<Body>,
) -> impl IntoResponse {
    app.phase.dispatch((*app).clone(), req).await
}