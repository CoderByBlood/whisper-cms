use super::ctx_bridge::{ctx_to_js_for_theme, merge_theme_ctx_from_js};
use super::error::RuntimeError;
use crate::core::context::RequestContext;
use crate::js::{JsEngine, JsValue};

/// Theme specification for loading a JS theme.
pub struct ThemeSpec {
    pub id: String,
    pub name: String,
    pub source: String,
}

impl ThemeSpec {
    pub fn new(id: impl Into<String>, name: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            source: source.into(),
        }
    }
}

/// ThemeRuntime manages a single JS theme.
///
/// It assumes the theme source, when evaluated, attaches an object like:
///
///   globalThis.themeId = {
///     init(ctx)   { /* optional */ },
///     handle(ctx) { /* required */ return ctx; }
///   };
pub struct ThemeRuntime<E: JsEngine> {
    engine: E,
    id: String,
    _name: String,
}

impl<E: JsEngine> ThemeRuntime<E> {
    /// Create a new ThemeRuntime by loading the theme module.
    pub fn new(mut engine: E, spec: ThemeSpec) -> Result<Self, RuntimeError> {
        engine.load_module(&spec.id, &spec.source)?;
        Ok(Self {
            engine,
            id: spec.id,
            _name: spec.name,
        })
    }

    /// Optionally call `init(ctx)` once (e.g. at startup).
    ///
    /// This is optional and may be skipped if you don't need theme init.
    pub fn init(&mut self, ctx: &RequestContext) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_theme(ctx);

        let _ = self
            .engine
            .call_function(&format!("{}.init", self.id), &[js_ctx])
            .or_else(|err| {
                if let crate::js::JsError::Call(msg) = &err {
                    if msg.contains("is not a function") {
                        // No init defined; ignore.
                        return Ok(JsValue::Null);
                    }
                }
                Err(err)
            })?;

        Ok(())
    }

    /// Call `handle(ctx)` on the theme.
    ///
    /// The theme is expected to:
    /// - inspect `ctx.request`, `ctx.content`, `ctx.config`, `ctx.recommend`, `ctx.response`
    /// - mutate ctx.recommend and ctx.response
    /// - return ctx
    pub fn handle(&mut self, ctx: &mut RequestContext) -> Result<(), RuntimeError> {
        let js_ctx = ctx_to_js_for_theme(ctx);

        let result = self
            .engine
            .call_function(&format!("{}.handle", self.id), &[js_ctx])?;

        if let JsValue::Object(_) = result {
            merge_theme_ctx_from_js(&result, ctx)?;
        } else {
            // If theme didn't return ctx, we still allow that, but no updates are applied.
        }

        Ok(())
    }
}
