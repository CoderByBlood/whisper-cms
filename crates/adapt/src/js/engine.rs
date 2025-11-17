use super::error::JsError;
use super::value::JsValue;
use boa_engine::context::Context;
use boa_engine::property::PropertyKey;
use boa_engine::JsValue as BoaJsValue;
use boa_engine::{js_string, Source};
use serde_json::Value as Json;

/// Engine abstraction.
///
/// For now, all calls are synchronous and assume the JS function returns
/// a JSON-like value (no engine-specific objects crossing the boundary).
pub trait JsEngine {
    /// Evaluate arbitrary JS code and return a JsValue.
    fn eval(&mut self, code: &str) -> Result<JsValue, JsError>;

    /// Load a module / plugin script.
    ///
    /// For Boa we simply `eval` the source into the current context. The
    /// `name` is currently unused but kept for future engine implementations.
    fn load_module(&mut self, name: &str, source: &str) -> Result<(), JsError>;

    /// Call a JS function by a dotted path (e.g. "plugin.handle" or "theme.handle").
    fn call_function(&mut self, func_path: &str, args: &[JsValue]) -> Result<JsValue, JsError>;
}

/// Concrete Boa-backed engine.
///
/// This is intentionally simple: a single Context which we keep alive and
/// reuse. All conversions to/from Rust types go through serde_json::Value.
pub struct BoaEngine {
    context: Context,
}

impl BoaEngine {
    pub fn new() -> Self {
        Self {
            context: Context::default(),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Host <-> serde_json <-> Boa conversions
    // ─────────────────────────────────────────────────────────────────────────

    fn host_to_json(value: &JsValue) -> Json {
        match value {
            JsValue::Null => Json::Null,
            JsValue::Bool(b) => Json::Bool(*b),
            JsValue::Number(n) => Json::Number(
                serde_json::Number::from_f64(*n).unwrap_or_else(|| serde_json::Number::from(0)),
            ),
            JsValue::String(s) => Json::String(s.clone()),
            JsValue::Array(items) => Json::Array(items.iter().map(Self::host_to_json).collect()),
            JsValue::Object(map) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in map {
                    obj.insert(k.clone(), Self::host_to_json(v));
                }
                Json::Object(obj)
            }
        }
    }

    fn json_to_host(value: &Json) -> JsValue {
        match value {
            Json::Null => JsValue::Null,
            Json::Bool(b) => JsValue::Bool(*b),
            Json::Number(n) => JsValue::Number(n.as_f64().unwrap_or(0.0)),
            Json::String(s) => JsValue::String(s.clone()),
            Json::Array(items) => JsValue::Array(items.iter().map(Self::json_to_host).collect()),
            Json::Object(map) => {
                let mut obj = std::collections::HashMap::new();
                for (k, v) in map {
                    obj.insert(k.clone(), Self::json_to_host(v));
                }
                JsValue::Object(obj)
            }
        }
    }

    fn to_boajs_value(&mut self, value: &JsValue) -> Result<BoaJsValue, JsError> {
        let json = Self::host_to_json(value);
        BoaJsValue::from_json(&json, &mut self.context)
            .map_err(|e| JsError::Conversion(e.to_string()))
    }

    fn from_boajs_value(&mut self, value: &BoaJsValue) -> Result<JsValue, JsError> {
        let json = value
            .to_json(&mut self.context)
            .map_err(|e| JsError::Conversion(e.to_string()))?;
        Ok(match json {
            Some(ref v) => Self::json_to_host(v),
            None => JsValue::Null,
        })
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Function resolution
    // ─────────────────────────────────────────────────────────────────────────

    /// Resolve a dotted path against the global object, e.g. "plugin.handle".
    fn resolve_path(&mut self, func_path: &str) -> Result<BoaJsValue, JsError> {
        let trimmed = func_path.trim();
        if trimmed.is_empty() {
            return Err(JsError::Call("empty function path".into()));
        }

        // Start from global object.
        let mut current = BoaJsValue::new(self.context.global_object().clone());

        for part in trimmed.split('.') {
            let obj = current
                .as_object()
                .ok_or_else(|| JsError::Call("intermediate value is not object".into()))?
                .clone();

            let key = PropertyKey::from(js_string!(part));
            current = obj
                .get(key, &mut self.context)
                .map_err(|e| JsError::Call(e.to_string()))?;
        }

        Ok(current)
    }
}

impl JsEngine for BoaEngine {
    fn eval(&mut self, code: &str) -> Result<JsValue, JsError> {
        match self.context.eval(Source::from_bytes(code)) {
            Ok(v) => self.from_boajs_value(&v),
            Err(e) => Err(JsError::Eval(e.to_string())),
        }
    }

    fn load_module(&mut self, _name: &str, source: &str) -> Result<(), JsError> {
        // For Boa, "loading a module" is just evaluating the source in this context.
        // The module itself is expected to attach things to globalThis (e.g.,
        // globalThis.plugin = { init(ctx) { ... }, handle(ctx) { ... } }).
        self.context
            .eval(Source::from_bytes(source))
            .map_err(|e| JsError::Eval(e.to_string()))?;
        Ok(())
    }

    fn call_function(&mut self, func_path: &str, args: &[JsValue]) -> Result<JsValue, JsError> {
        // Resolve the function value.
        let func_val = self.resolve_path(func_path)?;
        let func_obj = func_val
            .as_object()
            .ok_or_else(|| JsError::Call("function value is not object".into()))?
            .clone();

        // Convert args.
        let mut js_args = Vec::with_capacity(args.len());
        for a in args {
            js_args.push(self.to_boajs_value(a)?);
        }

        // Use global object as `this`.
        let this = BoaJsValue::new(self.context.global_object().clone());
        let res = func_obj.call(&this, &js_args, &mut self.context);

        match res {
            Ok(v) => self.from_boajs_value(&v),
            Err(e) => Err(JsError::Call(e.to_string())),
        }
    }
}
