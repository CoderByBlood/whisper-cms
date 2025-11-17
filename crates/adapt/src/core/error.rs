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
