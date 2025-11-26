// crates/adapt/src/http/app.rs

use crate::http::middleware::PluginLayer;
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
    // Collect the configured plugin IDs in the order they were discovered.
    let plugin_ids: Vec<String> = handles
        .plugin_configs
        .iter()
        .map(|cfg| cfg.id.clone())
        .collect();

    let state = AppState {
        content_root,
        plugin_rt: handles.plugin_client.clone(),
        theme_rt: handles.theme_client,
        plugin_configs: handles.plugin_configs,
        theme_configs: handles.theme_configs,
    };

    Router::new()
        .route("/*path", get(crate::http::theme::theme_entrypoint))
        .with_state(state.clone())
        .layer(PluginLayer::new(state.plugin_rt.clone(), plugin_ids))
}
