use super::ast::{CmpOp, FieldExpr, Filter, FindOptions};
use super::error::QueryError;
use serde_json::Value as Json;

/// Parse a Mongo-style JSON filter into a Filter AST.
pub fn parse_filter(json: &Json) -> Result<Filter, QueryError> {
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
        _ => Err(QueryError::InvalidFilter(
            "top-level filter must be an object".into(),
        )),
    }
}

fn parse_and(value: &Json) -> Result<Filter, QueryError> {
    match value {
        Json::Array(arr) => {
            let mut filters = Vec::new();
            for sub in arr {
                filters.push(parse_filter(sub)?);
            }
            Ok(Filter::And(filters))
        }
        _ => Err(QueryError::InvalidFilter(
            "$and value must be an array".into(),
        )),
    }
}

fn parse_or(value: &Json) -> Result<Filter, QueryError> {
    match value {
        Json::Array(arr) => {
            let mut filters = Vec::new();
            for sub in arr {
                filters.push(parse_filter(sub)?);
            }
            Ok(Filter::Or(filters))
        }
        _ => Err(QueryError::InvalidFilter(
            "$or value must be an array".into(),
        )),
    }
}

fn parse_field_expr(path: &str, v: &Json) -> Result<Filter, QueryError> {
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
        return Err(QueryError::InvalidFilter(format!(
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

fn parse_cmp_op(_path: &str, op_name: &str, value: &Json) -> Result<CmpOp, QueryError> {
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
                .ok_or_else(|| QueryError::InvalidFilter("$in expects array".into()))?;
            Ok(In(arr.clone()))
        }
        "$nin" => {
            let arr = value
                .as_array()
                .ok_or_else(|| QueryError::InvalidFilter("$nin expects array".into()))?;
            Ok(Nin(arr.clone()))
        }
        "$all" => {
            let arr = value
                .as_array()
                .ok_or_else(|| QueryError::InvalidFilter("$all expects array".into()))?;
            Ok(All(arr.clone()))
        }
        "$exists" => {
            let b = value
                .as_bool()
                .ok_or_else(|| QueryError::InvalidFilter("$exists expects boolean".into()))?;
            Ok(Exists(b))
        }
        "$size" => {
            let n = match value {
                Json::Number(num) => num
                    .as_i64()
                    .ok_or_else(|| QueryError::InvalidFilter("$size expects integer".into()))?,
                _ => return Err(QueryError::InvalidFilter("$size expects integer".into())),
            };
            Ok(Size(n))
        }
        "$not" => {
            // $not value is a single-field expression object
            let inner_obj = value
                .as_object()
                .ok_or_else(|| QueryError::InvalidFilter("$not expects object".into()))?;
            if inner_obj.len() != 1 {
                return Err(QueryError::InvalidFilter(
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
        _ => Err(QueryError::InvalidOperator(format!(
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
pub fn parse_find_options(json: &Json) -> Result<FindOptions, QueryError> {
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
                        QueryError::InvalidSort("sort direction must be 1 or -1".into())
                    })? as i8,
                    _ => {
                        return Err(QueryError::InvalidSort(
                            "sort direction must be 1 or -1".into(),
                        ))
                    }
                };
                if dir != 1 && dir != -1 {
                    return Err(QueryError::InvalidSort(
                        "sort direction must be 1 or -1".into(),
                    ));
                }
                sort_vec.push((field.clone(), dir));
            }
            opts.sort = sort_vec;
        } else {
            return Err(QueryError::InvalidSort(
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
