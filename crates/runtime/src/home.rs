use axum::response::{IntoResponse, Response};

#[tracing::instrument(skip_all)]
pub async fn get_home() -> Response {
    "WhisperCMS runtime serving.".into_response()
}