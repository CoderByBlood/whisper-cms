use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// Comparison operations on a single field path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CmpOp {
    Eq(Json),
    Ne(Json),
    Gt(Json),
    Gte(Json),
    Lt(Json),
    Lte(Json),
    In(Vec<Json>),
    Nin(Vec<Json>),
    All(Vec<Json>),
    Exists(bool),
    Size(i64),
    Not(Box<FieldExpr>),
}

/// A single field expression: `<path> <op>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldExpr {
    pub path: String, // e.g. "kind", "front_matter.tags"
    pub op: CmpOp,
}

/// Filter tree:
/// - Field(expr)
/// - And([...])
/// - Or([...])
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Field(FieldExpr),
}

/// Query options:
/// - sort: Vec<(field_path, dir: 1|-1)>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindOptions {
    pub sort: Vec<(String, i8)>,
    pub limit: Option<usize>,
    pub skip: Option<usize>,
}

impl Default for FindOptions {
    fn default() -> Self {
        Self {
            sort: Vec::new(),
            limit: None,
            skip: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json, to_value};

    // ─────────────────────────────────────────────────────────────────────
    // CmpOp construction & serde
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn cmpop_eq_serde_round_trip() {
        let op = CmpOp::Eq(json!("value"));
        let v = to_value(&op).expect("serialize CmpOp::Eq");
        // Derived representation is { "Eq": <json> }
        assert_eq!(v, json!({ "Eq": "value" }));

        let back: CmpOp = from_value(v).expect("deserialize CmpOp::Eq");
        match back {
            CmpOp::Eq(j) => assert_eq!(j, json!("value")),
            _ => panic!("expected CmpOp::Eq"),
        }
    }

    #[test]
    fn cmpop_in_and_not_variants_serde_round_trip() {
        let op = CmpOp::In(vec![json!(1), json!(2)]);
        let v = to_value(&op).expect("serialize CmpOp::In");
        assert_eq!(v, json!({ "In": [1, 2] }));

        let back: CmpOp = from_value(v).expect("deserialize CmpOp::In");
        match back {
            CmpOp::In(vals) => assert_eq!(vals, vec![json!(1), json!(2)]),
            _ => panic!("expected CmpOp::In"),
        }

        // Nested Not(FieldExpr)
        let inner = FieldExpr {
            path: "age".to_string(),
            op: CmpOp::Gt(json!(18)),
        };
        let op_not = CmpOp::Not(Box::new(inner.clone()));
        let v_not = to_value(&op_not).expect("serialize CmpOp::Not");
        let back_not: CmpOp = from_value(v_not).expect("deserialize CmpOp::Not");

        match back_not {
            CmpOp::Not(inner_back) => {
                assert_eq!(inner_back.path, inner.path);
                match inner_back.op {
                    CmpOp::Gt(j) => assert_eq!(j, json!(18)),
                    _ => panic!("expected inner op Gt"),
                }
            }
            _ => panic!("expected CmpOp::Not"),
        }
    }

    #[test]
    fn cmpop_exists_and_size_variants() {
        let exists = CmpOp::Exists(true);
        let size = CmpOp::Size(3);

        let v_exists = to_value(&exists).unwrap();
        let v_size = to_value(&size).unwrap();

        assert_eq!(v_exists, json!({ "Exists": true }));
        assert_eq!(v_size, json!({ "Size": 3 }));

        let back_exists: CmpOp = from_value(v_exists).unwrap();
        let back_size: CmpOp = from_value(v_size).unwrap();

        match back_exists {
            CmpOp::Exists(flag) => assert!(flag),
            _ => panic!("expected Exists"),
        }
        match back_size {
            CmpOp::Size(n) => assert_eq!(n, 3),
            _ => panic!("expected Size"),
        }
    }

    #[test]
    fn cmpop_deserialize_invalid_fails() {
        // Wrong shape: not an object
        let bad = json!("Eq");
        assert!(from_value::<CmpOp>(bad).is_err());

        // Unknown variant
        let bad2 = json!({ "Unknown": 1 });
        assert!(from_value::<CmpOp>(bad2).is_err());

        // Wrong payload type for Size (expects i64)
        let bad3 = json!({ "Size": "not-an-int" });
        assert!(from_value::<CmpOp>(bad3).is_err());
    }

    // ─────────────────────────────────────────────────────────────────────
    // FieldExpr
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn fieldexpr_basic_construction_and_serde() {
        let expr = FieldExpr {
            path: "front_matter.tags".to_string(),
            op: CmpOp::In(vec![json!("rust"), json!("cms")]),
        };

        let v = to_value(&expr).expect("serialize FieldExpr");
        let back: FieldExpr = from_value(v).expect("deserialize FieldExpr");

        assert_eq!(back.path, "front_matter.tags");
        match back.op {
            CmpOp::In(vals) => {
                assert_eq!(vals, vec![json!("rust"), json!("cms")]);
            }
            _ => panic!("expected CmpOp::In"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Filter tree serde
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn filter_nested_and_or_serde_round_trip() {
        let f = Filter::And(vec![
            Filter::Field(FieldExpr {
                path: "kind".to_string(),
                op: CmpOp::Eq(json!("post")),
            }),
            Filter::Or(vec![
                Filter::Field(FieldExpr {
                    path: "front_matter.tags".to_string(),
                    op: CmpOp::In(vec![json!("rust")]),
                }),
                Filter::Field(FieldExpr {
                    path: "front_matter.published".to_string(),
                    op: CmpOp::Eq(json!(true)),
                }),
            ]),
        ]);

        let v = to_value(&f).expect("serialize Filter");
        let back: Filter = from_value(v).expect("deserialize Filter");

        // Quick sanity: top-level is And with 2 children
        match back {
            Filter::And(children) => {
                assert_eq!(children.len(), 2);
                // First child is Field(kind == "post")
                match &children[0] {
                    Filter::Field(fe) => {
                        assert_eq!(fe.path, "kind");
                        match &fe.op {
                            CmpOp::Eq(j) => assert_eq!(j, &json!("post")),
                            _ => panic!("expected Eq for kind"),
                        }
                    }
                    _ => panic!("expected Field filter as first child"),
                }
                // Second child is Or(...)
                match &children[1] {
                    Filter::Or(inner) => {
                        assert_eq!(inner.len(), 2);
                    }
                    _ => panic!("expected Or as second child"),
                }
            }
            _ => panic!("expected top-level And"),
        }
    }

    #[test]
    fn filter_deserialize_invalid_variant_fails() {
        let bad = json!({ "Unknown": [] });
        assert!(from_value::<Filter>(bad).is_err());
    }

    // ─────────────────────────────────────────────────────────────────────
    // FindOptions
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn findoptions_default_values() {
        let opts = FindOptions::default();
        assert!(opts.sort.is_empty());
        assert!(opts.limit.is_none());
        assert!(opts.skip.is_none());
    }

    #[test]
    fn findoptions_custom_values_and_serde() {
        let opts = FindOptions {
            sort: vec![
                ("front_matter.date".to_string(), -1),
                ("kind".to_string(), 1),
            ],
            limit: Some(10),
            skip: Some(5),
        };

        let v = to_value(&opts).expect("serialize FindOptions");
        let back: FindOptions = from_value(v).expect("deserialize FindOptions");

        assert_eq!(back.sort.len(), 2);
        assert_eq!(back.sort[0].0, "front_matter.date");
        assert_eq!(back.sort[0].1, -1);
        assert_eq!(back.sort[1].0, "kind");
        assert_eq!(back.sort[1].1, 1);
        assert_eq!(back.limit, Some(10));
        assert_eq!(back.skip, Some(5));
    }

    #[test]
    fn findoptions_deserialize_invalid_types_fail() {
        // limit should be usize; here we give a string.
        let bad = json!({
            "sort": [["field", 1]],
            "limit": "not-a-number",
            "skip": 0
        });

        assert!(from_value::<FindOptions>(bad).is_err());
    }
}
