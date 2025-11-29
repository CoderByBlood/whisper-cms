// crates/adapt/src/runtime/theme.rs

use super::bridge::{ctx_to_js_for_theme, merge_theme_ctx_from_js, CTX_SHIM_SRC};
use super::error::RuntimeError;
use crate::js::{JsEngine, JsValue};
use serde_json;
use serve::render::http::RequestContext;
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
            "Before Handling theme {} with context {}",
            self.internal_id, ctx.req_id
        );
        let js_ctx = ctx_to_js_for_theme(ctx, &self.configured_id);

        let js_ctx = self.engine.call_function("__wrapCtx", &[js_ctx])?;

        let func_name = format!("{}.render", self.internal_id);
        let result = self.engine.call_function(&func_name, &[js_ctx])?;

        if let JsValue::Object(_) = result {
            merge_theme_ctx_from_js(&result, ctx)?;
        }

        debug!(
            "After Handling theme {} with context {}",
            self.internal_id, ctx.req_id
        );
        Ok(())
    }
}
