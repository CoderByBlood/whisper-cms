// crates/adapt/src/http/mod.rs

pub mod app;
pub mod error;
pub mod plugin;
pub mod plugin_middleware;
pub mod resolver;
pub mod theme;

pub use error::HttpError;
pub use plugin_middleware::{PluginLayer, PluginMiddleware};
pub use resolver::{
    build_request_context, ContentResolver, ResolvedContent, SimpleContentResolver,
};
pub use theme::theme_entrypoint;
