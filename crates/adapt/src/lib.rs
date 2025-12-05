pub mod js;
pub mod mql;
pub mod plugin;
pub mod runtime;

use serve::Error as ServeError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Serve error: {0}")]
    ServeError(#[from] ServeError),

    #[error("missing RequestContext in request extensions")]
    MissingContext,

    #[error("theme error: {0}")]
    Theme(String),

    #[error("JS engine error: {0}")]
    Engine(String),

    #[error("JS evaluation error: {0}")]
    Eval(String),

    #[error("JS function call error: {0}")]
    Call(String),

    #[error("conversion error: {0}")]
    Conversion(String),

    #[error("invalid filter: {0}")]
    InvalidFilter(String),

    #[error("invalid operator: {0}")]
    InvalidOperator(String),

    #[error("invalid sort spec: {0}")]
    InvalidSort(String),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Context bridging / (de)serialization issues when moving between
    /// Rust structs and JsValue/JSON.
    #[error("context bridge error: {0}")]
    ContextBridge(String),

    // ─────────────────────────────────────────────────────────────────────
    // Plugin-related errors
    // ─────────────────────────────────────────────────────────────────────
    /// Failure while loading / bootstrapping plugins at startup.
    #[error("plugin bootstrap error: {0}")]
    PluginBootstrap(String),

    /// Failure while executing plugin lifecycle hooks (`init`, `before`,
    /// `after`).
    #[error("plugin execution error: {0}")]
    PluginExecution(String),

    // ─────────────────────────────────────────────────────────────────────
    // Theme-related errors
    // ─────────────────────────────────────────────────────────────────────
    /// Failure while discovering, loading, or binding themes at startup.
    #[error("theme bootstrap error: {0}")]
    ThemeBootstrap(String),

    /// Failure while executing theme lifecycle hooks (`init`, `handle`,
    /// or any theme-specific entrypoints).
    #[error("theme execution error: {0}")]
    ThemeExecution(String),

    #[error("unknown error: {0}")]
    Other(String),
}

impl Error {
    // Small helpers if you want to use them ergonomically from other modules.

    #[inline]
    pub fn ctx_bridge(msg: impl Into<String>) -> Self {
        Error::ContextBridge(msg.into())
    }

    #[inline]
    pub fn plugin_bootstrap(msg: impl Into<String>) -> Self {
        Error::PluginBootstrap(msg.into())
    }

    #[inline]
    pub fn plugin_execution(msg: impl Into<String>) -> Self {
        Error::PluginExecution(msg.into())
    }

    #[inline]
    pub fn theme_bootstrap(msg: impl Into<String>) -> Self {
        Error::ThemeBootstrap(msg.into())
    }

    #[inline]
    pub fn theme_execution(msg: impl Into<String>) -> Self {
        Error::ThemeExecution(msg.into())
    }

    #[inline]
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }
}
