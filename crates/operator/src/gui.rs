use std::sync::Arc;
use axum::{Router, extract::State, response::IntoResponse};
use axum::body::Body;
use axum::http::Request;

use crate::state::OperState;

#[tracing::instrument(skip_all)]
pub fn build(app: OperState) -> Router {
    Router::new()
        .fallback(dispatch)
        .with_state(Arc::new(app))
}

#[tracing::instrument(skip_all)]
async fn dispatch(
    State(app): State<Arc<OperState>>,
    req: Request<Body>,
) -> impl IntoResponse {
    app.phase.dispatch((*app).clone(), req).await
}