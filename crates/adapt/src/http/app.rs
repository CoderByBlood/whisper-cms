// crates/adapt/src/http/app.rs

use crate::http::plugin_middleware::PluginLayer;
use crate::runtime::bootstrap::{PluginConfig, RuntimeHandles, ThemeConfig};
use crate::runtime::{PluginRuntimeClient, ThemeRuntimeClient};

use axum::{routing::get, Router};
use std::path::PathBuf;

#[derive(Clone)]
pub struct AppState {
    pub content_root: PathBuf,
    pub plugin_rt: PluginRuntimeClient,
    pub theme_rt: ThemeRuntimeClient,
    pub plugin_configs: Vec<PluginConfig>,
    pub theme_configs: Vec<ThemeConfig>,
}

#[tracing::instrument(skip_all)]
pub fn build_app(content_root: PathBuf, handles: RuntimeHandles) -> Router {
    let state = AppState {
        content_root,
        plugin_rt: handles.plugin_client.clone(),
        theme_rt: handles.theme_client,
        plugin_configs: handles.plugin_configs,
        theme_configs: handles.theme_configs,
    };

    Router::new()
        .route("/*path", get(crate::http::theme::theme_entrypoint))
        .with_state(state)
        .layer(PluginLayer::new(handles.plugin_client))
}
