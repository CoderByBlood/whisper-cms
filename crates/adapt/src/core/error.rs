use http::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid header value: {0}")]
    InvalidHeaderValue(String),

    #[error("json patch error: {0}")]
    JsonPatch(#[from] json_patch::PatchError),

    #[error("other core error: {0}")]
    Other(String),
}

impl CoreError {
    pub fn to_status(&self) -> StatusCode {
        match self {
            CoreError::InvalidHeaderValue(_) => StatusCode::BAD_REQUEST,
            CoreError::JsonPatch(_) => StatusCode::INTERNAL_SERVER_ERROR,
            CoreError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
