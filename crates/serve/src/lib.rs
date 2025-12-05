pub mod indexer;
pub mod render;
pub mod resolver;

use http::StatusCode;
use std::{io, string::FromUtf8Error};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("front matter parse error: {0}")]
    FrontMatter(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("front-matter index error: {0}")]
    FrontMatterIndex(String),

    #[error("content index error: {0}")]
    ContentIndex(String),

    #[error("AsciiDoc conversion error: {0}")]
    AsciiDoc(String),

    #[error("reStructuredText conversion error: {0}")]
    ReStructuredText(String),

    #[error("Org-mode conversion error: {0}")]
    Org(String),

    #[error("Edge scan error: {0}")]
    Scan(String),

    #[error("template error: {0}")]
    Template(String),

    #[error("handlebars error: {0}")]
    Handlebars(#[from] handlebars::RenderError),

    #[error("invalid regex `{pattern}`: {error}")]
    InvalidRegex { pattern: String, error: String },

    #[error("lol_html error: {0}")]
    LolHtml(String),

    #[error("invalid header value: {0}")]
    InvalidHeaderValue(String),

    #[error("json-patch error: {0}")]
    JsonPatch(#[from] json_patch::PatchError),

    #[error("JSON parse error after regex transformations: {0}")]
    JsonAfterRegex(String),

    #[error("FromUTF8 error: {0}")]
    FromUTF8(#[from] FromUtf8Error),

    #[error("Backend error: {0}")]
    Backend(String),

    #[error("other core error: {0}")]
    Other(String),
}

impl From<handlebars::TemplateError> for Error {
    fn from(e: handlebars::TemplateError) -> Self {
        Error::Template(e.to_string())
    }
}

impl Error {
    pub fn to_status(&self) -> StatusCode {
        match self {
            Error::InvalidHeaderValue(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}
