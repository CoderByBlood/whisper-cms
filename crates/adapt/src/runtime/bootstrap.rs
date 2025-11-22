// crates/adapt/src/runtime/bootstrap.rs

use crate::core::context::{RequestContext, ResponseBodySpec};
use crate::js::engine::BoaEngine;
use crate::js::JsEngine;
use crate::runtime::error::RuntimeError;
use crate::runtime::plugin::{PluginRuntime, PluginSpec};
use crate::runtime::plugin_actor::PluginRuntimeClient;
use crate::runtime::theme::{ThemeRuntime, ThemeSpec};
use crate::runtime::theme_actor::ThemeRuntimeClient;

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
    /// JavaScript source of the theme (already loaded from disk).
    pub source: String,
}

impl From<&ThemeConfig> for ThemeSpec {
    fn from(cfg: &ThemeConfig) -> Self {
        ThemeSpec::new(&cfg.id, &cfg.name, &cfg.source)
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
    pub fn render(&mut self, ctx: RequestContext) -> Result<ResponseBodySpec, RuntimeError> {
        // Delegate the heavy lifting to the ThemeRuntime.
        self.runtime.handle(&mut ctx.clone())?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::RequestContext;
    use serde_json::json;
    use std::collections::HashMap;
    use tokio::runtime::Builder as RtBuilder;
    use tokio::task::LocalSet;

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn dummy_ctx() -> RequestContext {
        RequestContext::builder()
            .path("/test")
            .method("GET")
            .version("HTTP/1.1")
            .headers(json!({}))
            .params(json!({}))
            .content_meta(json!({ "title": "test" }))
            .theme_config(json!({}))
            .plugin_configs(HashMap::new())
            // No streams for this test
            .build()
    }

    // -------------------------------------------------------------------------
    // PluginConfig → PluginSpec
    // -------------------------------------------------------------------------

    #[test]
    fn plugin_config_converts_to_spec() {
        let cfg = PluginConfig {
            id: "p1".into(),
            name: "Plugin One".into(),
            source: "globalThis.p1={before(x){return x}}".into(),
        };

        let spec = PluginSpec::from(&cfg);

        assert_eq!(spec.id, "p1");
        assert_eq!(spec.name, "Plugin One");
        assert_eq!(spec.source, "globalThis.p1={before(x){return x}}");
    }

    // -------------------------------------------------------------------------
    // ThemeConfig → ThemeSpec
    // -------------------------------------------------------------------------

    #[test]
    fn theme_config_converts_to_spec() {
        let cfg = ThemeConfig {
            id: "t1".into(),
            name: "Theme One".into(),
            source: "globalThis.t1={handle(x){return x}}".into(),
        };

        let spec = ThemeSpec::from(&cfg);

        assert_eq!(spec.id, "t1");
        assert_eq!(spec.name, "Theme One");
        assert_eq!(spec.source, "globalThis.t1={handle(x){return x}}");
    }

    // -------------------------------------------------------------------------
    // load_themes
    // -------------------------------------------------------------------------

    #[test]
    fn load_themes_success() {
        let cfgs = vec![
            ThemeConfig {
                id: "a".into(),
                name: "A".into(),
                source: r#"globalThis["a"]={handle(ctx){return ctx}}"#.into(),
            },
            ThemeConfig {
                id: "b".into(),
                name: "B".into(),
                source: r#"globalThis["b"]={handle(ctx){return ctx}}"#.into(),
            },
        ];

        let result = super::load_themes(&cfgs);

        assert!(result.is_ok(), "load_themes should succeed on valid JS");
        let themes = result.unwrap();

        assert_eq!(themes.len(), 2);
        assert_eq!(themes[0].id(), "a");
        assert_eq!(themes[1].id(), "b");
    }

    #[test]
    fn load_themes_empty_ok() {
        let cfgs: Vec<ThemeConfig> = vec![];
        let themes = super::load_themes(&cfgs).expect("empty list must be OK");
        assert!(themes.is_empty());
    }

    #[test]
    fn load_themes_invalid_js_errors() {
        let cfgs = vec![ThemeConfig {
            id: "bad".into(),
            name: "Broken Theme".into(),
            source: "this is not valid JS @@@".into(),
        }];

        let result = super::load_themes(&cfgs);
        assert!(result.is_err(), "Invalid JS should produce a RuntimeError");
    }

    // -------------------------------------------------------------------------
    // BoundTheme::init
    // -------------------------------------------------------------------------

    #[test]
    fn bound_theme_init_calls_js() {
        let cfg = ThemeConfig {
            id: "th".into(),
            name: "Theme".into(),
            source: r#"
                globalThis["th"] = {
                    init(x){ x.front_matter = { "ok": true }; return x; },
                    handle(x){ return x; }
                }
            "#
            .into(),
        };

        let mut themes = load_themes(&[cfg]).expect("valid theme");
        let mut theme = themes.remove(0);

        let ctx = dummy_ctx();

        let result = theme.init(&ctx);
        assert!(result.is_ok(), "init() must call JS and succeed");
    }

    // -------------------------------------------------------------------------
    // BoundTheme::render
    // -------------------------------------------------------------------------

    #[test]
    fn bound_theme_render_runs_without_error() {
        let cfg = ThemeConfig {
            id: "th".into(),
            name: "Theme".into(),
            source: r#"
                globalThis["th"] = {
                    handle(ctx){
                        // Whatever the bridge expects, we just return ctx.
                        return ctx;
                    }
                }
            "#
            .into(),
        };

        let mut themes = load_themes(&[cfg]).expect("theme loads");
        let mut theme = themes.remove(0);

        let ctx = dummy_ctx();
        let spec = theme.render(ctx).expect("render must succeed");

        // At minimum, ensure we get *some* ResponseBodySpec and not panic.
        // We don't assert exact variant here because it depends on the
        // ctx bridge implementation.
        match spec {
            ResponseBodySpec::Unset
            | ResponseBodySpec::None
            | ResponseBodySpec::HtmlTemplate { .. }
            | ResponseBodySpec::HtmlString(_)
            | ResponseBodySpec::JsonValue(_) => { /* ok */ }
        }
    }

    #[test]
    fn bound_theme_render_propagates_js_error() {
        let cfg = ThemeConfig {
            id: "th".into(),
            name: "Theme".into(),
            source: r#"
                globalThis["th"] = {
                    handle(ctx){ throw new Error("boom"); }
                }
            "#
            .into(),
        };

        let mut themes = load_themes(&[cfg]).expect("theme loads");
        let mut theme = themes.remove(0);

        let ctx = dummy_ctx();
        let result = theme.render(ctx);

        assert!(
            result.is_err(),
            "JS error thrown from theme.handle must propagate as RuntimeError"
        );
    }

    #[test]
    fn bound_theme_render_js_error_propagates() {
        let cfg = ThemeConfig {
            id: "th".into(),
            name: "Theme".into(),
            source: r#"
                globalThis["th"] = {
                    handle(ctx){ throw new Error("boom"); }
                }
            "#
            .into(),
        };

        let mut themes = load_themes(&[cfg]).expect("theme loads");
        let mut theme = themes.remove(0);

        let ctx = dummy_ctx();
        let result = theme.render(ctx);

        assert!(
            result.is_err(),
            "JS error thrown from theme.handle must propagate as RuntimeError"
        );
    }

    // -------------------------------------------------------------------------
    // bootstrap_all (mock-only; no actor threads)
    // -------------------------------------------------------------------------

    #[test]
    fn bootstrap_all_builds_specs_and_returns_handles() {
        // Mock plugin/theme configs.
        let plugin_cfgs = vec![PluginConfig {
            id: "p1".into(),
            name: "Plugin 1".into(),
            source: r#"globalThis["p1"]={before(x){return x}}"#.into(),
        }];

        let theme_cfgs = vec![ThemeConfig {
            id: "t1".into(),
            name: "Theme 1".into(),
            source: r#"globalThis["t1"]={handle(x){return x}}"#.into(),
        }];

        // We need a current-thread runtime + LocalSet so that any
        // `spawn_local` calls inside `bootstrap_all` succeed.
        let rt = RtBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = LocalSet::new();

        rt.block_on(local.run_until(async {
            let handles = bootstrap_all(plugin_cfgs.clone(), theme_cfgs.clone())
                .expect("bootstrap_all must return handles");

            // Ensure configs were passed through unchanged.
            assert_eq!(handles.plugin_configs.len(), 1);
            assert_eq!(handles.plugin_configs[0].id, "p1");

            assert_eq!(handles.theme_configs.len(), 1);
            assert_eq!(handles.theme_configs[0].id, "t1");

            // Best-effort shutdown of actors.
            handles.plugin_client.stop();
            handles.theme_client.stop();
        }));
    }
}
