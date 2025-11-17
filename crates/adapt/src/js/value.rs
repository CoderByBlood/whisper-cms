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
            JsValue::Number(n) => {
                // Prefer preserving integer-ness when possible.
                if n.is_finite() {
                    let rounded = n.round();
                    // Close enough to an integer and within i64 range
                    if (n - rounded).abs() < 1e-9
                        && rounded >= i64::MIN as f64
                        && rounded <= i64::MAX as f64
                    {
                        J::Number(serde_json::Number::from(rounded as i64))
                    } else if let Some(num) = serde_json::Number::from_f64(*n) {
                        J::Number(num)
                    } else {
                        // Extremely large magnitude that can't be represented as JSON number
                        J::Null
                    }
                } else {
                    // NaN / +/- Infinity -> not representable in JSON
                    J::Null
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serde_json::Value as J;
    use std::collections::HashMap;

    #[test]
    fn constructors_create_expected_variants() {
        assert_eq!(JsValue::null(), JsValue::Null);
        assert_eq!(JsValue::bool(true), JsValue::Bool(true));
        assert_eq!(JsValue::number(1.5), JsValue::Number(1.5));
        assert_eq!(JsValue::string("hi"), JsValue::String("hi".to_string()));

        let arr = JsValue::array(vec![JsValue::Bool(false)]);
        assert!(matches!(arr, JsValue::Array(v) if v == vec![JsValue::Bool(false)]));

        let mut map = HashMap::new();
        map.insert("k".to_string(), JsValue::Null);
        let obj = JsValue::object(map.clone());
        assert!(matches!(obj, JsValue::Object(m) if m == map));
    }

    #[test]
    fn from_json_primitives() {
        assert_eq!(JsValue::from_json(&J::Null), JsValue::Null);
        assert_eq!(JsValue::from_json(&J::Bool(true)), JsValue::Bool(true));
        assert_eq!(JsValue::from_json(&json!(1)), JsValue::Number(1.0));
        assert_eq!(
            JsValue::from_json(&J::String("abc".into())),
            JsValue::String("abc".into())
        );
    }

    #[test]
    fn from_json_nested_structures() {
        let input = json!({
            "a": 1,
            "b": [true, false, null],
            "c": { "d": "x" }
        });

        let js = JsValue::from_json(&input);

        match js {
            JsValue::Object(mut m) => {
                assert!(
                    matches!(m.remove("a"), Some(JsValue::Number(n)) if (n - 1.0).abs() < 1e-9)
                );
                assert!(matches!(m.remove("b"), Some(JsValue::Array(_))));
                assert!(matches!(m.remove("c"), Some(JsValue::Object(_))));
                assert!(m.is_empty());
            }
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn to_json_primitives() {
        assert_eq!(JsValue::Null.to_json(), J::Null);
        assert_eq!(JsValue::Bool(true).to_json(), J::Bool(true));
        assert_eq!(JsValue::String("x".into()).to_json(), J::String("x".into()));
    }

    #[test]
    fn to_json_preserves_integers_when_possible() {
        let v = JsValue::Number(1.0);
        assert_eq!(v.to_json(), json!(1));

        let v = JsValue::Number(-42.0);
        assert_eq!(v.to_json(), json!(-42));
    }

    #[test]
    fn to_json_handles_non_integer_floats() {
        let v = JsValue::Number(1.5);
        assert_eq!(v.to_json(), json!(1.5));

        let v = JsValue::Number(-3.75);
        assert_eq!(v.to_json(), json!(-3.75));
    }

    #[test]
    fn to_json_non_finite_numbers_become_null() {
        let v = JsValue::Number(f64::NAN);
        assert_eq!(v.to_json(), J::Null);

        let v = JsValue::Number(f64::INFINITY);
        assert_eq!(v.to_json(), J::Null);

        let v = JsValue::Number(f64::NEG_INFINITY);
        assert_eq!(v.to_json(), J::Null);
    }

    #[test]
    fn round_trip_json_to_jsvalue_to_json_simple() {
        let original = json!({
            "null": null,
            "bool": true,
            "num": 1.5,
            "arr": [1, 2, 3],
            "obj": { "x": 1, "y": false },
            "str": "hello"
        });

        let js = JsValue::from_json(&original);
        let back = js.to_json();

        assert_eq!(back, original);
    }

    #[test]
    fn round_trip_nested_structures() {
        let original = json!({
            "level1": {
                "level2": {
                    "list": [
                        { "a": 1 },
                        { "b": [true, false] }
                    ]
                }
            }
        });

        let js = JsValue::from_json(&original);
        let back = js.to_json();

        assert_eq!(back, original);
    }

    #[test]
    fn array_and_object_round_trip_through_json() {
        let mut inner_map = HashMap::new();
        inner_map.insert("k".to_string(), JsValue::Number(10.0));

        let value = JsValue::Array(vec![
            JsValue::Null,
            JsValue::Object(inner_map),
            JsValue::Bool(true),
        ]);

        let json = value.to_json();
        let back = JsValue::from_json(&json);

        assert_eq!(value, back);
    }
}
