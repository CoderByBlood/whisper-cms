use std::collections::HashMap;

use crate::core::context::RequestContext;
use crate::js::JsError;
use crate::js::{JsEngine, JsValue};

use super::ctx_bridge::{ctx_to_js_for_plugins, merge_recommendations_from_js};
use super::error::RuntimeError;

/// Specification for a plugin, suitable for loading at startup.
///
/// Each plugin is expected to export an object on some internal key
/// (derived and managed by the host), with optional `init(ctx)`,
/// `before(ctx)`, and `after(ctx)` functions.
pub struct PluginSpec {
    pub id: String,
    pub name: String,
    pub source: String,
}

/// Metadata about a loaded plugin.
#[derive(Clone)]
pub struct PluginMeta {
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
///   globalThis[internalId] = {
///     init(ctx) { ... },   // optional
///     before(ctx) { ... }, // optional
///     after(ctx) { ... },  // optional
///   };
///   ```
///
/// - All JS hook functions are called with a single argument: `ctx`.
/// - If a hook is missing (`is not a function`), it is treated as a no-op.
/// - If a hook returns an object, we call `merge_recommendations_from_js`
///   so that plugins can emit header / model / body recommendations.
pub struct PluginRuntime<E: JsEngine> {
    engine: E,
    /// Map from internal plugin id → plugin metadata.
    plugins: HashMap<String, PluginMeta>,
}

impl<E: JsEngine> PluginRuntime<E> {
    /// Create a new runtime with a given `JsEngine`.
    pub fn new(engine: E) -> Self {
        Self {
            engine,
            plugins: HashMap::new(),
        }
    }

    /// Load all plugins from their specs.
    ///
    /// For each plugin:
    /// - `engine.load_module(plugin.id, plugin.source)`
    /// - the plugin JS is expected to wire its lifecycle functions
    ///   under that (internally generated) id.
    pub fn load_plugins(&mut self, specs: &[PluginSpec]) -> Result<(), RuntimeError> {
        for spec in specs {
            self.engine.load_module(&spec.id, &spec.source)?;
            self.plugins.insert(
                spec.id.clone(),
                PluginMeta {
                    id: spec.id.clone(),
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
    fn call_init(&mut self, meta: &PluginMeta, ctx: &RequestContext) -> Result<(), RuntimeError> {
        // Build per-plugin ctx (e.g. with per-plugin config).
        let js_ctx = ctx_to_js_for_plugins(ctx, &meta.id);

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
    use crate::js::{JsEngine, JsError, JsValue};
    use std::collections::HashMap;

    // ─────────────────────────────────────────────────────────────────────
    // Test helpers
    // ─────────────────────────────────────────────────────────────────────

    #[derive(Default)]
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
        let mut runtime = PluginRuntime::new(engine);

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

        // We’re inside the same module, so we can inspect private fields.
        assert_eq!(runtime.plugins.len(), 2);
        assert!(runtime.plugins.contains_key("p1"));
        assert!(runtime.plugins.contains_key("p2"));

        let load_calls = &runtime.engine.load_calls;
        assert_eq!(load_calls.len(), 2);
        assert_eq!(load_calls[0].0, "p1");
        assert_eq!(load_calls[1].0, "p2");
    }

    #[test]
    fn load_plugins_propagates_engine_load_error() {
        let engine = MockEngine::with_load_error(JsError::Eval("boom".into()));
        let mut runtime = PluginRuntime::new(engine);

        let specs = vec![PluginSpec {
            id: "p1".into(),
            name: "Plugin 1".into(),
            source: "/* p1 */".into(),
        }];

        let result = runtime.load_plugins(&specs);
        assert!(
            result.is_err(),
            "load_plugins should propagate load_module errors"
        );
    }

    #[test]
    fn load_plugins_on_empty_list_is_ok_and_keeps_plugins_empty() {
        let engine = MockEngine::default();
        let mut runtime = PluginRuntime::new(engine);

        let specs: Vec<PluginSpec> = Vec::new();
        let result = runtime.load_plugins(&specs);

        assert!(result.is_ok());
        assert!(runtime.plugins.is_empty());
        assert!(runtime.engine.load_calls.is_empty());
    }
}
