// crates/adapt/src/runtime/plugin.rs

use std::collections::HashMap;

use super::ctx_bridge::{ctx_to_js_for_plugins, merge_recommendations_from_js};
use super::error::RuntimeError;
use crate::js::{JsEngine, JsError, JsValue};
use crate::runtime::ctx_bridge::CTX_SHIM_SRC;
use serve::context::RequestContext;

use serde_json;
use uuid::Uuid;

/// Host-facing plugin spec. Configured ID never leaves Rust.
#[derive(Debug, Clone)]
pub struct PluginSpec {
    pub id: String,
    pub name: String,
    pub source: String,
}

/// Metadata for runtime bookkeeping
#[derive(Clone, Debug)]
pub struct PluginMeta {
    pub internal_id: String,   // opaque runtime ID, used to call hooks
    pub configured_id: String, // used ONLY for ctx.config lookup
    pub name: String,
}

/// PluginRuntime: manages a single Boa engine and multiple plugins inside it
#[derive(Debug)]
pub struct PluginRuntime<E: JsEngine> {
    engine: E,
    plugins: HashMap<String, PluginMeta>, // keyed by internal_id
}

fn build_plugin_prelude(internal_id: &str) -> String {
    let internal_id_json = serde_json::to_string(internal_id).unwrap();

    // No configured ID. No user ID. Only opaque internal ID known by host.
    format!(
        r#"(function (global) {{
    const INTERNAL_ID = {internal_id};

    // Host-provided registration. Plugins call this INSIDE init().
    global.registerPlugin = function(hooks) {{
        if (!hooks || typeof hooks !== "object") {{
            throw new Error("registerPlugin: hooks object is required");
        }}
        // We DO NOT register init. Only before/after.
        global[INTERNAL_ID] = {{
            before: typeof hooks.before === "function"
                ? hooks.before
                : undefined,
            after: typeof hooks.after === "function"
                ? hooks.after
                : undefined
        }};
    }};
}})(typeof globalThis !== "undefined" ? globalThis : this);"#,
        internal_id = internal_id_json,
    )
}

impl<E: JsEngine> PluginRuntime<E> {
    #[tracing::instrument(skip_all)]
    pub fn new(mut engine: E) -> Result<Self, RuntimeError> {
        engine.load_module("__ctx_shim__", CTX_SHIM_SRC)?;
        Ok(Self {
            engine,
            plugins: HashMap::new(),
        })
    }

    #[tracing::instrument(skip_all)]
    pub fn load_plugins(&mut self, specs: &[PluginSpec]) -> Result<(), RuntimeError> {
        for spec in specs {
            let configured_id = spec.id.clone();
            let internal_id = format!("plugin_{}", Uuid::new_v4().simple());

            // Prelude: defines registerPlugin()
            let prelude = build_plugin_prelude(&internal_id);
            let prelude_name = format!("__plugin_prelude_{}", internal_id);
            self.engine.load_module(&prelude_name, &prelude)?;

            // Load plugin JS → its top-level defines init(ctx)
            self.engine.load_module(&configured_id, &spec.source)?;

            // Record metadata
            self.plugins.insert(
                internal_id.clone(),
                PluginMeta {
                    internal_id,
                    configured_id,
                    name: spec.name.clone(),
                },
            );
        }

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub fn init_all(&mut self, ctx: &RequestContext) -> Result<(), RuntimeError> {
        let metas: Vec<PluginMeta> = self.plugins.values().cloned().collect();
        for meta in &metas {
            self.call_init(meta, ctx)?;
        }
        Ok(())
    }

    fn call_init(&mut self, meta: &PluginMeta, ctx: &RequestContext) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_plugins(ctx, &meta.configured_id);
        let js_ctx = self.engine.call_function("__wrapCtx", &[js_ctx])?;

        // Call global init(ctx) defined in plugin module.
        // Plugin decides whether to call registerPlugin inside.
        let result = self
            .engine
            .call_function("init", &[js_ctx])
            .or_else(|err| {
                if let JsError::Call(msg) = &err {
                    if msg.contains("is not a function") {
                        return Ok(JsValue::Null); // plugin has no init → ok
                    }
                }
                Err(err)
            })?;

        // Init is *not* merged — plugin returns ctx only for convenience.
        let _ = result;
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub fn before_all(&mut self, ctx: &mut RequestContext) -> Result<(), RuntimeError> {
        let metas: Vec<PluginMeta> = self.plugins.values().cloned().collect();
        for meta in &metas {
            self.call_before(meta, ctx)?;
        }
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub fn after_all(&mut self, ctx: &mut RequestContext) -> Result<(), RuntimeError> {
        // Reverse order for after()
        let mut metas: Vec<_> = self.plugins.values().cloned().collect();
        metas.reverse();

        for meta in &metas {
            self.call_after(meta, ctx)?;
        }
        Ok(())
    }

    fn call_before(
        &mut self,
        meta: &PluginMeta,
        ctx: &mut RequestContext,
    ) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_plugins(ctx, &meta.configured_id);
        let js_ctx = self.engine.call_function("__wrapCtx", &[js_ctx])?;

        let func_name = format!("{}.before", meta.internal_id);

        let result = self
            .engine
            .call_function(&func_name, &[js_ctx])
            .or_else(|err| {
                if let JsError::Call(msg) = &err {
                    if msg.contains("is not a function") {
                        return Ok(JsValue::Null); // no before()
                    }
                }
                Err(err)
            })?;

        if let JsValue::Object(_) = result {
            merge_recommendations_from_js(&result, ctx)?;
        }

        Ok(())
    }

    fn call_after(
        &mut self,
        meta: &PluginMeta,
        ctx: &mut RequestContext,
    ) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_plugins(ctx, &meta.configured_id);
        let js_ctx = self.engine.call_function("__wrapCtx", &[js_ctx])?;

        let func_name = format!("{}.after", meta.internal_id);

        let result = self
            .engine
            .call_function(&func_name, &[js_ctx])
            .or_else(|err| {
                if let JsError::Call(msg) = &err {
                    if msg.contains("is not a function") {
                        return Ok(JsValue::Null); // no after()
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
    use serde_json::json;
    use serve::context::RequestContext;
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
            internal_id: Uuid::new_v4().to_string(),
            configured_id: "p".into(),
            name: "Plugin".into(),
        };

        let clone = meta.clone();
        assert_eq!(clone.configured_id, "p");
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
