use crate::http::plugin_middleware::PluginLayer;
use crate::http::resolver::SimpleContentResolver;
use crate::runtime::bootstrap::{PluginConfig, RuntimeHandles, ThemeConfig};
use crate::runtime::{PluginRuntimeClient, ThemeRuntimeClient};

use axum::{routing::get, Router};
use std::path::PathBuf;

#[derive(Clone)]
pub struct AppState {
    pub content_root: PathBuf,
    pub resolver: SimpleContentResolver,
    pub plugin_rt: PluginRuntimeClient,
    pub theme_rt: ThemeRuntimeClient,
    pub plugin_configs: Vec<PluginConfig>,
    pub theme_configs: Vec<ThemeConfig>,
}

pub fn build_app(
    content_root: PathBuf,
    resolver: SimpleContentResolver,
    handles: RuntimeHandles,
) -> Router {
    let state = AppState {
        content_root,
        resolver,
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
