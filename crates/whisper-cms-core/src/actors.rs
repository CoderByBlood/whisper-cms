pub mod config;
pub mod request;

//use ractor::MessagingErr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SceneError {
    #[error("Database error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("Could execute scene due to: {0}")]
    Transformation(String),
}
