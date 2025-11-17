use std::collections::HashMap;

use crate::core::context::RequestContext;
use crate::js::JsError;
use crate::js::{JsEngine, JsValue};

use super::ctx_bridge::{ctx_to_js_for_plugins, merge_recommendations_from_js};
use super::error::RuntimeError;

/// Specification for a plugin, suitable for loading at startup.
///
/// Each plugin is expected to export an object on `globalThis[plugin.id]`
/// with optional `init(ctx)`, `before(ctx)`, and `after(ctx)` functions.
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
/// - JS side is expected to attach to `globalThis[id]` an object:
///
///   ```js
///   globalThis["my-plugin"] = {
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
    /// - expect that plugin JS attaches to `globalThis[plugin.id]`
    ///   an object with optional `init/before/after` methods.
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

    /// Call `globalThis[id].init(ctx)` on all plugins at startup-like time.
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

    /// Call `globalThis[id].before(ctx)` on all plugins in load order.
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

    /// Call `globalThis[id].after(ctx)` on all plugins in **reverse** load order.
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

    /// Internal: call `globalThis[id].init(ctx)` if present.
    ///
    /// Missing function is treated as no-op.
    fn call_init(&mut self, meta: &PluginMeta, ctx: &RequestContext) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_plugins(ctx);

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

    /// Internal: call `globalThis[id].before(ctx)` if present.
    ///
    /// If the function returns an object, we try to merge recommendations.
    fn call_before(
        &mut self,
        meta: &PluginMeta,
        ctx: &mut RequestContext,
    ) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_plugins(ctx);

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

    /// Internal: call `globalThis[id].after(ctx)` if present.
    ///
    /// If the function returns an object, we try to merge recommendations.
    fn call_after(
        &mut self,
        meta: &PluginMeta,
        ctx: &mut RequestContext,
    ) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_plugins(ctx);

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
