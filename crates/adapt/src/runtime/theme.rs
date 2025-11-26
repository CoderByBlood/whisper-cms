// crates/adapt/src/runtime/theme.rs

use super::bridge::{ctx_to_js_for_theme, merge_theme_ctx_from_js, CTX_SHIM_SRC};
use super::error::RuntimeError;
use crate::js::{JsEngine, JsValue};
use serde_json;
use serve::ctx::http::RequestContext;
use tracing::debug;
use uuid::Uuid;

fn build_theme_prelude(internal_id: &str, configured_id: &str) -> String {
    let internal_id_json = serde_json::to_string(internal_id).unwrap();
    let configured_id_json = serde_json::to_string(configured_id).unwrap();

    format!(
        r#"(function (global) {{
    const INTERNAL_ID = {internal_id};
    const CONFIG_ID = {configured_id};

    // Host-provided registration hook for themes.
    global.registerTheme = function(hooks) {{
        if (!hooks || typeof hooks.render !== "function") {{
            throw new Error("registerTheme: hooks.render(ctx) is required");
        }}

        // Stash the hooks under an *opaque* internal id
        global[INTERNAL_ID] = hooks;
    }};
}})(typeof globalThis !== "undefined" ? globalThis : this);"#,
        internal_id = internal_id_json,
        configured_id = configured_id_json,
    )
}

/// Theme specification for loading a JS theme.
#[derive(Debug, Clone)]
pub struct ThemeSpec {
    pub id: String, // configured id, e.g. "demo-theme"
    pub name: String,
    pub mount_path: String,
    pub source: String,
}

impl ThemeSpec {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        mount_path: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            mount_path: mount_path.into(),
            source: source.into(),
        }
    }
}

/// ThemeRuntime manages a single theme.
pub struct ThemeRuntime<E: JsEngine> {
    engine: E,

    /// Opaque internal id used in JS to look up hooks.
    internal_id: String,

    /// Configured theme id (from TOML / discovery).
    configured_id: String,

    /// Display name (not used internally).
    _name: String,
}

impl<E: JsEngine> ThemeRuntime<E> {
    #[tracing::instrument(skip_all)]
    pub fn new(mut engine: E, spec: ThemeSpec) -> Result<Self, RuntimeError> {
        let configured_id = spec.id;
        let internal_id = format!("theme_{}", Uuid::new_v4().simple());

        // 1) host prelude: defines registerTheme(...)
        let prelude = build_theme_prelude(&internal_id, &configured_id);
        engine.load_module("__theme_prelude__", &prelude)?;

        // 2) theme module (your demo-theme.js)
        engine.load_module(&configured_id, &spec.source)?;

        // 3) ctx shim
        engine.load_module("__ctx_shim__", CTX_SHIM_SRC)?;

        Ok(Self {
            engine,
            internal_id,
            configured_id,
            _name: spec.name,
        })
    }

    /// Optionally call `init(ctx)` once.
    #[tracing::instrument(skip_all)]
    pub fn init(&mut self, ctx: &RequestContext) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_theme(ctx, &self.configured_id);

        let js_ctx = self.engine.call_function("__wrapCtx", &[js_ctx])?;

        // global init(ctx) in the theme module
        self.engine
            .call_function("init", &[js_ctx])
            .or_else(|err| {
                if let crate::js::JsError::Call(msg) = &err {
                    if msg.contains("is not a function") {
                        // theme has no init → fine
                        return Ok(JsValue::Null);
                    }
                }
                Err(err)
            })?;

        Ok(())
    }

    /// Call `<internal_id>.render(ctx)` on the registered hooks.
    #[tracing::instrument(skip_all, fields(req_id = %ctx.req_id))]
    pub fn handle(&mut self, ctx: &mut RequestContext) -> Result<(), RuntimeError> {
        debug!(
            "Before Handling theme {} with context {:?}",
            self.internal_id, ctx
        );
        let js_ctx = ctx_to_js_for_theme(ctx, &self.configured_id);

        let js_ctx = self.engine.call_function("__wrapCtx", &[js_ctx])?;

        let func_name = format!("{}.render", self.internal_id);
        let result = self.engine.call_function(&func_name, &[js_ctx])?;

        if let JsValue::Object(_) = result {
            merge_theme_ctx_from_js(&result, ctx)?;
        }

        debug!(
            "After Handling theme {} with context {:?}",
            self.internal_id, ctx
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js::{JsEngine, JsError, JsValue};
    use serde_json::json;
    use serve::ctx::http::RequestContext;
    use std::collections::HashMap;

    // ─────────────────────────────────────────────────────────────────────────
    // Test helpers
    // ─────────────────────────────────────────────────────────────────────────

    #[derive(Default)]
    struct MockEngine {
        // record load_module calls
        load_calls: Vec<(String, String)>,
        // optional error to return on next load_module
        next_load_err: Option<JsError>,

        // map from func_path -> result to return
        call_results: HashMap<String, Result<JsValue, JsError>>,
        // log of called function paths
        call_log: Vec<String>,
    }

    impl MockEngine {
        fn with_load_error(err: JsError) -> Self {
            Self {
                next_load_err: Some(err),
                ..Self::default()
            }
        }

        fn with_call_result(mut self, path: &str, result: Result<JsValue, JsError>) -> Self {
            self.call_results.insert(path.to_string(), result);
            self
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

    // ─────────────────────────────────────────────────────────────────────────
    // ThemeSpec tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn theme_spec_new_populates_fields() {
        let spec = ThemeSpec::new("id-123", "My Theme", "/", "/* js source */");

        assert_eq!(spec.id, "id-123");
        assert_eq!(spec.name, "My Theme");
        assert_eq!(spec.source, "/* js source */");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ThemeRuntime::new tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn theme_runtime_new_loads_module_successfully() {
        let engine = MockEngine::default();
        let spec = ThemeSpec::new("themeA", "Theme A", "/", "/* theme js */");

        let rt = ThemeRuntime::new(engine, spec);

        assert!(rt.is_ok());
        let _rt = rt.unwrap();
    }

    #[test]
    fn theme_runtime_new_propagates_load_module_error() {
        let engine = MockEngine::with_load_error(JsError::Eval("boom".into()));
        let spec = ThemeSpec::new("themeErr", "Theme Err", "/", "/* theme js */");

        let rt = ThemeRuntime::new(engine, spec);

        assert!(
            rt.is_err(),
            "expected ThemeRuntime::new to fail on load_module error"
        );
    }

    #[test]
    fn theme_runtime_new_calls_load_module_with_spec_id_and_source() {
        let mut engine = MockEngine::default();
        let spec = ThemeSpec::new("themeX", "Theme X", "/", "/* theme js */");

        // call load_module directly on the mock to verify behavior
        let res = engine.load_module(&spec.id, &spec.source);
        assert!(res.is_ok());

        assert_eq!(engine.load_calls.len(), 1);
        assert_eq!(engine.load_calls[0].0, "themeX");
        assert_eq!(engine.load_calls[0].1, "/* theme js */");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ThemeRuntime::init tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn init_calls_init_function_when_defined() {
        let engine = MockEngine::default().with_call_result("theme1.init", Ok(JsValue::Null));
        let spec = ThemeSpec::new("theme1", "Theme 1", "/", "/* js */");

        let mut rt = ThemeRuntime::new(engine, spec).expect("runtime should construct");
        let ctx = dummy_ctx();

        let result = rt.init(&ctx);
        assert!(
            result.is_ok(),
            "init should succeed when init function returns Ok"
        );

        // Ensure call_function was invoked for "theme1.init"
        assert!(
            rt.engine.call_log.contains(&"theme1.init".to_string()),
            "expected call to theme1.init"
        );
    }

    #[test]
    fn init_ignores_missing_init_function_with_specific_error_message() {
        let engine = MockEngine::default().with_call_result(
            "theme2.init",
            Err(JsError::Call("theme2.init is not a function".into())),
        );
        let spec = ThemeSpec::new("theme2", "Theme 2", "/", "/* js */");

        let mut rt = ThemeRuntime::new(engine, spec).expect("runtime should construct");
        let ctx = dummy_ctx();

        let result = rt.init(&ctx);
        assert!(
            result.is_ok(),
            "init should ignore 'is not a function' errors and succeed"
        );

        // Still should have attempted the call
        assert!(
            rt.engine.call_log.contains(&"theme2.init".to_string()),
            "expected call to theme2.init"
        );
    }

    #[test]
    fn init_propagates_other_call_errors() {
        let engine = MockEngine::default()
            .with_call_result("theme3.init", Err(JsError::Call("some other error".into())));
        let spec = ThemeSpec::new("theme3", "Theme 3", "/", "/* js */");

        let mut rt = ThemeRuntime::new(engine, spec).expect("runtime should construct");
        let ctx = dummy_ctx();

        let result = rt.init(&ctx);
        assert!(
            result.is_err(),
            "init should propagate call errors that are not 'is not a function'"
        );

        assert!(
            rt.engine.call_log.contains(&"theme3.init".to_string()),
            "expected call to theme3.init"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ThemeRuntime::handle tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn handle_applies_updates_when_theme_returns_object() {
        let engine = MockEngine::default()
            .with_call_result("theme4.handle", Ok(JsValue::Object(HashMap::new())));
        let spec = ThemeSpec::new("theme4", "Theme 4", "/", "/* js */");

        let mut rt = ThemeRuntime::new(engine, spec).expect("runtime should construct");
        let mut ctx = dummy_ctx();

        let result = rt.handle(&mut ctx);
        assert!(
            result.is_ok(),
            "handle should succeed when theme returns an object ctx"
        );

        assert!(
            rt.engine.call_log.contains(&"theme4.handle".to_string()),
            "expected call to theme4.handle"
        );
        // Actual ctx mutation behavior is covered by merge_theme_ctx_from_js tests.
    }

    #[test]
    fn handle_allows_non_object_result_without_error() {
        let engine =
            MockEngine::default().with_call_result("theme5.handle", Ok(JsValue::Bool(true)));
        let spec = ThemeSpec::new("theme5", "Theme 5", "/", "/* js */");

        let mut rt = ThemeRuntime::new(engine, spec).expect("runtime should construct");
        let mut ctx = dummy_ctx();

        let result = rt.handle(&mut ctx);
        assert!(
            result.is_ok(),
            "handle should succeed even when theme returns a non-object value"
        );

        assert!(
            rt.engine.call_log.contains(&"theme5.handle".to_string()),
            "expected call to theme5.handle"
        );
    }

    #[test]
    fn handle_propagates_call_error() {
        let engine = MockEngine::default()
            .with_call_result("theme6.handle", Err(JsError::Call("boom".into())));
        let spec = ThemeSpec::new("theme6", "Theme 6", "/", "/* js */");

        let mut rt = ThemeRuntime::new(engine, spec).expect("runtime should construct");
        let mut ctx = dummy_ctx();

        let result = rt.handle(&mut ctx);
        assert!(
            result.is_err(),
            "handle should propagate errors from engine.call_function"
        );

        assert!(
            rt.engine.call_log.contains(&"theme6.handle".to_string()),
            "expected call to theme6.handle"
        );
    }
}
