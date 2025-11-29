// crates/adapt/src/runtime/plugin.rs

use std::collections::HashMap;

use super::bridge::{ctx_to_js_for_plugins, merge_recommendations_from_js, CTX_SHIM_SRC};
use super::error::RuntimeError;
use crate::js::{JsEngine, JsError, JsValue};
use serve::render::http::RequestContext;

use serde_json;
use tracing::debug;
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
    /// Keyed by internal (opaque) ID.
    plugins: HashMap<String, PluginMeta>,
}

#[tracing::instrument(skip_all)]
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

    /// Call `init(ctx)` on every loaded plugin, in registration order.
    #[tracing::instrument(skip_all)]
    pub fn init_all(&mut self, ctx: &RequestContext) -> Result<(), RuntimeError> {
        let metas: Vec<PluginMeta> = self.plugins.values().cloned().collect();
        for meta in &metas {
            self.call_init(meta, ctx)?;
        }
        Ok(())
    }

    #[tracing::instrument(skip_all)]
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
                        // Plugin has no init(ctx); that's fine.
                        return Ok(JsValue::Null);
                    }
                }
                Err(err)
            })?;

        // Init is *not* merged — plugin returns ctx only for convenience.
        let _ = result;
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────
    // Legacy "all-plugins" hooks (still available; actor no longer needs
    // them once the middleware is fully migrated, but keeping them for now).
    // ─────────────────────────────────────────────────────────────────────

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

    // ─────────────────────────────────────────────────────────────────────
    // NEW: per-plugin public hooks by configured ID
    // ─────────────────────────────────────────────────────────────────────

    /// Run the `before` hook for a single plugin identified by its
    /// configured ID (the host-facing `plugin.id`).
    ///
    /// If the plugin has no `before` hook or the ID is unknown, this is a no-op.
    #[tracing::instrument(skip_all)]
    pub fn before_plugin(
        &mut self,
        configured_id: &str,
        ctx: &mut RequestContext,
    ) -> Result<(), RuntimeError> {
        let meta_opt = {
            // Limit the immutable borrow of `self` to this block so we
            // can mutably borrow `self` later when calling `call_before`.
            let mut iter = self.plugins.values();
            iter.find(|m| m.configured_id == configured_id).cloned()
        };

        if let Some(meta) = meta_opt {
            self.call_before(&meta, ctx)?;
        }
        Ok(())
    }

    /// Run the `after` hook for a single plugin identified by its
    /// configured ID (the host-facing `plugin.id`).
    ///
    /// If the plugin has no `after` hook or the ID is unknown, this is a no-op.
    #[tracing::instrument(skip_all)]
    pub fn after_plugin(
        &mut self,
        configured_id: &str,
        ctx: &mut RequestContext,
    ) -> Result<(), RuntimeError> {
        let meta_opt = {
            // Again, restrict the immutable borrow of `self` to this block.
            let mut iter = self.plugins.values();
            iter.find(|m| m.configured_id == configured_id).cloned()
        };

        if let Some(meta) = meta_opt {
            self.call_after(&meta, ctx)?;
        }
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────────
    // Internal helpers for calling JS hooks
    // ─────────────────────────────────────────────────────────────────────

    #[tracing::instrument(skip_all)]
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
                        // Plugin has no before() hook; silently ignore.
                        return Ok(JsValue::Null);
                    }
                }
                Err(err)
            })?;

        debug!(
            "Result from executing plugin before lifecycle: {:?}",
            result
        );

        if let JsValue::Object(_) = result {
            merge_recommendations_from_js(&result, ctx)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip_all)]
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
                        // Plugin has no after() hook; silently ignore.
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
