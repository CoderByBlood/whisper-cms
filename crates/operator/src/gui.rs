use axum::body::Body;
use axum::http::Request;
use axum::{extract::State, middleware::from_fn_with_state, response::IntoResponse, Router};
use std::sync::Arc;

use crate::auth;
use crate::state::OperState;

#[tracing::instrument(skip_all)]
pub fn build(app: OperState) -> Router {
    Router::new()
        .fallback(dispatch)
        .with_state(Arc::new(app.clone()))
        // Auth runs for every request to the operator GUI
        .layer(from_fn_with_state(app, auth::gate))
}

#[tracing::instrument(skip_all)]
async fn dispatch(State(app): State<Arc<OperState>>, req: Request<Body>) -> impl IntoResponse {
    app.phase.dispatch((*app).clone(), req).await
}
