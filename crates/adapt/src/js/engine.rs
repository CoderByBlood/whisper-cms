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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js::value::JsValue;
    use std::collections::HashMap;

    fn assert_number(v: &JsValue, expected: f64) {
        match v {
            JsValue::Number(n) => {
                let diff = (n - expected).abs();
                assert!(
                    diff < 1e-9,
                    "expected number {expected}, got {n} (diff {diff})"
                );
            }
            other => panic!("expected JsValue::Number, got {:?}", other),
        }
    }

    #[test]
    fn eval_simple_number_expression() {
        let mut engine = BoaEngine::new();
        let result = engine.eval("1 + 2").expect("eval should succeed");
        assert_number(&result, 3.0);
    }

    #[test]
    fn eval_primitive_values() {
        let mut engine = BoaEngine::new();

        // Bool
        let v = engine.eval("true").expect("eval bool");
        assert_eq!(v, JsValue::Bool(true));

        // String
        let v = engine.eval("'hello'").expect("eval string");
        assert_eq!(v, JsValue::String("hello".into()));

        // Null
        let v = engine.eval("null").expect("eval null");
        assert_eq!(v, JsValue::Null);
    }

    #[test]
    fn eval_object_and_array_literal() {
        let mut engine = BoaEngine::new();
        // Parentheses so it's treated as an expression, not a block.
        let code = r#"
            ({
                a: 1,
                b: true,
                c: "hi",
                d: [1, 2, 3]
            })
        "#;

        let v = engine.eval(code).expect("eval object literal");

        let obj = match v {
            JsValue::Object(map) => map,
            other => panic!("expected object, got {:?}", other),
        };

        assert_number(obj.get("a").expect("key a missing"), 1.0);
        assert_eq!(obj.get("b"), Some(&JsValue::Bool(true)));
        assert_eq!(obj.get("c"), Some(&JsValue::String("hi".into())));

        match obj.get("d") {
            Some(JsValue::Array(arr)) => {
                assert_eq!(arr.len(), 3);
                assert_number(&arr[0], 1.0);
                assert_number(&arr[1], 2.0);
                assert_number(&arr[2], 3.0);
            }
            other => panic!("expected array for d, got {:?}", other),
        }
    }

    #[test]
    fn eval_syntax_error_returns_err() {
        let mut engine = BoaEngine::new();
        let result = engine.eval("let =");
        assert!(result.is_err(), "expected syntax error, got {:?}", result);
        if let Err(JsError::Eval(msg)) = result {
            assert!(
                msg.to_lowercase().contains("syntax"),
                "expected syntax-related message, got: {msg}"
            );
        }
    }

    #[test]
    fn load_module_and_call_exported_function() {
        let mut engine = BoaEngine::new();

        let module_src = r#"
            globalThis.plugin = {
                add: (a, b) => a + b,
            };
        "#;
        engine
            .load_module("plugin", module_src)
            .expect("load_module should succeed");

        let res = engine
            .call_function("plugin.add", &[JsValue::number(2.0), JsValue::number(3.0)])
            .expect("call_function should succeed");

        assert_number(&res, 5.0);
    }

    #[test]
    fn load_module_with_syntax_error_returns_err() {
        let mut engine = BoaEngine::new();

        let bad_src = r#"
            globalThis.plugin = {
                bad: () => { let =; }
            };
        "#;

        let res = engine.load_module("bad_plugin", bad_src);
        assert!(res.is_err(), "expected load_module error, got {:?}", res);
        if let Err(JsError::Eval(msg)) = res {
            assert!(
                msg.to_lowercase().contains("syntax") || msg.to_lowercase().contains("parse"),
                "expected syntax/parse-related message, got: {msg}"
            );
        }
    }

    #[test]
    fn call_function_passes_and_returns_complex_value() {
        let mut engine = BoaEngine::new();

        let module_src = r#"
            globalThis.id = (x) => x;
        "#;
        engine
            .load_module("id_module", module_src)
            .expect("load_module should succeed");

        // Build a nested JsValue object/array.
        let mut inner_obj = HashMap::new();
        inner_obj.insert("x".to_string(), JsValue::number(42.0));
        inner_obj.insert("flag".to_string(), JsValue::bool(true));

        let arg = JsValue::array(vec![
            JsValue::string("first"),
            JsValue::object(inner_obj.clone()),
        ]);

        let result = engine
            .call_function("id", &[arg.clone()])
            .expect("call_function should succeed");

        assert_eq!(result, arg);
    }

    #[test]
    fn call_function_with_empty_path_is_error() {
        let mut engine = BoaEngine::new();
        let res = engine.call_function("", &[]);
        assert!(res.is_err(), "expected error for empty path");

        if let Err(JsError::Call(msg)) = res {
            assert!(
                msg.to_lowercase().contains("empty"),
                "expected message mentioning 'empty', got: {msg}"
            );
        }
    }

    #[test]
    fn call_function_missing_property_is_error() {
        let mut engine = BoaEngine::new();

        // No module or global function set up -> path resolution should fail.
        let res = engine.call_function("does.not.exist", &[]);
        assert!(res.is_err(), "expected error for missing function path");

        if let Err(JsError::Call(msg)) = res {
            assert!(
                msg.to_lowercase().contains("object") || msg.to_lowercase().contains("undefined"),
                "expected message about non-object/undefined, got: {msg}"
            );
        }
    }

    #[test]
    fn call_function_target_is_not_a_function() {
        let mut engine = BoaEngine::new();

        // Set a non-function value on globalThis.
        engine
            .eval("globalThis.notAFunction = 123;")
            .expect("setup should succeed");

        let res = engine.call_function("notAFunction", &[]);
        assert!(res.is_err(), "expected error for non-function target");

        if let Err(JsError::Call(msg)) = res {
            assert!(
                msg.to_lowercase().contains("function value is not object")
                    || msg.to_lowercase().contains("not object"),
                "unexpected error message: {msg}"
            );
        }
    }

    #[test]
    fn call_function_that_throws_propagates_error() {
        let mut engine = BoaEngine::new();

        let src = r#"
            globalThis.boom = () => {
                throw new Error("boom!");
            };
        "#;
        engine
            .load_module("boom_module", src)
            .expect("load_module should succeed");

        let res = engine.call_function("boom", &[]);
        assert!(res.is_err(), "expected error from thrown JS exception");

        if let Err(JsError::Call(msg)) = res {
            assert!(
                msg.to_lowercase().contains("boom"),
                "expected error mentioning 'boom', got: {msg}"
            );
        }
    }
}
