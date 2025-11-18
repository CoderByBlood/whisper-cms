use super::ast::{CmpOp, FieldExpr, Filter, FindOptions};
use super::error::QueryError;
use super::eval::{eval_filter, get_field_value};
use super::index::{IndexBackend, IndexConfig, JsonStore};

use serde_json::Value as Json;
use std::cmp::Ordering;
use std::collections::HashSet;

/// Result of a query: document ID + owned JSON document.
///
/// Owned JSON keeps this API usable for both in-memory and disk-backed stores.
#[derive(Debug, Clone)]
pub struct QueryResult<Id> {
    pub id: Id,
    pub doc: Json,
}

/// Plans and executes queries over a JsonStore + IndexBackend pair.
///
/// - Uses `IndexConfig` to discover which fields are indexed.
/// - Extracts simple indexable constraints from the filter (equality / IN).
/// - Asks the index backend for candidate ID sets.
/// - Intersects candidate sets when multiple constraints are available.
/// - Falls back to full scan when no index can be used.
/// - Always uses `eval_filter` for final correctness.
///
/// This stays generic over the actual storage engine (in-memory, indexed_json, etc.).
#[derive(Debug)]
pub struct QueryPlanner<'a> {
    index_config: &'a IndexConfig,
}

impl<'a> QueryPlanner<'a> {
    pub fn new(index_config: &'a IndexConfig) -> Self {
        Self { index_config }
    }

    /// Execute a query against the given store + index backend (async).
    pub async fn execute<S, I>(
        &self,
        store: &S,
        index: &I,
        filter: &Filter,
        opts: &FindOptions,
    ) -> Result<Vec<QueryResult<S::Id>>, QueryError>
    where
        S: JsonStore,
        I: IndexBackend<Id = S::Id>,
    {
        // 1. Collect indexable constraints (only equality / IN on indexed fields).
        let constraints = collect_indexable_constraints(filter, self.index_config);

        // 2. Determine candidate IDs using the index, or fall back to all IDs.
        let candidate_ids: Vec<S::Id> = if constraints.is_empty() {
            store.all_ids().await
        } else {
            let mut sets: Vec<HashSet<S::Id>> = Vec::new();

            for c in &constraints {
                if let Some(ids) = lookup_ids_for_constraint(index, c).await {
                    sets.push(ids);
                }
            }

            if sets.is_empty() {
                // No usable index constraints (backend couldn't answer any).
                store.all_ids().await
            } else {
                // Intersect all constraint sets to get final candidate IDs.
                let mut iter = sets.into_iter();
                let first = iter.next().unwrap();
                let acc: HashSet<S::Id> =
                    iter.fold(first, |acc, set| acc.intersection(&set).copied().collect());

                acc.into_iter().collect()
            }
        };

        // 3. Load documents, evaluate filter, and collect matches.
        let mut matches: Vec<QueryResult<S::Id>> = Vec::new();

        for id in candidate_ids {
            if let Some(doc) = store.get(id).await {
                if eval_filter(filter, &doc) {
                    matches.push(QueryResult { id, doc });
                }
            }
        }

        // 4. Apply sorting, skipping, and limiting.
        apply_sort(&mut matches, opts);
        let sliced = apply_skip_limit(matches, opts.skip, opts.limit);

        Ok(sliced)
    }
}

/// Convenience helper to execute a query in one call (async).
///
/// If you already have an IndexConfig and a planner is not reused heavily,
/// this is a nicer single-shot API.
pub async fn execute_query<S, I>(
    index_config: &IndexConfig,
    store: &S,
    index: &I,
    filter: &Filter,
    opts: &FindOptions,
) -> Result<Vec<QueryResult<S::Id>>, QueryError>
where
    S: JsonStore,
    I: IndexBackend<Id = S::Id>,
{
    let planner = QueryPlanner::new(index_config);
    planner.execute(store, index, filter, opts).await
}

/// Constraints that can be answered by the index backend.
///
/// For now we only use:
/// - field == value
/// - field IN values
///
/// Range support can be added later if/when backends implement `lookup_range`.
#[derive(Debug, Clone)]
enum IndexConstraint {
    Eq { field: String, value: Json },
    In { field: String, values: Vec<Json> },
}

/// Walk the filter and extract indexable constraints.
///
/// We are conservative:
/// - Only take constraints on fields that `IndexConfig::is_indexed`.
/// - Only equality / IN (`$eq` / `$in`) are considered indexable for now.
/// - We only harvest constraints in AND contexts; constraints under OR are
///   ignored for indexing (correctness still ensured by eval_filter).
fn collect_indexable_constraints(filter: &Filter, config: &IndexConfig) -> Vec<IndexConstraint> {
    let mut out = Vec::new();
    collect_indexable_constraints_inner(filter, config, true, &mut out);
    out
}

fn collect_indexable_constraints_inner(
    filter: &Filter,
    config: &IndexConfig,
    in_and_context: bool,
    out: &mut Vec<IndexConstraint>,
) {
    match filter {
        Filter::And(children) => {
            for child in children {
                collect_indexable_constraints_inner(child, config, true, out);
            }
        }
        Filter::Or(children) => {
            // Constraints in OR blocks are not safe to use for intersection-based
            // candidate pruning (at least not without more sophisticated analysis),
            // so we recurse with `in_and_context = false`.
            for child in children {
                collect_indexable_constraints_inner(child, config, false, out);
            }
        }
        Filter::Field(FieldExpr { path, op }) => {
            if !in_and_context {
                // Skip index hints under OR for now.
                return;
            }
            if !config.is_indexed(path) {
                return;
            }

            match op {
                CmpOp::Eq(v) => {
                    out.push(IndexConstraint::Eq {
                        field: path.clone(),
                        value: v.clone(),
                    });
                }
                CmpOp::In(values) => {
                    out.push(IndexConstraint::In {
                        field: path.clone(),
                        values: values.clone(),
                    });
                }
                // For now we do not try to use range constraints with indexes.
                _ => {}
            }
        }
    }
}

/// Ask the index backend for candidate IDs for a single constraint (async).
///
/// If the backend cannot answer this constraint, returns None and the caller
/// will treat it as "no index available", falling back to a broader scan.
async fn lookup_ids_for_constraint<I>(index: &I, c: &IndexConstraint) -> Option<HashSet<I::Id>>
where
    I: IndexBackend,
{
    match c {
        IndexConstraint::Eq { field, value } => index.lookup_eq(field, value).await,
        IndexConstraint::In { field, values } => index.lookup_in(field, values).await,
    }
}

/// Apply sorting in-place using `FindOptions.sort`.
///
/// We rely on `get_field_value` to resolve the sort key path, and a simple
/// JSON comparison that orders:
/// - `None` (missing field) after `Some`
/// - Within `Some`, compares only if types match (string, number, bool).
fn apply_sort<Id>(results: &mut [QueryResult<Id>], opts: &FindOptions) {
    if opts.sort.is_empty() {
        return;
    }

    results.sort_by(|a, b| compare_docs_for_sort(a, b, &opts.sort));
}

fn compare_docs_for_sort<Id>(
    a: &QueryResult<Id>,
    b: &QueryResult<Id>,
    sort_keys: &[(String, i8)],
) -> Ordering {
    for (field, dir) in sort_keys {
        let av = get_field_value(&a.doc, field);
        let bv = get_field_value(&b.doc, field);
        let ord = json_cmp(av, bv);

        if ord != Ordering::Equal {
            return if *dir >= 0 { ord } else { ord.reverse() };
        }
    }
    Ordering::Equal
}

/// Compare two optional JSON values for sorting.
///
/// Rules:
/// - `None` is considered greater than `Some` (so missing fields sort last).
/// - If both are `Some`:
///   - strings compare lexicographically,
///   - numbers compare by numeric value,
///   - bools compare `false < true`,
///   - other types compare as `Equal` (stable but arbitrary).
fn json_cmp(a: Option<&Json>, b: Option<&Json>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(va), Some(vb)) => match (va, vb) {
            (Json::String(sa), Json::String(sb)) => sa.cmp(sb),
            (Json::Number(na), Json::Number(nb)) => {
                let fa = na.as_f64();
                let fb = nb.as_f64();
                match (fa, fb) {
                    (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
                    _ => Ordering::Equal,
                }
            }
            (Json::Bool(ba), Json::Bool(bb)) => ba.cmp(bb),
            // For mixed or other types, treat as equal to avoid weird ordering.
            _ => Ordering::Equal,
        },
    }
}

/// Apply skip/limit to a vector of results and return the sliced vector.
fn apply_skip_limit<Id: Clone>(
    results: Vec<QueryResult<Id>>,
    skip: Option<usize>,
    limit: Option<usize>,
) -> Vec<QueryResult<Id>> {
    let start = skip.unwrap_or(0);
    if start >= results.len() {
        return Vec::new();
    }

    let end = if let Some(lim) = limit {
        start.saturating_add(lim).min(results.len())
    } else {
        results.len()
    };

    results[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mql::store::mem::{InMemoryIndexBackend, InMemoryJsonStore};
    use serde_json::json;
    use std::collections::HashSet;

    // We use tokio because QueryPlanner::execute is async.
    use tokio;

    // ─────────────────────────────────────────────────────────────────────
    // collect_indexable_constraints tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn collects_eq_and_in_on_indexed_fields_in_and_context() {
        let filter = Filter::And(vec![
            // indexed eq
            Filter::Field(FieldExpr {
                path: "type".to_string(),
                op: CmpOp::Eq(json!("post")),
            }),
            // indexed in
            Filter::Field(FieldExpr {
                path: "tax.tags".to_string(),
                op: CmpOp::In(vec![json!("rust"), json!("wasm")]),
            }),
            // non-indexed field should be ignored
            Filter::Field(FieldExpr {
                path: "unknown".to_string(),
                op: CmpOp::Eq(json!("ignored")),
            }),
        ]);

        let config = IndexConfig::new(["type", "tax.tags"]);

        let constraints = super::collect_indexable_constraints(&filter, &config);
        assert_eq!(constraints.len(), 2);

        // We don't assert internal order strongly, just that both are present.
        let mut fields: HashSet<String> = constraints
            .iter()
            .map(|c| match c {
                super::IndexConstraint::Eq { field, .. } => field.clone(),
                super::IndexConstraint::In { field, .. } => field.clone(),
            })
            .collect();

        assert!(fields.remove("type"));
        assert!(fields.remove("tax.tags"));
        assert!(fields.is_empty());
    }

    #[test]
    fn ignores_constraints_under_or_for_indexing() {
        let filter = Filter::Or(vec![
            Filter::Field(FieldExpr {
                path: "type".to_string(),
                op: CmpOp::Eq(json!("post")),
            }),
            Filter::Field(FieldExpr {
                path: "tax.tags".to_string(),
                op: CmpOp::In(vec![json!("rust")]),
            }),
        ]);

        let config = IndexConfig::new(["type", "tax.tags"]);
        let constraints = super::collect_indexable_constraints(&filter, &config);

        // We should not harvest any constraints from an OR block.
        assert!(constraints.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────
    // json_cmp tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn json_cmp_orders_missing_last() {
        // Some < None
        let a = Some(&json!(1));
        let b: Option<&Json> = None;
        assert_eq!(super::json_cmp(a, b), Ordering::Less);
        assert_eq!(super::json_cmp(b, a), Ordering::Greater);
    }

    #[test]
    fn json_cmp_handles_strings_numbers_and_bools() {
        // Strings
        assert_eq!(
            super::json_cmp(Some(&json!("a")), Some(&json!("b"))),
            Ordering::Less
        );
        assert_eq!(
            super::json_cmp(Some(&json!("b")), Some(&json!("a"))),
            Ordering::Greater
        );
        assert_eq!(
            super::json_cmp(Some(&json!("x")), Some(&json!("x"))),
            Ordering::Equal
        );

        // Numbers
        assert_eq!(
            super::json_cmp(Some(&json!(1)), Some(&json!(2))),
            Ordering::Less
        );
        assert_eq!(
            super::json_cmp(Some(&json!(2.0)), Some(&json!(1.0))),
            Ordering::Greater
        );
        assert_eq!(
            super::json_cmp(Some(&json!(1.0)), Some(&json!(1))),
            Ordering::Equal
        );

        // Bools
        assert_eq!(
            super::json_cmp(Some(&json!(false)), Some(&json!(true))),
            Ordering::Less
        );
        assert_eq!(
            super::json_cmp(Some(&json!(true)), Some(&json!(false))),
            Ordering::Greater
        );
        assert_eq!(
            super::json_cmp(Some(&json!(true)), Some(&json!(true))),
            Ordering::Equal
        );
    }

    #[test]
    fn json_cmp_mixed_types_treated_as_equal() {
        // Different JSON types should be treated as equal (stable but arbitrary).
        assert_eq!(
            super::json_cmp(Some(&json!("1")), Some(&json!(1))),
            Ordering::Equal
        );
        assert_eq!(
            super::json_cmp(Some(&json!(true)), Some(&json!(1))),
            Ordering::Equal
        );
        assert_eq!(
            super::json_cmp(Some(&json!({ "a": 1 })), Some(&json!([1, 2, 3]))),
            Ordering::Equal
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // apply_skip_limit tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn apply_skip_limit_no_skip_no_limit_returns_all() {
        let results = vec![
            QueryResult {
                id: 0usize,
                doc: json!({ "slug": "a" }),
            },
            QueryResult {
                id: 1usize,
                doc: json!({ "slug": "b" }),
            },
        ];

        let out = super::apply_skip_limit(results.clone(), None, None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].doc["slug"], "a");
        assert_eq!(out[1].doc["slug"], "b");
    }

    #[test]
    fn apply_skip_limit_with_skip_and_limit() {
        let results = vec![
            QueryResult {
                id: 0usize,
                doc: json!({ "slug": "a" }),
            },
            QueryResult {
                id: 1usize,
                doc: json!({ "slug": "b" }),
            },
            QueryResult {
                id: 2usize,
                doc: json!({ "slug": "c" }),
            },
            QueryResult {
                id: 3usize,
                doc: json!({ "slug": "d" }),
            },
        ];

        let out = super::apply_skip_limit(results, Some(1), Some(2));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].doc["slug"], "b");
        assert_eq!(out[1].doc["slug"], "c");
    }

    #[test]
    fn apply_skip_limit_with_skip_beyond_len_returns_empty() {
        let results = vec![
            QueryResult {
                id: 0usize,
                doc: json!({ "slug": "a" }),
            },
            QueryResult {
                id: 1usize,
                doc: json!({ "slug": "b" }),
            },
        ];

        let out = super::apply_skip_limit(results, Some(10), Some(5));
        assert!(out.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────
    // apply_sort tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn apply_sort_sorts_by_single_field_ascending_and_descending() {
        let mut results = vec![
            QueryResult {
                id: 0usize,
                doc: json!({ "slug": "b", "order": 2 }),
            },
            QueryResult {
                id: 1usize,
                doc: json!({ "slug": "a", "order": 1 }),
            },
            QueryResult {
                id: 2usize,
                doc: json!({ "slug": "c", "order": 3 }),
            },
        ];

        // Sort by "order" ascending.
        let opts = FindOptions {
            sort: vec![("order".to_string(), 1)],
            limit: None,
            skip: None,
        };
        super::apply_sort(&mut results, &opts);
        let slugs: Vec<String> = results
            .iter()
            .map(|r| r.doc["slug"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(slugs, vec!["a", "b", "c"]);

        // Sort by "order" descending.
        let mut results = vec![
            QueryResult {
                id: 0usize,
                doc: json!({ "slug": "b", "order": 2 }),
            },
            QueryResult {
                id: 1usize,
                doc: json!({ "slug": "a", "order": 1 }),
            },
            QueryResult {
                id: 2usize,
                doc: json!({ "slug": "c", "order": 3 }),
            },
        ];
        let opts_desc = FindOptions {
            sort: vec![("order".to_string(), -1)],
            limit: None,
            skip: None,
        };
        super::apply_sort(&mut results, &opts_desc);
        let slugs_desc: Vec<String> = results
            .iter()
            .map(|r| r.doc["slug"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(slugs_desc, vec!["c", "b", "a"]);
    }

    #[test]
    fn apply_sort_places_missing_fields_last() {
        let mut results = vec![
            QueryResult {
                id: 0usize,
                doc: json!({ "slug": "a", "order": 1 }),
            },
            QueryResult {
                id: 1usize,
                doc: json!({ "slug": "b" }),
            }, // missing "order"
            QueryResult {
                id: 2usize,
                doc: json!({ "slug": "c", "order": 0 }),
            },
        ];

        let opts = FindOptions {
            sort: vec![("order".to_string(), 1)],
            limit: None,
            skip: None,
        };
        super::apply_sort(&mut results, &opts);

        let slugs: Vec<String> = results
            .iter()
            .map(|r| r.doc["slug"].as_str().unwrap().to_string())
            .collect();

        // "b" (missing order) should come last.
        assert_eq!(slugs, vec!["c", "a", "b"]);
    }

    // ─────────────────────────────────────────────────────────────────────
    // QueryPlanner + InMemoryJsonStore / InMemoryIndexBackend tests
    // ─────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn planner_falls_back_to_full_scan_when_no_indexes() {
        let docs = vec![
            json!({ "type": "post", "slug": "a", "draft": false }),
            json!({ "type": "post", "slug": "b", "draft": true }),
            json!({ "type": "page", "slug": "c", "draft": false }),
        ];

        let store = InMemoryJsonStore::new(docs);
        let config = IndexConfig::new([] as [&str; 0]); // no indexed fields
        let index = InMemoryIndexBackend {
            field_value_to_ids: Default::default(),
        };

        // Filter: type == "post" AND draft == false
        let filter = Filter::And(vec![
            Filter::Field(FieldExpr {
                path: "type".to_string(),
                op: CmpOp::Eq(json!("post")),
            }),
            Filter::Field(FieldExpr {
                path: "draft".to_string(),
                op: CmpOp::Eq(json!(false)),
            }),
        ]);

        let opts = FindOptions::default();
        let planner = QueryPlanner::new(&config);

        let results = planner
            .execute(&store, &index, &filter, &opts)
            .await
            .expect("query should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].doc["slug"], json!("a"));
    }

    #[tokio::test]
    async fn planner_uses_simple_eq_index() {
        let docs = vec![
            json!({ "type": "post", "slug": "a", "draft": false }),
            json!({ "type": "post", "slug": "b", "draft": false }),
            json!({ "type": "page", "slug": "c", "draft": false }),
        ];

        let store = InMemoryJsonStore::new(docs);
        // Index on "type" only.
        let config = IndexConfig::new(["type"]);
        let index = InMemoryIndexBackend::build(&config, &store).await;

        // Filter: type == "post"
        let filter = Filter::Field(FieldExpr {
            path: "type".to_string(),
            op: CmpOp::Eq(json!("post")),
        });

        let opts = FindOptions::default();
        let planner = QueryPlanner::new(&config);

        let results = planner
            .execute(&store, &index, &filter, &opts)
            .await
            .expect("query should succeed");

        // We expect both posts "a" and "b" and not the "page".
        let slugs: HashSet<String> = results
            .into_iter()
            .map(|r| r.doc["slug"].as_str().unwrap().to_string())
            .collect();

        assert_eq!(slugs.len(), 2);
        assert!(slugs.contains("a"));
        assert!(slugs.contains("b"));
    }

    #[tokio::test]
    async fn planner_uses_in_constraint_and_intersects_constraints() {
        let docs = vec![
            json!({ "type": "post", "slug": "a", "draft": false }),
            json!({ "type": "post", "slug": "b", "draft": false }),
            json!({ "type": "post", "slug": "c", "draft": true  }),
            json!({ "type": "page", "slug": "d", "draft": false }),
        ];

        let store = InMemoryJsonStore::new(docs);

        // Index on all fields we’re going to constrain.
        let config = IndexConfig::new(["type", "draft", "slug"]);
        let index = InMemoryIndexBackend::build(&config, &store).await;

        // Filter:
        //   type == "post"
        //   draft == false
        //   slug IN ["a", "b"]
        let filter = Filter::And(vec![
            Filter::Field(FieldExpr {
                path: "type".to_string(),
                op: CmpOp::Eq(json!("post")),
            }),
            Filter::Field(FieldExpr {
                path: "draft".to_string(),
                op: CmpOp::Eq(json!(false)),
            }),
            Filter::Field(FieldExpr {
                path: "slug".to_string(),
                op: CmpOp::In(vec![json!("a"), json!("b")]),
            }),
        ]);

        let opts = FindOptions::default();
        let planner = QueryPlanner::new(&config);

        let results = planner
            .execute(&store, &index, &filter, &opts)
            .await
            .expect("query should succeed");

        // Only "a" and "b" satisfy all three constraints.
        let slugs: HashSet<String> = results
            .into_iter()
            .map(|r| r.doc["slug"].as_str().unwrap().to_string())
            .collect();

        assert_eq!(slugs.len(), 2);
        assert!(slugs.contains("a"));
        assert!(slugs.contains("b"));
        assert!(!slugs.contains("c"));
        assert!(!slugs.contains("d"));
    }

    #[tokio::test]
    async fn planner_returns_empty_when_filter_matches_no_docs() {
        let docs = vec![
            json!({ "type": "post", "slug": "a", "draft": false }),
            json!({ "type": "post", "slug": "b", "draft": true }),
        ];

        let store = InMemoryJsonStore::new(docs);
        let config = IndexConfig::new(["type"]);
        let index = InMemoryIndexBackend::build(&config, &store).await;

        // Filter: type == "page" (no pages in data)
        let filter = Filter::Field(FieldExpr {
            path: "type".to_string(),
            op: CmpOp::Eq(json!("page")),
        });

        let opts = FindOptions::default();
        let planner = QueryPlanner::new(&config);

        let results = planner
            .execute(&store, &index, &filter, &opts)
            .await
            .expect("query should succeed");

        assert!(results.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────
    // execute_query helper tests
    // ─────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn execute_query_single_shot_api() {
        let docs = vec![
            json!({ "type": "post", "slug": "a" }),
            json!({ "type": "page", "slug": "b" }),
        ];

        let store = InMemoryJsonStore::new(docs);
        let config = IndexConfig::new(["type"]);
        let index = InMemoryIndexBackend::build(&config, &store).await;

        let filter = Filter::Field(FieldExpr {
            path: "type".to_string(),
            op: CmpOp::Eq(json!("page")),
        });

        let opts = FindOptions::default();

        let results = execute_query(&config, &store, &index, &filter, &opts)
            .await
            .expect("query should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].doc["slug"], json!("b"));
    }
}
