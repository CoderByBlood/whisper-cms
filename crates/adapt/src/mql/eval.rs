// crates/adapt/src/mql/eval.rs

use serde_json::Value as Json;

use crate::mql::ast::{CmpOp, FieldExpr, Filter};

/// Resolve a dotted field path (e.g. "front_matter.tags") into a nested JSON value.
///
/// Returns `None` if any segment is missing.
fn field_value<'a>(doc: &'a Json, path: &str) -> Option<&'a Json> {
    let mut current = doc;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Public accessor used by other modules (e.g. index.rs).
pub fn get_field_value<'a>(doc: &'a Json, path: &str) -> Option<&'a Json> {
    field_value(doc, path)
}

/// Evaluate a single comparison operator against an optional JSON value.
fn eval_cmp(op: &CmpOp, actual: Option<&Json>) -> bool {
    use CmpOp::*;

    match op {
        // { field: { $eq: value } }
        Eq(expected) => match actual {
            Some(actual) => actual == expected,
            None => false,
        },

        // { field: { $ne: value } }
        Ne(expected) => match actual {
            Some(actual) => actual != expected,
            None => true,
        },

        // { field: { $gt: value } }
        Gt(expected) => match (actual, expected) {
            (Some(Json::Number(a)), Json::Number(b)) => match (a.as_f64(), b.as_f64()) {
                (Some(av), Some(bv)) => av > bv,
                _ => false,
            },
            (Some(Json::String(a)), Json::String(b)) => a > b,
            _ => false,
        },

        // { field: { $gte: value } }
        Gte(expected) => match (actual, expected) {
            (Some(Json::Number(a)), Json::Number(b)) => match (a.as_f64(), b.as_f64()) {
                (Some(av), Some(bv)) => av >= bv,
                _ => false,
            },
            (Some(Json::String(a)), Json::String(b)) => a >= b,
            _ => false,
        },

        // { field: { $lt: value } }
        Lt(expected) => match (actual, expected) {
            (Some(Json::Number(a)), Json::Number(b)) => match (a.as_f64(), b.as_f64()) {
                (Some(av), Some(bv)) => av < bv,
                _ => false,
            },
            (Some(Json::String(a)), Json::String(b)) => a < b,
            _ => false,
        },

        // { field: { $lte: value } }
        Lte(expected) => match (actual, expected) {
            (Some(Json::Number(a)), Json::Number(b)) => match (a.as_f64(), b.as_f64()) {
                (Some(av), Some(bv)) => av <= bv,
                _ => false,
            },
            (Some(Json::String(a)), Json::String(b)) => a <= b,
            _ => false,
        },

        // { field: { $in: [v1, v2, ...] } }
        In(list) => match actual {
            Some(actual) => list.iter().any(|v| actual == v),
            None => false,
        },

        // { field: { $nin: [v1, v2, ...] } }
        Nin(list) => match actual {
            Some(actual) => !list.iter().any(|v| actual == v),
            None => true,
        },

        // { field: { $all: [v1, v2, ...] } } for array fields
        All(values) => match actual {
            Some(Json::Array(arr)) => values.iter().all(|v| arr.contains(v)),
            _ => false,
        },

        // { field: { $exists: true|false } }
        Exists(flag) => match (flag, actual) {
            (true, Some(_)) => true,
            (true, None) => false,
            (false, Some(_)) => false,
            (false, None) => true,
        },

        // { field: { $size: n } } for arrays or strings
        Size(expected_len) => match actual {
            Some(Json::Array(arr)) => arr.len() as i64 == *expected_len,
            Some(Json::String(s)) => s.len() as i64 == *expected_len,
            _ => false,
        },

        // { field: { $not: { <cmp expr> } } }
        Not(inner) => {
            // `inner` is a FieldExpr over the *same* field; we pass the already
            // resolved value down to it.
            eval_field_expr(inner, actual).map(|v| !v).unwrap_or(true)
        }
    }
}

/// Evaluate a single field expression given an already-resolved JSON value.
///
/// This is mostly useful for `$not` where we re-use the resolved value.
fn eval_field_expr(expr: &FieldExpr, actual: Option<&Json>) -> Option<bool> {
    Some(eval_cmp(&expr.op, actual))
}

/// Evaluate a full Filter against a document.
pub fn eval_filter(filter: &Filter, doc: &Json) -> bool {
    use Filter::*;

    match filter {
        Field(expr) => {
            let val = field_value(doc, &expr.path);
            eval_cmp(&expr.op, val)
        }
        And(filters) => filters.iter().all(|f| eval_filter(f, doc)),
        Or(filters) => filters.iter().any(|f| eval_filter(f, doc)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mql::ast::{CmpOp, FieldExpr, Filter};
    use serde_json::json;

    // ─────────────────────────────────────────────────────────────
    // field_value / get_field_value
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn get_field_value_top_level_and_nested() {
        let doc = json!({
            "kind": "post",
            "front_matter": {
                "title": "Hello",
                "tags": ["rust", "cms"]
            }
        });

        // top level
        let v = get_field_value(&doc, "kind").unwrap();
        assert_eq!(v, &json!("post"));

        // nested
        let v = get_field_value(&doc, "front_matter.title").unwrap();
        assert_eq!(v, &json!("Hello"));

        // path into array (JSON index as string key – should fail)
        assert!(get_field_value(&doc, "front_matter.tags.0").is_none());

        // missing field
        assert!(get_field_value(&doc, "does_not_exist").is_none());
        assert!(get_field_value(&doc, "front_matter.missing").is_none());
    }

    // ─────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────

    fn field_filter(path: &str, op: CmpOp) -> Filter {
        Filter::Field(FieldExpr {
            path: path.to_string(),
            op,
        })
    }

    // ─────────────────────────────────────────────────────────────
    // Eq / Ne
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn eval_eq_and_ne_basic() {
        let doc = json!({ "status": "published", "views": 10 });

        // Eq true
        let f = field_filter("status", CmpOp::Eq(json!("published")));
        assert!(eval_filter(&f, &doc));

        // Eq false
        let f = field_filter("status", CmpOp::Eq(json!("draft")));
        assert!(!eval_filter(&f, &doc));

        // Eq on missing field -> false
        let f = field_filter("missing", CmpOp::Eq(json!(1)));
        assert!(!eval_filter(&f, &doc));

        // Ne true
        let f = field_filter("status", CmpOp::Ne(json!("draft")));
        assert!(eval_filter(&f, &doc));

        // Ne false
        let f = field_filter("status", CmpOp::Ne(json!("published")));
        assert!(!eval_filter(&f, &doc));

        // Ne on missing field -> true
        let f = field_filter("missing", CmpOp::Ne(json!(1)));
        assert!(eval_filter(&f, &doc));
    }

    // ─────────────────────────────────────────────────────────────
    // Gt / Gte / Lt / Lte (numbers + strings)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn eval_numeric_comparisons() {
        let doc = json!({ "n": 10 });

        // n > 5
        assert!(eval_filter(&field_filter("n", CmpOp::Gt(json!(5))), &doc));

        // n > 10 -> false
        assert!(!eval_filter(&field_filter("n", CmpOp::Gt(json!(10))), &doc));

        // n >= 10
        assert!(eval_filter(&field_filter("n", CmpOp::Gte(json!(10))), &doc));

        // n < 20
        assert!(eval_filter(&field_filter("n", CmpOp::Lt(json!(20))), &doc));

        // n <= 10
        assert!(eval_filter(&field_filter("n", CmpOp::Lte(json!(10))), &doc));

        // mismatched types: doc number vs expected string -> false
        assert!(!eval_filter(
            &field_filter("n", CmpOp::Gt(json!("not-number"))),
            &doc
        ));

        // missing field -> false
        assert!(!eval_filter(
            &field_filter("missing", CmpOp::Gt(json!(1))),
            &doc
        ));
    }

    #[test]
    fn eval_string_comparisons() {
        let doc = json!({ "s": "mango" });

        // lexicographic behavior
        assert!(eval_filter(
            &field_filter("s", CmpOp::Gt(json!("apple"))),
            &doc
        ));
        assert!(eval_filter(
            &field_filter("s", CmpOp::Gte(json!("mango"))),
            &doc
        ));
        assert!(!eval_filter(
            &field_filter("s", CmpOp::Lt(json!("kiwi"))),
            &doc
        ));

        // mismatched types: doc string vs expected number -> false
        assert!(!eval_filter(&field_filter("s", CmpOp::Gt(json!(1))), &doc));
    }

    // ─────────────────────────────────────────────────────────────
    // In / Nin
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn eval_in_and_nin_with_match() {
        let doc = json!({ "role": "admin" });

        let f_in = field_filter("role", CmpOp::In(vec![json!("admin"), json!("editor")]));
        assert!(eval_filter(&f_in, &doc));

        let f_nin = field_filter("role", CmpOp::Nin(vec![json!("guest"), json!("reader")]));
        assert!(eval_filter(&f_nin, &doc));
    }

    #[test]
    fn eval_in_and_nin_no_match_and_missing() {
        let doc = json!({ "role": "admin" });

        // In, no match
        let f_in = field_filter("role", CmpOp::In(vec![json!("guest")]));
        assert!(!eval_filter(&f_in, &doc));

        // In on missing field -> false
        let f_in_missing = field_filter("missing", CmpOp::In(vec![json!("x")]));
        assert!(!eval_filter(&f_in_missing, &doc));

        // Nin, value in list -> false
        let f_nin = field_filter("role", CmpOp::Nin(vec![json!("admin")]));
        assert!(!eval_filter(&f_nin, &doc));

        // Nin on missing field -> true
        let f_nin_missing = field_filter("missing", CmpOp::Nin(vec![json!("x")]));
        assert!(eval_filter(&f_nin_missing, &doc));
    }

    // ─────────────────────────────────────────────────────────────
    // All (arrays)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn eval_all_on_array() {
        let doc = json!({ "tags": ["rust", "cms", "static"] });

        // all present
        let f = field_filter("tags", CmpOp::All(vec![json!("rust"), json!("cms")]));
        assert!(eval_filter(&f, &doc));

        // one missing
        let f = field_filter("tags", CmpOp::All(vec![json!("rust"), json!("missing")]));
        assert!(!eval_filter(&f, &doc));

        // non-array actual -> false
        let doc2 = json!({ "tags": "rust" });
        let f = field_filter("tags", CmpOp::All(vec![json!("rust")]));
        assert!(!eval_filter(&f, &doc2));

        // missing field -> false
        let f = field_filter("missing", CmpOp::All(vec![json!("x")]));
        assert!(!eval_filter(&f, &doc));
    }

    #[test]
    fn eval_all_empty_values_is_vacuously_true_for_arrays() {
        let doc = json!({ "tags": ["rust", "cms"] });

        // All([]) should be true on an array (vacuously true)
        let f = field_filter("tags", CmpOp::All(vec![]));
        assert!(eval_filter(&f, &doc));

        // But false if field is not an array
        let doc2 = json!({ "tags": "rust" });
        assert!(!eval_filter(&f, &doc2));
    }

    // ─────────────────────────────────────────────────────────────
    // Exists
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn eval_exists_true_and_false() {
        let doc = json!({ "a": 1 });

        // field exists & flag true -> true
        let f = field_filter("a", CmpOp::Exists(true));
        assert!(eval_filter(&f, &doc));

        // field exists & flag false -> false
        let f = field_filter("a", CmpOp::Exists(false));
        assert!(!eval_filter(&f, &doc));

        // field missing & flag true -> false
        let f = field_filter("missing", CmpOp::Exists(true));
        assert!(!eval_filter(&f, &doc));

        // field missing & flag false -> true
        let f = field_filter("missing", CmpOp::Exists(false));
        assert!(eval_filter(&f, &doc));
    }

    // ─────────────────────────────────────────────────────────────
    // Size
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn eval_size_on_arrays_and_strings() {
        let doc = json!({
            "arr": [1, 2, 3],
            "str": "abcd",
            "num": 10
        });

        // array
        let f = field_filter("arr", CmpOp::Size(3));
        assert!(eval_filter(&f, &doc));
        let f = field_filter("arr", CmpOp::Size(2));
        assert!(!eval_filter(&f, &doc));

        // string
        let f = field_filter("str", CmpOp::Size(4));
        assert!(eval_filter(&f, &doc));
        let f = field_filter("str", CmpOp::Size(5));
        assert!(!eval_filter(&f, &doc));

        // non-array/string -> false
        let f = field_filter("num", CmpOp::Size(2));
        assert!(!eval_filter(&f, &doc));

        // missing -> false
        let f = field_filter("missing", CmpOp::Size(0));
        assert!(!eval_filter(&f, &doc));
    }

    // ─────────────────────────────────────────────────────────────
    // Not
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn eval_not_inverts_inner_expression() {
        let doc = json!({ "n": 10 });

        // Inner expr: n > 5 (true), then Not -> false
        let inner = FieldExpr {
            path: "n".to_string(),
            op: CmpOp::Gt(json!(5)),
        };
        let f = field_filter("n", CmpOp::Not(Box::new(inner)));
        assert!(!eval_filter(&f, &doc));

        // Inner expr: n < 5 (false), then Not -> true
        let inner = FieldExpr {
            path: "n".to_string(),
            op: CmpOp::Lt(json!(5)),
        };
        let f = field_filter("n", CmpOp::Not(Box::new(inner)));
        assert!(eval_filter(&f, &doc));
    }

    #[test]
    fn eval_not_with_missing_field_defaults_to_true() {
        let doc = json!({});

        // Inner expression would see `actual = None`.
        let inner = FieldExpr {
            path: "missing".to_string(),
            op: CmpOp::Eq(json!(1)),
        };
        let f = field_filter("missing", CmpOp::Not(Box::new(inner)));

        // eval_field_expr(inner, None) -> Some(false), so Not -> !false = true.
        assert!(eval_filter(&f, &doc));
    }

    // ─────────────────────────────────────────────────────────────
    // And / Or combinators
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn eval_and_or_combinations() {
        let doc = json!({
            "kind": "post",
            "status": "published",
            "views": 100
        });

        let f = Filter::And(vec![
            field_filter("kind", CmpOp::Eq(json!("post"))),
            Filter::Or(vec![
                field_filter("status", CmpOp::Eq(json!("draft"))),
                field_filter("views", CmpOp::Gt(json!(50))),
            ]),
        ]);

        // kind == post AND (status == draft OR views > 50)
        // -> true AND (false OR true) -> true
        assert!(eval_filter(&f, &doc));

        // If we require views > 200, expression becomes false.
        let f_false = Filter::And(vec![
            field_filter("kind", CmpOp::Eq(json!("post"))),
            Filter::Or(vec![
                field_filter("status", CmpOp::Eq(json!("draft"))),
                field_filter("views", CmpOp::Gt(json!(200))),
            ]),
        ]);
        assert!(!eval_filter(&f_false, &doc));
    }
}
