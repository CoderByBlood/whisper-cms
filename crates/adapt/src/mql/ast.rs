use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// Comparison operations on a single field path.
///
/// This is the core of the MQL-style filter:
///   { "field": { "$op": value } }
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

/// A field-level predicate: path + comparison op.
///
/// Example:
///   path: "kind", op: Eq("post")
///   path: "front_matter.tags", op: In(["rust","wasm"])
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldExpr {
    pub path: String,
    pub op: CmpOp,
}

/// Filter tree:
/// - And([...])
/// - Or([...])
/// - Field(FieldExpr)
///
/// This matches our “Whisper MQL subset v1” design:
///   { kind: "post", draft: false, $or: [ {...}, {...} ] }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Field(FieldExpr),
}

/// Query options:
/// - sort: Vec<(field_path, dir: 1|-1)>
/// - limit: Option<usize>
/// - skip: Option<usize>
///
/// The JSON *input* format for plugins is:
///   {
///     "sort": { "fieldA": 1, "fieldB": -1 },
///     "limit": 10,
///     "skip": 5
///   }
///
/// That is parsed into this internal representation by `parse_find_options`.
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
    // CmpOp tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn cmpop_all_simple_variants_roundtrip() {
        let ops = vec![
            CmpOp::Eq(json!("post")),
            CmpOp::Ne(json!(42)),
            CmpOp::Gt(json!(10)),
            CmpOp::Gte(json!(10)),
            CmpOp::Lt(json!(10)),
            CmpOp::Lte(json!(10)),
            CmpOp::In(vec![json!("rust"), json!("wasm")]),
            CmpOp::Nin(vec![json!("draft")]),
            CmpOp::All(vec![json!("tag1"), json!("tag2")]),
            CmpOp::Exists(true),
            CmpOp::Size(3),
        ];

        for op in ops {
            let v = to_value(&op).expect("serialize CmpOp");
            let back: CmpOp = from_value(v).expect("deserialize CmpOp");
            match (&op, &back) {
                (CmpOp::Eq(a), CmpOp::Eq(b)) => assert_eq!(a, b),
                (CmpOp::Ne(a), CmpOp::Ne(b)) => assert_eq!(a, b),
                (CmpOp::Gt(a), CmpOp::Gt(b)) => assert_eq!(a, b),
                (CmpOp::Gte(a), CmpOp::Gte(b)) => assert_eq!(a, b),
                (CmpOp::Lt(a), CmpOp::Lt(b)) => assert_eq!(a, b),
                (CmpOp::Lte(a), CmpOp::Lte(b)) => assert_eq!(a, b),
                (CmpOp::In(a), CmpOp::In(b)) => assert_eq!(a, b),
                (CmpOp::Nin(a), CmpOp::Nin(b)) => assert_eq!(a, b),
                (CmpOp::All(a), CmpOp::All(b)) => assert_eq!(a, b),
                (CmpOp::Exists(a), CmpOp::Exists(b)) => assert_eq!(a, b),
                (CmpOp::Size(a), CmpOp::Size(b)) => assert_eq!(a, b),
                _ => panic!("variant mismatch after roundtrip: {:?} vs {:?}", op, back),
            }
        }
    }

    #[test]
    fn cmpop_not_variant_roundtrip() {
        let inner = FieldExpr {
            path: "draft".into(),
            op: CmpOp::Eq(json!(true)),
        };
        let op = CmpOp::Not(Box::new(inner.clone()));

        let v = to_value(&op).expect("serialize CmpOp::Not");
        let back: CmpOp = from_value(v).expect("deserialize CmpOp::Not");

        match back {
            CmpOp::Not(boxed) => {
                assert_eq!(boxed.path, inner.path);
                match boxed.op {
                    CmpOp::Eq(j) => assert_eq!(j, json!(true)),
                    other => panic!("expected inner Eq(true), got: {:?}", other),
                }
            }
            other => panic!("expected Not variant, got: {:?}", other),
        }
    }

    #[test]
    fn cmpop_invalid_exists_type_fails() {
        // Exists should be a bool; here we give it a string.
        let v = json!({ "Exists": "not_bool" });
        let res: Result<CmpOp, _> = from_value(v);
        assert!(
            res.is_err(),
            "Expected invalid Exists type to fail deserialization"
        );
    }

    #[test]
    fn cmpop_invalid_size_type_fails() {
        // Size should be an integer; here we give it a string.
        let v = json!({ "Size": "not_int" });
        let res: Result<CmpOp, _> = from_value(v);
        assert!(
            res.is_err(),
            "Expected invalid Size type to fail deserialization"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // FieldExpr & Filter tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn fieldexpr_roundtrip() {
        let fe = FieldExpr {
            path: "front_matter.tags".into(),
            op: CmpOp::In(vec![json!("rust"), json!("wasm")]),
        };

        let v = to_value(&fe).expect("serialize FieldExpr");
        let back: FieldExpr = from_value(v).expect("deserialize FieldExpr");

        assert_eq!(back.path, fe.path);
        match (fe.op, back.op) {
            (CmpOp::In(a), CmpOp::In(b)) => assert_eq!(a, b),
            other => panic!("expected In variant, got {:?}", other),
        }
    }

    #[test]
    fn filter_complex_tree_roundtrip() {
        let f = Filter::And(vec![
            Filter::Field(FieldExpr {
                path: "kind".into(),
                op: CmpOp::Eq(json!("post")),
            }),
            Filter::Or(vec![
                Filter::Field(FieldExpr {
                    path: "draft".into(),
                    op: CmpOp::Eq(json!(false)),
                }),
                Filter::Field(FieldExpr {
                    path: "lang".into(),
                    op: CmpOp::Eq(json!("en")),
                }),
            ]),
            Filter::Field(FieldExpr {
                path: "front_matter.tags".into(),
                op: CmpOp::All(vec![json!("rust"), json!("wasm")]),
            }),
        ]);

        let v = to_value(&f).expect("serialize Filter");
        let back: Filter = from_value(v).expect("deserialize Filter");

        match back {
            Filter::And(children) => {
                assert_eq!(children.len(), 3);

                // First child: Field(kind == "post")
                match &children[0] {
                    Filter::Field(fe) => {
                        assert_eq!(fe.path, "kind");
                        match &fe.op {
                            CmpOp::Eq(j) => assert_eq!(*j, json!("post")),
                            other => panic!("expected kind Eq('post'), got {:?}", other),
                        }
                    }
                    other => panic!("expected Field as first child, got {:?}", other),
                }

                // Second child: Or([...])
                match &children[1] {
                    Filter::Or(inner) => {
                        assert_eq!(inner.len(), 2);
                    }
                    other => panic!("expected Or as second child, got {:?}", other),
                }

                // Third child: Field(front_matter.tags All [...])
                match &children[2] {
                    Filter::Field(fe) => {
                        assert_eq!(fe.path, "front_matter.tags");
                        match &fe.op {
                            CmpOp::All(tags) => {
                                assert_eq!(tags, &vec![json!("rust"), json!("wasm")]);
                            }
                            other => panic!("expected All([...]) for tags, got {:?}", other),
                        }
                    }
                    other => panic!(
                        "expected Field(front_matter.tags) as third child, got {:?}",
                        other
                    ),
                }
            }
            other => panic!("expected top-level And, got {:?}", other),
        }
    }

    #[test]
    fn filter_invalid_enum_tag_fails_deserialization() {
        // There is no "Foo" variant on Filter; this should fail.
        let v = json!({ "Foo": [] });
        let res: Result<Filter, _> = from_value(v);
        assert!(
            res.is_err(),
            "Expected invalid Filter tag to fail deserialization"
        );
    }

    #[test]
    fn filter_invalid_shape_for_and_fails_deserialization() {
        // And expects an array of Filters, but here we give an object.
        let v = json!({ "And": { "not": "an array" } });
        let res: Result<Filter, _> = from_value(v);
        assert!(
            res.is_err(),
            "Expected invalid And shape to fail deserialization"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // FindOptions tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn findoptions_default_is_empty() {
        let opts = FindOptions::default();
        assert!(opts.sort.is_empty());
        assert_eq!(opts.limit, None);
        assert_eq!(opts.skip, None);
    }

    #[test]
    fn findoptions_roundtrip_with_values() {
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
    fn findoptions_invalid_sort_shape_fails_deserialization() {
        // FindOptions.sort is Vec<(String, i8)>, but here we give a number.
        let v = json!({
            "sort": 123,
            "limit": 10,
            "skip": 0
        });

        let res: Result<FindOptions, _> = from_value(v);
        assert!(
            res.is_err(),
            "Expected invalid sort shape to fail deserialization"
        );
    }

    #[test]
    fn findoptions_invalid_sort_entry_fails_deserialization() {
        // sort should be a sequence of [field, dir]; feeding incorrect structure should fail.
        let v = json!({
            "sort": [ 1, 2, 3 ],
            "limit": 10,
            "skip": 0
        });

        let res: Result<FindOptions, _> = from_value(v);
        assert!(
            res.is_err(),
            "Expected invalid sort entries to fail deserialization"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Integration sanity: build a realistic filter + options and serialize
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn integration_realistic_filter_and_options_roundtrip() {
        let filter = Filter::And(vec![
            Filter::Field(FieldExpr {
                path: "kind".into(),
                op: CmpOp::Eq(json!("post")),
            }),
            Filter::Field(FieldExpr {
                path: "draft".into(),
                op: CmpOp::Eq(json!(false)),
            }),
            Filter::Field(FieldExpr {
                path: "front_matter.tags".into(),
                op: CmpOp::In(vec![json!("rust"), json!("wasm")]),
            }),
        ]);

        let opts = FindOptions {
            sort: vec![("front_matter.date".to_string(), -1)],
            limit: Some(20),
            skip: Some(0),
        };

        let fv = to_value(&filter).expect("serialize Filter");
        let ov = to_value(&opts).expect("serialize FindOptions");

        let f_back: Filter = from_value(fv).expect("deserialize Filter");
        let o_back: FindOptions = from_value(ov).expect("deserialize FindOptions");

        // Just basic checks to ensure structure survived.
        match f_back {
            Filter::And(children) => {
                assert_eq!(children.len(), 3);
            }
            other => panic!("expected top-level And, got {:?}", other),
        }

        assert_eq!(o_back.sort.len(), 1);
        assert_eq!(o_back.sort[0].0, "front_matter.date");
        assert_eq!(o_back.sort[0].1, -1);
        assert_eq!(o_back.limit, Some(20));
        assert_eq!(o_back.skip, Some(0));
    }
}
