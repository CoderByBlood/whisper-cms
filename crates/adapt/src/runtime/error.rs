// crates/adapt/src/runtime/error.rs

use crate::js::JsError;
use thiserror::Error;

/// Errors that can occur in the JS runtime / plugin / theme layer.
///
/// This is intentionally fairly high-level so callers in the edge layer
/// don't need to know about lower-level details (Boa internals, etc.).
#[derive(Debug, Error)]
pub enum RuntimeError {
    // ─────────────────────────────────────────────────────────────────────
    // JS / engine / bridge errors
    // ─────────────────────────────────────────────────────────────────────
    /// Any error coming from the JS engine abstraction (Boa in your case).
    #[error("JavaScript engine error: {0}")]
    Js(#[from] JsError),

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

    // ─────────────────────────────────────────────────────────────────────
    // Catch-all
    // ─────────────────────────────────────────────────────────────────────
    /// A generic runtime error when nothing more specific fits.
    #[error("runtime error: {0}")]
    Other(String),
}

impl RuntimeError {
    // Small helpers if you want to use them ergonomically from other modules.

    #[inline]
    pub fn ctx_bridge(msg: impl Into<String>) -> Self {
        RuntimeError::ContextBridge(msg.into())
    }

    #[inline]
    pub fn plugin_bootstrap(msg: impl Into<String>) -> Self {
        RuntimeError::PluginBootstrap(msg.into())
    }

    #[inline]
    pub fn plugin_execution(msg: impl Into<String>) -> Self {
        RuntimeError::PluginExecution(msg.into())
    }

    #[inline]
    pub fn theme_bootstrap(msg: impl Into<String>) -> Self {
        RuntimeError::ThemeBootstrap(msg.into())
    }

    #[inline]
    pub fn theme_execution(msg: impl Into<String>) -> Self {
        RuntimeError::ThemeExecution(msg.into())
    }

    #[inline]
    pub fn other(msg: impl Into<String>) -> Self {
        RuntimeError::Other(msg.into())
    }
}
