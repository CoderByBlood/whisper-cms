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
