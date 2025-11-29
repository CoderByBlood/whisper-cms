use thiserror::Error;

#[derive(Debug, Error)]
pub enum JsError {
    #[error("JS engine error: {0}")]
    Engine(String),

    #[error("JS evaluation error: {0}")]
    Eval(String),

    #[error("JS function call error: {0}")]
    Call(String),

    #[error("conversion error: {0}")]
    Conversion(String),
}
