// crates/adapt/src/mql/parser.rs

use super::ast::{CmpOp, FieldExpr, Filter, FindOptions};
use crate::Error;
use serde_json::Value as Json;

/// Parse a Mongo-style JSON filter into a Filter AST.
pub fn parse_filter(json: &Json) -> Result<Filter, Error> {
    match json {
        Json::Object(map) => {
            // top-level: fields & logical operators
            let mut filters = Vec::new();

            for (k, v) in map {
                if k == "$and" {
                    filters.push(parse_and(v)?);
                } else if k == "$or" {
                    filters.push(parse_or(v)?);
                } else {
                    // field expression, implicit $eq or operator object
                    filters.push(parse_field_expr(k, v)?);
                }
            }

            if filters.is_empty() {
                Ok(Filter::And(vec![]))
            } else if filters.len() == 1 {
                Ok(filters.remove(0))
            } else {
                Ok(Filter::And(filters))
            }
        }
        _ => Err(Error::InvalidFilter(
            "top-level filter must be an object".into(),
        )),
    }
}

fn parse_and(value: &Json) -> Result<Filter, Error> {
    match value {
        Json::Array(arr) => {
            let mut filters = Vec::new();
            for sub in arr {
                filters.push(parse_filter(sub)?);
            }
            Ok(Filter::And(filters))
        }
        _ => Err(Error::InvalidFilter("$and value must be an array".into())),
    }
}

fn parse_or(value: &Json) -> Result<Filter, Error> {
    match value {
        Json::Array(arr) => {
            let mut filters = Vec::new();
            for sub in arr {
                filters.push(parse_filter(sub)?);
            }
            Ok(Filter::Or(filters))
        }
        _ => Err(Error::InvalidFilter("$or value must be an array".into())),
    }
}

fn parse_field_expr(path: &str, v: &Json) -> Result<Filter, Error> {
    // Shorthand: { field: value } → Eq
    if !v.is_object() {
        let op = CmpOp::Eq(v.clone());
        return Ok(Filter::Field(FieldExpr {
            path: path.to_string(),
            op,
        }));
    }

    let obj = v.as_object().unwrap();
    if obj.is_empty() {
        return Err(Error::InvalidFilter(format!(
            "empty operator object for field {}",
            path
        )));
    }

    let mut and_ops = Vec::new();

    for (op_name, op_val) in obj {
        let cmp = parse_cmp_op(path, op_name, op_val)?;
        and_ops.push(Filter::Field(FieldExpr {
            path: path.to_string(),
            op: cmp,
        }));
    }

    if and_ops.len() == 1 {
        Ok(and_ops.remove(0))
    } else {
        Ok(Filter::And(and_ops))
    }
}

fn parse_cmp_op(_path: &str, op_name: &str, value: &Json) -> Result<CmpOp, Error> {
    use CmpOp::*;

    match op_name {
        "$eq" => Ok(Eq(value.clone())),
        "$ne" => Ok(Ne(value.clone())),
        "$gt" => Ok(Gt(value.clone())),
        "$gte" => Ok(Gte(value.clone())),
        "$lt" => Ok(Lt(value.clone())),
        "$lte" => Ok(Lte(value.clone())),
        "$in" => {
            let arr = value
                .as_array()
                .ok_or_else(|| Error::InvalidFilter("$in expects array".into()))?;
            Ok(In(arr.clone()))
        }
        "$nin" => {
            let arr = value
                .as_array()
                .ok_or_else(|| Error::InvalidFilter("$nin expects array".into()))?;
            Ok(Nin(arr.clone()))
        }
        "$all" => {
            let arr = value
                .as_array()
                .ok_or_else(|| Error::InvalidFilter("$all expects array".into()))?;
            Ok(All(arr.clone()))
        }
        "$exists" => {
            let b = value
                .as_bool()
                .ok_or_else(|| Error::InvalidFilter("$exists expects boolean".into()))?;
            Ok(Exists(b))
        }
        "$size" => {
            let n = match value {
                Json::Number(num) => num
                    .as_i64()
                    .ok_or_else(|| Error::InvalidFilter("$size expects integer".into()))?,
                _ => return Err(Error::InvalidFilter("$size expects integer".into())),
            };
            Ok(Size(n))
        }
        "$not" => {
            // $not value is a single-field expression object
            let inner_obj = value
                .as_object()
                .ok_or_else(|| Error::InvalidFilter("$not expects object".into()))?;
            if inner_obj.len() != 1 {
                return Err(Error::InvalidFilter(
                    "$not expects a single operator object".into(),
                ));
            }
            let (inner_op, inner_val) = inner_obj.iter().next().unwrap();
            let inner_cmp = parse_cmp_op("", inner_op, inner_val)?;
            let field_expr = FieldExpr {
                path: "".to_string(), // path not used in Not; will be filled by caller if needed
                op: inner_cmp,
            };
            Ok(Not(Box::new(field_expr)))
        }
        _ => Err(Error::InvalidOperator(format!(
            "unsupported operator {}",
            op_name
        ))),
    }
}

/// Parse FindOptions from a JSON object (typically coming from JS).
///
/// {
///   sort: { "field": 1, "other": -1 },
///   limit: 10,
///   skip: 5
/// }
pub fn parse_find_options(json: &Json) -> Result<FindOptions, Error> {
    let mut opts = FindOptions::default();

    let obj = match json {
        Json::Object(m) => m,
        _ => return Ok(opts),
    };

    // sort
    if let Some(sort_val) = obj.get("sort") {
        if let Some(sort_obj) = sort_val.as_object() {
            let mut sort_vec = Vec::new();
            for (field, dir_val) in sort_obj {
                let dir = match dir_val {
                    Json::Number(n) => n.as_i64().ok_or_else(|| {
                        Error::InvalidSort("sort direction must be 1 or -1".into())
                    })? as i8,
                    _ => return Err(Error::InvalidSort("sort direction must be 1 or -1".into())),
                };
                if dir != 1 && dir != -1 {
                    return Err(Error::InvalidSort("sort direction must be 1 or -1".into()));
                }
                sort_vec.push((field.clone(), dir));
            }
            opts.sort = sort_vec;
        } else {
            return Err(Error::InvalidSort(
                "sort must be object { field: 1|-1 }".into(),
            ));
        }
    }

    // limit
    if let Some(limit_val) = obj.get("limit") {
        if let Some(n) = limit_val.as_i64() {
            if n > 0 {
                opts.limit = Some(n as usize);
            }
        }
    }

    // skip
    if let Some(skip_val) = obj.get("skip") {
        if let Some(n) = skip_val.as_i64() {
            if n > 0 {
                opts.skip = Some(n as usize);
            }
        }
    }

    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mql::ast::{CmpOp, FieldExpr, Filter};
    use serde_json::json;

    // Helper to extract a single FieldExpr from a Filter::Field, panic otherwise.
    fn as_field(filter: &Filter) -> &FieldExpr {
        match filter {
            Filter::Field(fe) => fe,
            other => panic!("expected Filter::Field, got: {:?}", other),
        }
    }

    // ─────────────────────────────────────────────────────────────
    // parse_filter – basic / implicit $eq
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_filter_implicit_eq_single_field() {
        let json = json!({ "status": "published" });
        let f = parse_filter(&json).expect("parse_filter failed");

        let fe = as_field(&f);
        assert_eq!(fe.path, "status");
        match &fe.op {
            CmpOp::Eq(v) => assert_eq!(v, &json!("published")),
            other => panic!("expected CmpOp::Eq, got: {:?}", other),
        }
    }

    #[test]
    fn parse_filter_multiple_implicit_eq_becomes_and() {
        let json = json!({ "status": "published", "kind": "post" });
        let f = parse_filter(&json).expect("parse_filter failed");

        match f {
            Filter::And(filters) => {
                assert_eq!(filters.len(), 2);

                let fe1 = as_field(&filters[0]);
                let fe2 = as_field(&filters[1]);
                let paths = vec![fe1.path.clone(), fe2.path.clone()];
                assert!(paths.contains(&"status".to_string()));
                assert!(paths.contains(&"kind".to_string()));
            }
            other => panic!("expected top-level And, got: {:?}", other),
        }
    }

    #[test]
    fn parse_filter_top_level_must_be_object() {
        let json = json!(["not-an-object"]);
        let err = parse_filter(&json).unwrap_err();
        matches!(err, Error::InvalidFilter(_));
    }

    // ─────────────────────────────────────────────────────────────
    // $and / $or handling
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_filter_and_or_nested() {
        let json = json!({
            "$and": [
                { "status": "published" },
                {
                    "$or": [
                        { "kind": "post" },
                        { "kind": "page" }
                    ]
                }
            ]
        });

        let f = parse_filter(&json).expect("parse_filter failed");

        match f {
            Filter::And(and_list) => {
                assert_eq!(and_list.len(), 2);

                // First child should be a simple Field filter on status
                let fe = as_field(&and_list[0]);
                assert_eq!(fe.path, "status");
                matches!(&fe.op, CmpOp::Eq(v) if v == "published");

                // Second child should be Or
                match &and_list[1] {
                    Filter::Or(or_list) => {
                        assert_eq!(or_list.len(), 2);
                    }
                    other => panic!("expected Or inside And[1], got: {:?}", other),
                }
            }
            other => panic!("expected top-level And, got: {:?}", other),
        }
    }

    #[test]
    fn parse_and_value_must_be_array() {
        let json = json!({ "$and": { "status": "published" } });
        let err = parse_filter(&json).unwrap_err();
        matches!(err, Error::InvalidFilter(_));
    }

    #[test]
    fn parse_or_value_must_be_array() {
        let json = json!({ "$or": { "status": "published" } });
        let err = parse_filter(&json).unwrap_err();
        matches!(err, Error::InvalidFilter(_));
    }

    // ─────────────────────────────────────────────────────────────
    // field expressions with operators
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_field_expr_single_operator_stays_field() {
        let json = json!({
            "views": { "$gt": 10 }
        });

        let f = parse_filter(&json).expect("parse_filter failed");

        let fe = as_field(&f);
        assert_eq!(fe.path, "views");
        match &fe.op {
            CmpOp::Gt(v) => assert_eq!(v, &json!(10)),
            other => panic!("expected CmpOp::Gt, got: {:?}", other),
        }
    }

    #[test]
    fn parse_field_expr_multiple_operators_become_and_of_fields() {
        let json = json!({
            "views": { "$gt": 10, "$lt": 100 }
        });

        let f = parse_filter(&json).expect("parse_filter failed");

        match f {
            Filter::And(filters) => {
                assert_eq!(filters.len(), 2);
                for sub in filters {
                    let fe = as_field(&sub);
                    assert_eq!(fe.path, "views");
                    match fe.op {
                        CmpOp::Gt(_) | CmpOp::Lt(_) => {}
                        ref other => panic!("expected Gt or Lt, got: {:?}", other),
                    }
                }
            }
            other => panic!("expected And for multiple ops, got: {:?}", other),
        }
    }

    #[test]
    fn parse_field_expr_empty_operator_object_is_error() {
        let json = json!({
            "status": {}
        });

        let err = parse_filter(&json).unwrap_err();
        matches!(err, Error::InvalidFilter(_));
    }

    // ─────────────────────────────────────────────────────────────
    // Specific operators: $in, $nin, $all, $exists, $size, $not
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_in_and_nin_and_all_with_arrays() {
        let json = json!({
            "tags": {
                "$in": ["rust", "cms"],
                "$nin": ["old"],
                "$all": ["rust"]
            }
        });

        let f = parse_filter(&json).expect("parse_filter failed");

        match f {
            Filter::And(filters) => {
                // We don't care about order, but we expect 3 filters.
                assert_eq!(filters.len(), 3);

                let mut seen_in = false;
                let mut seen_nin = false;
                let mut seen_all = false;

                for sub in filters {
                    let fe = as_field(&sub);
                    assert_eq!(fe.path, "tags");
                    match &fe.op {
                        CmpOp::In(vals) => {
                            seen_in = true;
                            assert_eq!(vals.len(), 2);
                        }
                        CmpOp::Nin(vals) => {
                            seen_nin = true;
                            assert_eq!(vals.len(), 1);
                        }
                        CmpOp::All(vals) => {
                            seen_all = true;
                            assert_eq!(vals.len(), 1);
                        }
                        other => panic!("unexpected op in tags: {:?}", other),
                    }
                }

                assert!(seen_in && seen_nin && seen_all);
            }
            other => panic!("expected And, got: {:?}", other),
        }
    }

    #[test]
    fn parse_in_nin_all_require_array() {
        let bad_in = json!({ "tags": { "$in": "not-array" } });
        let err = parse_filter(&bad_in).unwrap_err();
        matches!(err, Error::InvalidFilter(_));

        let bad_nin = json!({ "tags": { "$nin": 123 } });
        let err = parse_filter(&bad_nin).unwrap_err();
        matches!(err, Error::InvalidFilter(_));

        let bad_all = json!({ "tags": { "$all": { "x": 1 } } });
        let err = parse_filter(&bad_all).unwrap_err();
        matches!(err, Error::InvalidFilter(_));
    }

    #[test]
    fn parse_exists_requires_boolean() {
        let ok = json!({ "flag": { "$exists": true } });
        let _ = parse_filter(&ok).expect("parse_filter failed for $exists true");

        let bad = json!({ "flag": { "$exists": 1 } });
        let err = parse_filter(&bad).unwrap_err();
        matches!(err, Error::InvalidFilter(_));
    }

    #[test]
    fn parse_size_requires_integer_number() {
        let ok = json!({ "arr": { "$size": 3 } });
        let _ = parse_filter(&ok).expect("parse_filter failed for $size int");

        let bad_float = json!({ "arr": { "$size": 3.5 } });
        let err = parse_filter(&bad_float).unwrap_err();
        matches!(err, Error::InvalidFilter(_));

        let bad_type = json!({ "arr": { "$size": "not-int" } });
        let err = parse_filter(&bad_type).unwrap_err();
        matches!(err, Error::InvalidFilter(_));
    }

    #[test]
    fn parse_not_with_single_inner_operator() {
        let json = json!({
            "views": {
                "$not": { "$gt": 10 }
            }
        });

        let f = parse_filter(&json).expect("parse_filter failed");

        let fe = as_field(&f);
        assert_eq!(fe.path, "views");

        match &fe.op {
            CmpOp::Not(inner) => {
                // inner.path is "" per implementation; inner.op should be Gt.
                assert_eq!(inner.path, "");
                matches!(inner.op, CmpOp::Gt(_));
            }
            other => panic!("expected CmpOp::Not, got: {:?}", other),
        }
    }

    #[test]
    fn parse_not_requires_object_with_single_operator() {
        // non-object
        let bad = json!({ "views": { "$not": 123 } });
        let err = parse_filter(&bad).unwrap_err();
        matches!(err, Error::InvalidFilter(_));

        // multiple operators inside $not
        let bad_multi = json!({
            "views": {
                "$not": { "$gt": 10, "$lt": 100 }
            }
        });
        let err = parse_filter(&bad_multi).unwrap_err();
        matches!(err, Error::InvalidFilter(_));
    }

    #[test]
    fn parse_unknown_operator_is_invalid_operator() {
        let json = json!({
            "field": { "$weird": 1 }
        });

        let err = parse_filter(&json).unwrap_err();
        matches!(err, Error::InvalidOperator(_));
    }

    // ─────────────────────────────────────────────────────────────
    // parse_find_options
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_find_options_defaults_when_not_object() {
        let json = json!("not-object");
        let opts = parse_find_options(&json).expect("parse_find_options failed");
        assert!(opts.sort.is_empty());
        assert!(opts.limit.is_none());
        assert!(opts.skip.is_none());
    }

    #[test]
    fn parse_find_options_valid_sort_limit_skip() {
        let json = json!({
            "sort": { "a": 1, "b": -1 },
            "limit": 10,
            "skip": 5
        });

        let opts = parse_find_options(&json).expect("parse_find_options failed");

        // sort
        assert_eq!(opts.sort.len(), 2);
        assert!(opts.sort.contains(&(String::from("a"), 1)));
        assert!(opts.sort.contains(&(String::from("b"), -1)));

        // limit/skip
        assert_eq!(opts.limit, Some(10));
        assert_eq!(opts.skip, Some(5));
    }

    #[test]
    fn parse_find_options_sort_must_be_object() {
        let json = json!({
            "sort": ["not", "object"]
        });

        let err = parse_find_options(&json).unwrap_err();
        matches!(err, Error::InvalidSort(_));
    }

    #[test]
    fn parse_find_options_sort_dir_must_be_number_1_or_minus_1() {
        // non-number
        let json = json!({
            "sort": { "a": "asc" }
        });
        let err = parse_find_options(&json).unwrap_err();
        matches!(err, Error::InvalidSort(_));

        // number but not 1/-1
        let json = json!({
            "sort": { "a": 2 }
        });
        let err = parse_find_options(&json).unwrap_err();
        matches!(err, Error::InvalidSort(_));
    }

    #[test]
    fn parse_find_options_limit_and_skip_ignore_non_positive() {
        let json = json!({
            "limit": 0,
            "skip": -1
        });

        let opts = parse_find_options(&json).expect("parse_find_options failed");
        assert!(opts.limit.is_none());
        assert!(opts.skip.is_none());
    }

    #[test]
    fn parse_find_options_limit_and_skip_ignore_non_numeric() {
        let json = json!({
            "limit": "10",
            "skip": "5"
        });

        let opts = parse_find_options(&json).expect("parse_find_options failed");
        assert!(opts.limit.is_none());
        assert!(opts.skip.is_none());
    }
}
