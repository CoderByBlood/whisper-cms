use thiserror::Error;

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("invalid filter: {0}")]
    InvalidFilter(String),

    #[error("invalid operator: {0}")]
    InvalidOperator(String),

    #[error("invalid sort spec: {0}")]
    InvalidSort(String),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("other query error: {0}")]
    Other(String),
}
