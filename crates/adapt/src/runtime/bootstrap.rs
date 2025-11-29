// crates/adapt/src/runtime/bootstrap.rs

use crate::js::engine::BoaEngine;
use crate::js::JsEngine;
use crate::runtime::error::RuntimeError;
use crate::runtime::plugin::{PluginRuntime, PluginSpec};
use crate::runtime::plugin_actor::PluginRuntimeClient;
use crate::runtime::theme::{ThemeRuntime, ThemeSpec};
use crate::runtime::theme_actor::ThemeRuntimeClient;
use serve::render::http::{RequestContext, ResponseBodySpec};

/// Configuration for plugins.
///
/// This is the host-side configuration that you can build from disk/TOML/etc.
/// It is intentionally close to `PluginSpec` so conversion is trivial.
#[derive(Clone, Debug)]
pub struct PluginConfig {
    /// Host-facing plugin identifier (e.g., folder name, slug, etc.).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// JavaScript source of the plugin (already loaded from disk).
    pub source: String,
}

impl From<&PluginConfig> for PluginSpec {
    fn from(cfg: &PluginConfig) -> Self {
        PluginSpec {
            id: cfg.id.clone(),
            name: cfg.name.clone(),
            source: cfg.source.clone(),
        }
    }
}

impl From<&PluginSpec> for PluginConfig {
    fn from(spec: &PluginSpec) -> Self {
        PluginConfig {
            id: spec.id.to_owned(),
            name: spec.name.to_owned(),
            source: spec.source.to_owned(),
        }
    }
}

/// Configuration for themes.
///
/// Likewise, this is the host-side config that you build from manifests or
/// TOML files and then feed into the JS runtime bootstrap.
#[derive(Clone, Debug)]
pub struct ThemeConfig {
    /// Host-facing theme identifier (e.g., folder name, slug).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Human-readable display name.
    pub mount_path: String,
    /// JavaScript source of the theme (already loaded from disk).
    pub source: String,
}

impl From<&ThemeConfig> for ThemeSpec {
    fn from(cfg: &ThemeConfig) -> Self {
        ThemeSpec::new(&cfg.id, &cfg.name, &cfg.mount_path, &cfg.source)
    }
}

impl From<&ThemeSpec> for ThemeConfig {
    fn from(spec: &ThemeSpec) -> Self {
        ThemeConfig {
            id: spec.id.to_owned(),
            name: spec.name.to_owned(),
            mount_path: spec.mount_path.to_owned(),
            source: spec.source.to_owned(),
        }
    }
}

/// A fully bootstrapped set of runtime handles for HTTP layer.
///
/// This is what your edge / HTTP layer should depend on.
#[derive(Clone)]
pub struct RuntimeHandles {
    pub theme_client: ThemeRuntimeClient,
    pub plugin_client: PluginRuntimeClient,
    pub plugin_configs: Vec<PluginConfig>,
    pub theme_configs: Vec<ThemeConfig>,
}

/// A single bound theme: host id + JS runtime for that theme.
pub struct BoundTheme<E: JsEngine> {
    id: String,
    runtime: ThemeRuntime<E>,
}

impl<E: JsEngine> BoundTheme<E> {
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl BoundTheme<BoaEngine> {
    /// Initialize this theme with a context (optional boot hook).
    pub fn init(&mut self, ctx: &RequestContext) -> Result<(), RuntimeError> {
        self.runtime.init(ctx)
    }

    /// Render a response for this theme.
    ///
    /// The typical flow is:
    /// 1. Convert `RequestContext` to a JS object (via ctx bridge in ThemeRuntime).
    /// 2. Call `<themeId>.handle(ctx)` in JS.
    /// 3. Merge the JS-returned ctx back into the Rust `RequestContext`.
    /// 4. Extract a `ResponseBodySpec` from the final ctx.
    pub fn render(&mut self, mut ctx: RequestContext) -> Result<ResponseBodySpec, RuntimeError> {
        // Delegate the heavy lifting to the ThemeRuntime.
        self.runtime.handle(&mut ctx)?;
        Ok(ctx.into_response_body_spec())
    }
}

/// Build all runtimes (plugins + themes) and wrap them in actor clients.
///
/// Call this once at process-start and pass the returned `RuntimeHandles`
/// down into your Axum / HTTP bootstrap.
pub fn bootstrap_all(
    plugin_cfgs: Vec<PluginConfig>,
    theme_cfgs: Vec<ThemeConfig>,
) -> Result<RuntimeHandles, RuntimeError> {
    // ─────────────────────────────────────────────────────────────────────
    // 1. Build plugin runtime: one Boa engine shared across all plugins.
    // ─────────────────────────────────────────────────────────────────────
    let engine = BoaEngine::new();
    let mut plugin_rt = PluginRuntime::new(engine)?;

    let plugin_specs: Vec<PluginSpec> = plugin_cfgs.iter().map(PluginSpec::from).collect();
    plugin_rt.load_plugins(&plugin_specs)?;

    // Wrap the plugin runtime in its single-threaded actor.
    let plugin_client = PluginRuntimeClient::spawn(plugin_rt);

    // ─────────────────────────────────────────────────────────────────────
    // 2. Build all theme runtimes: one Boa engine per theme.
    // ─────────────────────────────────────────────────────────────────────
    let bound_themes = load_themes(&theme_cfgs)?;

    // Wrap all themes in a single-threaded actor.
    let theme_client = ThemeRuntimeClient::spawn(bound_themes);

    // ─────────────────────────────────────────────────────────────────────
    // 3. Return handles to the HTTP / edge layer.
    // ─────────────────────────────────────────────────────────────────────
    Ok(RuntimeHandles {
        theme_client,
        plugin_client,
        plugin_configs: plugin_cfgs,
        theme_configs: theme_cfgs,
    })
}

/// Load and bind all themes into `BoundTheme<BoaEngine>` values.
///
/// Each theme gets its own `BoaEngine` and `ThemeRuntime`. This keeps the
/// Boa requirement of "single-threaded" satisfied while still letting the
/// outer Axum server be multi-threaded (via the actor indirection).
fn load_themes(theme_cfgs: &[ThemeConfig]) -> Result<Vec<BoundTheme<BoaEngine>>, RuntimeError> {
    let mut themes = Vec::with_capacity(theme_cfgs.len());

    for cfg in theme_cfgs {
        // Fresh JS engine per theme.
        let engine = BoaEngine::new();

        // Convert config → spec for the runtime.
        let spec: ThemeSpec = ThemeSpec::from(cfg);

        // Create a ThemeRuntime for this theme (loads and evaluates JS).
        let runtime = ThemeRuntime::new(engine, spec)?;

        themes.push(BoundTheme {
            id: cfg.id.clone(),
            runtime,
        });
    }

    Ok(themes)
}
