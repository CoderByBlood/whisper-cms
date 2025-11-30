// crates/adapt/src/http/mod.rs

pub mod app;
pub mod error;
pub mod plugin;
pub mod theme;

pub use error::HttpError;
pub use plugin::PluginMiddleware;
pub use theme::theme_entrypoint;
