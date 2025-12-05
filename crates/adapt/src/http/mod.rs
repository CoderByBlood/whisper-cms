// crates/adapt/src/http/mod.rs

pub mod error;
pub mod plugin;

pub use error::HttpError;
pub use plugin::PluginMiddleware;
