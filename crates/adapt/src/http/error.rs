use serve::resolver::ResolverError;
use thiserror::Error;

use serve::ctx::http::ContextError;
use serve::render::error::RenderError;

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("core error: {0}")]
    Context(#[from] ContextError),

    #[error("render error: {0}")]
    Render(#[from] RenderError),

    #[error("resolver error: {0}")]
    Resolver(#[from] ResolverError),

    #[error("missing RequestContext in request extensions")]
    MissingContext,

    #[error("theme error: {0}")]
    Theme(String),

    #[error("unknown error: {0}")]
    Other(String),
}
