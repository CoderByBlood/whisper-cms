use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("template error: {0}")]
    Template(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("handlebars error: {0}")]
    Handlebars(#[from] handlebars::RenderError),

    #[error("invalid regex `{pattern}`: {error}")]
    InvalidRegex { pattern: String, error: String },

    #[error("lol_html error: {0}")]
    LolHtml(String),

    #[error("serde_json error: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("json-patch error: {0}")]
    JsonPatch(#[from] json_patch::PatchError),

    #[error("JSON parse error after regex transformations: {0}")]
    JsonAfterRegex(String),
}

impl From<handlebars::TemplateError> for RenderError {
    fn from(e: handlebars::TemplateError) -> Self {
        RenderError::Template(e.to_string())
    }
}
