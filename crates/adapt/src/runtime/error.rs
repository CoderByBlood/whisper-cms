use crate::js::JsError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("JS engine error: {0}")]
    Js(#[from] JsError),

    #[error("invalid plugin ctx shape: {0}")]
    InvalidCtx(String),

    #[error("plugin error: {0}")]
    Plugin(String),

    #[error("other runtime error: {0}")]
    Other(String),
}
