// crates/adapt/src/runtime/plugin.rs

use std::collections::HashMap;

use crate::core::context::RequestContext;
use crate::js::JsError;
use crate::js::{JsEngine, JsValue};
use crate::runtime::ctx_bridge::CTX_SHIM_SRC;

use super::ctx_bridge::{ctx_to_js_for_plugins, merge_recommendations_from_js};
use super::error::RuntimeError;

/// Specification for a plugin, suitable for loading at startup.
///
/// Each plugin is expected to export an object on some internal key
/// (derived and managed by the host), with optional `init(ctx)`,
/// `before(ctx)`, and `after(ctx)` functions.
#[derive(Debug, Clone)]
pub struct PluginSpec {
    /// Host-facing plugin identifier (e.g. from config / disk layout).
    /// This is *not* necessarily the same as any JS-visible identifier.
    pub id: String,
    pub name: String,
    pub source: String,
}

/// Metadata about a loaded plugin.
///
/// This is purely host-side: JS never sees these IDs unless you choose to
/// expose them via config or ctx.
#[derive(Clone, Debug)]
pub struct PluginMeta {
    /// Internal plugin id used to resolve lifecycle functions in JS.
    pub id: String,
    pub name: String,
}

/// Runtime responsible for loading and invoking plugins.
///
/// - Uses a single `JsEngine` instance.
/// - Each plugin script is loaded via `engine.load_module(id, source)`.
/// - JS side is expected to attach lifecycle functions in a way that
///   the engine can resolve under the plugin's internal id:
///
///   ```js
///   // Example for an internal id like "plugin_abc123"
///   globalThis["plugin_abc123"] = {
///     init(ctx)   { /* optional */ },
///     before(ctx) { /* optional */ return ctx; },
///     after(ctx)  { /* optional */ return ctx; },
///   };
///   ```
///
/// - All JS hook functions are called with a single argument: `ctx`.
/// - If a hook is missing (`is not a function`), it is treated as a no-op.
/// - If a hook returns an object, we call `merge_recommendations_from_js`
///   so that plugins can emit header / model / body recommendations.
#[derive(Debug)]
pub struct PluginRuntime<E: JsEngine> {
    engine: E,
    /// Map from internal plugin id → plugin metadata.
    ///
    /// The keys here are internal IDs chosen by the host (you can reuse
    /// `PluginSpec.id` or generate opaque ones); they are what we pass to
    /// `ctx_to_js_for_plugins` and use in `<internalId>.before` lookups.
    plugins: HashMap<String, PluginMeta>,
}

impl<E: JsEngine> PluginRuntime<E> {
    /// Create a new runtime with a given `JsEngine`.
    #[tracing::instrument(skip_all)]
    pub fn new(mut engine: E) -> Result<Self, RuntimeError> {
        // Load the ctx shim into this engine.
        // You can handle error propagation more gracefully if your JsEngine
        // exposes a concrete error type; keeping it simple here.
        engine.load_module("__ctx_shim__", CTX_SHIM_SRC)?;
        Ok(Self {
            engine,
            plugins: HashMap::new(),
        })
    }

    /// Load all plugins from their specs.
    ///
    /// For each plugin:
    /// - `engine.load_module(plugin.id, plugin.source)`
    /// - the plugin JS is expected to wire its lifecycle functions
    ///   under that (internally generated) id.
    #[tracing::instrument(skip_all)]
    pub fn load_plugins(&mut self, specs: &[PluginSpec]) -> Result<(), RuntimeError> {
        for spec in specs {
            // In a more hardened version, you could generate an opaque internal id
            // here (e.g. "plugin_<uuid>") instead of reusing spec.id.
            let internal_id = spec.id.clone();

            self.engine.load_module(&internal_id, &spec.source)?;

            self.plugins.insert(
                internal_id.clone(),
                PluginMeta {
                    id: internal_id,
                    name: spec.name.clone(),
                },
            );
        }
        Ok(())
    }

    /// Call `<internalId>.init(ctx)` on all plugins at startup-like time.
    ///
    /// This is optional; you can call it with a special "init context"
    /// or per-request if you want. For now, it uses a `RequestContext`
    /// and ignores recommendations.
    ///
    /// Important: we *clone* the metadata before iterating to avoid
    /// borrowing `self.plugins` immutably while also mutably borrowing
    /// `self` inside `call_init`.
    #[tracing::instrument(skip_all)]
    pub fn init_all(&mut self, ctx: &RequestContext) -> Result<(), RuntimeError> {
        let metas: Vec<PluginMeta> = self.plugins.values().cloned().collect();
        for meta in &metas {
            self.call_init(meta, ctx)?;
        }
        Ok(())
    }

    /// Call `<internalId>.before(ctx)` on all plugins in load order.
    ///
    /// Recommendations from plugins are **appended** to `ctx.recommendations`.
    /// We clone the metas up front to avoid borrow conflicts.
    #[tracing::instrument(skip_all)]
    pub fn before_all(&mut self, ctx: &mut RequestContext) -> Result<(), RuntimeError> {
        let metas: Vec<PluginMeta> = self.plugins.values().cloned().collect();
        for meta in &metas {
            self.call_before(meta, ctx)?;
        }
        Ok(())
    }

    /// Call `<internalId>.after(ctx)` on all plugins in **reverse** load order.
    ///
    /// Recommendations from plugins are **appended** to `ctx.recommendations`.
    #[tracing::instrument(skip_all)]
    pub fn after_all(&mut self, ctx: &mut RequestContext) -> Result<(), RuntimeError> {
        let mut metas: Vec<PluginMeta> = self.plugins.values().cloned().collect();
        metas.reverse();
        for meta in &metas {
            self.call_after(meta, ctx)?;
        }
        Ok(())
    }

    /// Internal: call `<internalId>.init(ctx)` if present.
    ///
    /// Missing function is treated as no-op.
    #[tracing::instrument(skip_all)]
    fn call_init(&mut self, meta: &PluginMeta, ctx: &RequestContext) -> Result<(), RuntimeError> {
        // Build per-plugin ctx (e.g. with per-plugin config if you wire that in).
        let js_ctx = ctx_to_js_for_plugins(ctx, &meta.id);

        // Apply the JS-side shim to get the nice header API
        let js_ctx = self.engine.call_function("__wrapCtx", &[js_ctx])?;

        // We ignore the return value from init; it is not required to return ctx.
        let _ = self
            .engine
            .call_function(&format!("{}.init", meta.id), &[js_ctx])
            .or_else(|err| {
                // If function is missing, we treat it as "no init".
                if let JsError::Call(msg) = &err {
                    if msg.contains("is not a function") {
                        return Ok(JsValue::Null);
                    }
                }
                Err(err)
            })?;

        Ok(())
    }

    /// Internal: call `<internalId>.before(ctx)` if present.
    ///
    /// If the function returns an object, we try to merge recommendations.
    fn call_before(
        &mut self,
        meta: &PluginMeta,
        ctx: &mut RequestContext,
    ) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_plugins(ctx, &meta.id);

        // Apply the JS-side shim to get the nice header API
        let js_ctx = self.engine.call_function("__wrapCtx", &[js_ctx])?;

        // Plugins are expected to *return* the ctx object (possibly mutated),
        // so Rust can see the updated recommendations.
        let result = self
            .engine
            .call_function(&format!("{}.before", meta.id), &[js_ctx])
            .or_else(|err| {
                // If function is missing, treat as no-op.
                if let JsError::Call(msg) = &err {
                    if msg.contains("is not a function") {
                        return Ok(JsValue::Null);
                    }
                }
                Err(err)
            })?;

        if let JsValue::Object(_) = result {
            merge_recommendations_from_js(&result, ctx)?;
        }

        Ok(())
    }

    /// Internal: call `<internalId>.after(ctx)` if present.
    ///
    /// If the function returns an object, we try to merge recommendations.
    fn call_after(
        &mut self,
        meta: &PluginMeta,
        ctx: &mut RequestContext,
    ) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_plugins(ctx, &meta.id);

        // Apply the JS-side shim to get the nice header API
        let js_ctx = self.engine.call_function("__wrapCtx", &[js_ctx])?;

        let result = self
            .engine
            .call_function(&format!("{}.after", meta.id), &[js_ctx])
            .or_else(|err| {
                // If function is missing, treat as no-op.
                if let JsError::Call(msg) = &err {
                    if msg.contains("is not a function") {
                        return Ok(JsValue::Null);
                    }
                }
                Err(err)
            })?;

        if let JsValue::Object(_) = result {
            merge_recommendations_from_js(&result, ctx)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::RequestContext;
    use crate::js::{JsEngine, JsError, JsValue};
    use serde_json::json;
    use std::collections::HashMap;

    // ─────────────────────────────────────────────────────────────────────
    // Test helpers
    // ─────────────────────────────────────────────────────────────────────

    #[derive(Default, Debug)]
    struct MockEngine {
        // record load_module calls
        load_calls: Vec<(String, String)>,
        // optional error for next load_module call
        next_load_err: Option<JsError>,

        // func_path -> pre-programmed result
        call_results: HashMap<String, Result<JsValue, JsError>>,
        // log of function paths invoked
        call_log: Vec<String>,
    }

    impl MockEngine {
        fn with_load_error(err: JsError) -> Self {
            Self {
                next_load_err: Some(err),
                ..Self::default()
            }
        }
    }

    impl JsEngine for MockEngine {
        fn eval(&mut self, _code: &str) -> Result<JsValue, JsError> {
            Ok(JsValue::Null)
        }

        fn load_module(&mut self, name: &str, source: &str) -> Result<(), JsError> {
            self.load_calls.push((name.to_string(), source.to_string()));

            if let Some(err) = self.next_load_err.take() {
                Err(err)
            } else {
                Ok(())
            }
        }

        fn call_function(
            &mut self,
            func_path: &str,
            _args: &[JsValue],
        ) -> Result<JsValue, JsError> {
            self.call_log.push(func_path.to_string());
            self.call_results
                .remove(func_path)
                .unwrap_or(Ok(JsValue::Null))
        }
    }

    /// Build a minimal-but-valid RequestContext for plugin calls.
    /// Build a minimal-but-valid RequestContext for actor calls.
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

    // ─────────────────────────────────────────────────────────────────────
    // PluginSpec / PluginMeta basics
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn plugin_spec_fields_are_set_correctly() {
        let spec = PluginSpec {
            id: "plugin-1".into(),
            name: "Test Plugin".into(),
            source: "/* js */".into(),
        };

        assert_eq!(spec.id, "plugin-1");
        assert_eq!(spec.name, "Test Plugin");
        assert_eq!(spec.source, "/* js */");
    }

    #[test]
    fn plugin_meta_clone_round_trips() {
        let meta = PluginMeta {
            id: "p".into(),
            name: "Plugin".into(),
        };

        let clone = meta.clone();
        assert_eq!(clone.id, "p");
        assert_eq!(clone.name, "Plugin");
    }

    // ─────────────────────────────────────────────────────────────────────
    // load_plugins
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn load_plugins_success_populates_plugins_and_calls_engine() {
        let engine = MockEngine::default();
        let mut runtime = PluginRuntime::new(engine).unwrap();

        let specs = vec![
            PluginSpec {
                id: "p1".into(),
                name: "Plugin 1".into(),
                source: "/* p1 */".into(),
            },
            PluginSpec {
                id: "p2".into(),
                name: "Plugin 2".into(),
                source: "/* p2 */".into(),
            },
        ];

        let result = runtime.load_plugins(&specs);
        assert!(result.is_ok(), "load_plugins should succeed");

        // Inspect private fields from the same module.
        assert_eq!(runtime.plugins.len(), 2);
        assert!(runtime.plugins.contains_key("p1"));
        assert!(runtime.plugins.contains_key("p2"));

        let load_calls = &runtime.engine.load_calls;
        assert_eq!(load_calls.len(), 3);
        assert_eq!(load_calls[0].0, "__ctx_shim__");
        assert_eq!(load_calls[1].0, "p1");
        assert_eq!(load_calls[2].0, "p2");
    }

    #[test]
    fn load_plugins_propagates_engine_load_error() {
        let engine = MockEngine::with_load_error(JsError::Eval("boom".into()));
        let err = PluginRuntime::new(engine).expect_err("expected load error");

        match err {
            RuntimeError::Js(js_err) => {
                let s = js_err.to_string();
                assert!(
                    s.contains("boom"),
                    "expected JsError to contain 'boom', got {s}"
                );
            }
            other => panic!("expected RuntimeError::Js, got {other:?}"),
        }
    }

    #[test]
    fn load_plugins_on_empty_list_is_ok_and_keeps_plugins_empty() {
        let engine = MockEngine::default();
        let mut runtime = PluginRuntime::new(engine).unwrap();

        let specs: Vec<PluginSpec> = Vec::new();
        let result = runtime.load_plugins(&specs);

        assert!(result.is_ok());
        assert!(runtime.plugins.is_empty());
        assert_eq!(
            runtime.engine.load_calls.len(),
            1,
            "only the ctx shim should have been loaded"
        );
        assert_eq!(
            runtime.engine.load_calls[0].0,
            "__ctx_shim__".to_string(),
            "first load must be the ctx shim module"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // init_all
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn init_all_calls_init_for_all_plugins_and_ignores_missing_function() {
        let mut engine = MockEngine::default();
        // p1.init exists and returns null
        engine
            .call_results
            .insert("p1.init".into(), Ok(JsValue::Null));
        // p2.init is "not a function" and should be treated as no-op
        engine.call_results.insert(
            "p2.init".into(),
            Err(JsError::Call("init is not a function".into())),
        );

        let mut runtime = PluginRuntime::new(engine).unwrap();
        let specs = vec![
            PluginSpec {
                id: "p1".into(),
                name: "Plugin 1".into(),
                source: "/* p1 */".into(),
            },
            PluginSpec {
                id: "p2".into(),
                name: "Plugin 2".into(),
                source: "/* p2 */".into(),
            },
        ];
        runtime
            .load_plugins(&specs)
            .expect("load_plugins should succeed");

        let ctx = dummy_ctx();
        let result = runtime.init_all(&ctx);

        assert!(result.is_ok(), "init_all should succeed");
        // Both init functions should have been attempted.
        let log = &runtime.engine.call_log;
        assert!(log.contains(&"p1.init".to_string()));
        assert!(log.contains(&"p2.init".to_string()));
    }

    #[test]
    fn init_all_propagates_non_missing_function_error() {
        let mut engine = MockEngine::default();
        engine.call_results.insert(
            "p1.init".into(),
            Err(JsError::Eval("boom during init".into())),
        );

        let mut runtime = PluginRuntime::new(engine).unwrap();
        let specs = vec![PluginSpec {
            id: "p1".into(),
            name: "Plugin 1".into(),
            source: "/* p1 */".into(),
        }];
        runtime
            .load_plugins(&specs)
            .expect("load_plugins should succeed");

        let ctx = dummy_ctx();
        let result = runtime.init_all(&ctx);

        assert!(
            result.is_err(),
            "init_all should propagate real errors from init hooks"
        );
    }

    #[test]
    fn init_all_with_no_plugins_is_noop() {
        let engine = MockEngine::default();
        let mut runtime = PluginRuntime::new(engine).unwrap();

        let ctx = dummy_ctx();
        let result = runtime.init_all(&ctx);

        assert!(result.is_ok());
        assert!(runtime.engine.call_log.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────
    // before_all
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn before_all_calls_before_for_each_plugin() {
        let mut engine = MockEngine::default();
        // two plugins, both with a before hook returning null
        engine
            .call_results
            .insert("p1.before".into(), Ok(JsValue::Null));
        engine
            .call_results
            .insert("p2.before".into(), Ok(JsValue::Null));

        let mut runtime = PluginRuntime::new(engine).unwrap();
        let specs = vec![
            PluginSpec {
                id: "p1".into(),
                name: "Plugin 1".into(),
                source: "/* p1 */".into(),
            },
            PluginSpec {
                id: "p2".into(),
                name: "Plugin 2".into(),
                source: "/* p2 */".into(),
            },
        ];
        runtime
            .load_plugins(&specs)
            .expect("load_plugins should succeed");

        let mut ctx = dummy_ctx();
        let result = runtime.before_all(&mut ctx);

        assert!(result.is_ok(), "before_all should succeed");
        let log = &runtime.engine.call_log;
        // We don't assert on order because HashMap iteration order is not stable.
        assert!(log.contains(&"p1.before".to_string()));
        assert!(log.contains(&"p2.before".to_string()));
    }

    #[test]
    fn before_all_treats_missing_before_as_noop() {
        let mut engine = MockEngine::default();
        engine.call_results.insert(
            "p1.before".into(),
            Err(JsError::Call("before is not a function".into())),
        );

        let mut runtime = PluginRuntime::new(engine).unwrap();
        let specs = vec![PluginSpec {
            id: "p1".into(),
            name: "Plugin 1".into(),
            source: "/* p1 */".into(),
        }];
        runtime
            .load_plugins(&specs)
            .expect("load_plugins should succeed");

        let mut ctx = dummy_ctx();
        let result = runtime.before_all(&mut ctx);

        assert!(
            result.is_ok(),
            "missing before hook should be treated as no-op"
        );
        assert_eq!(
            runtime.engine.call_log,
            vec!["__wrapCtx".to_string(), "p1.before".to_string()]
        );
    }

    #[test]
    fn before_all_propagates_real_before_error() {
        let mut engine = MockEngine::default();
        engine
            .call_results
            .insert("p1.before".into(), Err(JsError::Eval("boom before".into())));

        let mut runtime = PluginRuntime::new(engine).unwrap();
        let specs = vec![PluginSpec {
            id: "p1".into(),
            name: "Plugin 1".into(),
            source: "/* p1 */".into(),
        }];
        runtime
            .load_plugins(&specs)
            .expect("load_plugins should succeed");

        let mut ctx = dummy_ctx();
        let result = runtime.before_all(&mut ctx);

        assert!(
            result.is_err(),
            "before_all should propagate real errors from before hooks"
        );
    }

    #[test]
    fn before_all_with_no_plugins_is_noop() {
        let engine = MockEngine::default();
        let mut runtime = PluginRuntime::new(engine).unwrap();

        let mut ctx = dummy_ctx();
        let result = runtime.before_all(&mut ctx);

        assert!(result.is_ok());
        assert!(runtime.engine.call_log.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────
    // after_all
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn after_all_calls_after_for_each_plugin() {
        let mut engine = MockEngine::default();
        engine
            .call_results
            .insert("p1.after".into(), Ok(JsValue::Null));
        engine
            .call_results
            .insert("p2.after".into(), Ok(JsValue::Null));

        let mut runtime = PluginRuntime::new(engine).unwrap();
        let specs = vec![
            PluginSpec {
                id: "p1".into(),
                name: "Plugin 1".into(),
                source: "/* p1 */".into(),
            },
            PluginSpec {
                id: "p2".into(),
                name: "Plugin 2".into(),
                source: "/* p2 */".into(),
            },
        ];
        runtime
            .load_plugins(&specs)
            .expect("load_plugins should succeed");

        let mut ctx = dummy_ctx();
        let result = runtime.after_all(&mut ctx);

        assert!(result.is_ok(), "after_all should succeed");
        let log = &runtime.engine.call_log;
        assert!(log.contains(&"p1.after".to_string()));
        assert!(log.contains(&"p2.after".to_string()));
    }

    #[test]
    fn after_all_treats_missing_after_as_noop() {
        let mut engine = MockEngine::default();
        engine.call_results.insert(
            "p1.after".into(),
            Err(JsError::Call("after is not a function".into())),
        );

        let mut runtime = PluginRuntime::new(engine).unwrap();
        let specs = vec![PluginSpec {
            id: "p1".into(),
            name: "Plugin 1".into(),
            source: "/* p1 */".into(),
        }];
        runtime
            .load_plugins(&specs)
            .expect("load_plugins should succeed");

        let mut ctx = dummy_ctx();
        let result = runtime.after_all(&mut ctx);

        assert!(
            result.is_ok(),
            "missing after hook should be treated as no-op"
        );
        assert_eq!(
            runtime.engine.call_log,
            vec!["__wrapCtx".to_string(), "p1.after".to_string()]
        );
    }

    #[test]
    fn after_all_propagates_real_after_error() {
        let mut engine = MockEngine::default();
        engine
            .call_results
            .insert("p1.after".into(), Err(JsError::Eval("boom after".into())));

        let mut runtime = PluginRuntime::new(engine).unwrap();
        let specs = vec![PluginSpec {
            id: "p1".into(),
            name: "Plugin 1".into(),
            source: "/* p1 */".into(),
        }];
        runtime
            .load_plugins(&specs)
            .expect("load_plugins should succeed");

        let mut ctx = dummy_ctx();
        let result = runtime.after_all(&mut ctx);

        assert!(
            result.is_err(),
            "after_all should propagate real errors from after hooks"
        );
    }

    #[test]
    fn after_all_with_no_plugins_is_noop() {
        let engine = MockEngine::default();
        let mut runtime = PluginRuntime::new(engine).unwrap();

        let mut ctx = dummy_ctx();
        let result = runtime.after_all(&mut ctx);

        assert!(result.is_ok());
        assert!(runtime.engine.call_log.is_empty());
    }
}
