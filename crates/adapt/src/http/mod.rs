// crates/adapt/src/http/mod.rs

pub mod app;
pub mod error;
pub mod middleware;
pub mod plugin;
pub mod theme;

pub use error::HttpError;
pub use middleware::{PluginLayer, PluginMiddleware};
pub use theme::theme_entrypoint;
