// crates/adapt/src/http/app.rs

use crate::http::PluginMiddleware;
use crate::runtime::bootstrap::{PluginConfig, RuntimeHandles, ThemeConfig};
use crate::runtime::{PluginRuntimeClient, ThemeRuntimeClient};

use actix_web::{dev::HttpServiceFactory, web};
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
pub fn build_app(content_root: PathBuf, handles: RuntimeHandles) -> impl HttpServiceFactory {
    use crate::http::theme::theme_entrypoint;

    // Extract / clone what we need from `handles` up front.
    let plugin_client = handles.plugin_client.clone();
    let theme_client = handles.theme_client.clone();
    let plugin_configs = handles.plugin_configs.clone();
    let theme_configs = handles.theme_configs.clone();

    // Ordered list of plugin IDs for before/after hooks.
    let plugin_ids: Vec<String> = plugin_configs.iter().map(|cfg| cfg.id.clone()).collect();

    let state = AppState {
        content_root,
        plugin_rt: plugin_client.clone(),
        theme_rt: theme_client,
        plugin_configs,
        theme_configs,
    };

    // Scope that can be mounted at app root or elsewhere.
    web::scope("/")
        .app_data(web::Data::new(state))
        // Single middleware that drives JS plugins via PluginRuntimeClient.
        .wrap(PluginMiddleware::new(plugin_client, plugin_ids))
        // Handle the bare "/" path.
        .route("/", web::to(theme_entrypoint))
        // Catch-all: handle *all* methods and paths under this scope.
        .route("/{tail:.*}", web::to(theme_entrypoint))
}
