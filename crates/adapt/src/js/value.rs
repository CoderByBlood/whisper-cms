use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Engine-agnostic JS value representation.
///
/// Only JSON-like types are supported; no engine-specific objects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JsValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsValue>),
    Object(HashMap<String, JsValue>),
}

impl JsValue {
    pub fn null() -> Self {
        JsValue::Null
    }

    pub fn bool(b: bool) -> Self {
        JsValue::Bool(b)
    }

    pub fn number(n: f64) -> Self {
        JsValue::Number(n)
    }

    pub fn string<S: Into<String>>(s: S) -> Self {
        JsValue::String(s.into())
    }

    pub fn array(v: Vec<JsValue>) -> Self {
        JsValue::Array(v)
    }

    pub fn object(map: HashMap<String, JsValue>) -> Self {
        JsValue::Object(map)
    }

    /// Convert from serde_json::Value to JsValue.
    pub fn from_json(v: &serde_json::Value) -> Self {
        use serde_json::Value as J;
        match v {
            J::Null => JsValue::Null,
            J::Bool(b) => JsValue::Bool(*b),
            J::Number(n) => JsValue::Number(n.as_f64().unwrap_or(0.0)),
            J::String(s) => JsValue::String(s.clone()),
            J::Array(arr) => JsValue::Array(arr.iter().map(JsValue::from_json).collect()),
            J::Object(obj) => {
                let mut map = HashMap::new();
                for (k, v) in obj {
                    map.insert(k.clone(), JsValue::from_json(v));
                }
                JsValue::Object(map)
            }
        }
    }

    /// Convert JsValue back to serde_json::Value.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            JsValue::Null => J::Null,
            JsValue::Bool(b) => J::Bool(*b),
            JsValue::Number(n) => J::Number(
                serde_json::Number::from_f64(*n).unwrap_or_else(|| serde_json::Number::from(0)),
            ),
            JsValue::String(s) => J::String(s.clone()),
            JsValue::Array(arr) => J::Array(arr.iter().map(|v| v.to_json()).collect()),
            JsValue::Object(map) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in map {
                    obj.insert(k.clone(), v.to_json());
                }
                J::Object(obj)
            }
        }
    }
}
