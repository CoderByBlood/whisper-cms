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

    /// Execute a query against the given store + index backend.
    pub fn execute<S, I>(
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
            store.all_ids()
        } else {
            let mut sets: Vec<HashSet<S::Id>> = Vec::new();

            for c in &constraints {
                if let Some(ids) = lookup_ids_for_constraint(index, c) {
                    sets.push(ids);
                }
            }

            if sets.is_empty() {
                // No usable index constraints (backend couldn't answer any).
                store.all_ids()
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
            if let Some(doc) = store.get(id) {
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

/// Convenience helper to execute a query in one call.
///
/// If you already have an IndexConfig and a planner is not reused heavily,
/// this is a nicer single-shot API.
pub fn execute_query<S, I>(
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
    planner.execute(store, index, filter, opts)
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

/// Ask the index backend for candidate IDs for a single constraint.
///
/// If the backend cannot answer this constraint, returns None and the caller
/// will treat it as "no index available", falling back to a broader scan.
fn lookup_ids_for_constraint<I>(index: &I, c: &IndexConstraint) -> Option<HashSet<I::Id>>
where
    I: IndexBackend,
{
    match c {
        IndexConstraint::Eq { field, value } => index.lookup_eq(field, value),
        IndexConstraint::In { field, values } => index.lookup_in(field, values),
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
    use serde_json::{json, Value as Json};
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};

    // ─────────────────────────────────────────────────────────────────────
    // Test helpers
    // ─────────────────────────────────────────────────────────────────────

    /// Simple in-memory JsonStore for tests.
    #[derive(Debug, Clone)]
    struct TestStore {
        docs: Vec<Json>,
    }

    impl TestStore {
        fn new(docs: Vec<Json>) -> Self {
            Self { docs }
        }
    }

    impl JsonStore for TestStore {
        type Id = usize;

        fn all_ids(&self) -> Vec<Self::Id> {
            (0..self.docs.len()).collect()
        }

        fn get(&self, id: Self::Id) -> Option<Json> {
            self.docs.get(id).cloned()
        }
    }

    /// A simple IndexBackend that uses a `(field, value_string)` => set of IDs map.
    ///
    /// This lets us control index answers precisely in tests without knowing any
    /// internal DB structure.
    #[derive(Debug, Clone)]
    struct TestIndex {
        // field -> value_string -> set of IDs
        map: HashMap<String, HashMap<String, HashSet<usize>>>,
        // For debugging / verification if needed
        eq_calls: RefCell<Vec<(String, String)>>,
        in_calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl TestIndex {
        fn new() -> Self {
            Self {
                map: HashMap::new(),
                eq_calls: RefCell::new(Vec::new()),
                in_calls: RefCell::new(Vec::new()),
            }
        }

        /// Configure index entries: field, value_json, ids.
        fn add_entry<I>(&mut self, field: &str, value: Json, ids: I)
        where
            I: IntoIterator<Item = usize>,
        {
            let key = value.to_string();
            let field_map = self.map.entry(field.to_string()).or_default();
            let set = field_map.entry(key).or_default();
            set.extend(ids);
        }
    }

    impl IndexBackend for TestIndex {
        type Id = usize;

        fn lookup_eq(&self, field: &str, value: &Json) -> Option<HashSet<Self::Id>> {
            let key = value.to_string();
            self.eq_calls
                .borrow_mut()
                .push((field.to_string(), key.clone()));
            self.map.get(field)?.get(&key).cloned()
        }

        fn lookup_in(&self, field: &str, values: &[Json]) -> Option<HashSet<Self::Id>> {
            let mut keys = Vec::new();
            for v in values {
                keys.push(v.to_string());
            }
            self.in_calls
                .borrow_mut()
                .push((field.to_string(), keys.clone()));

            let field_map = self.map.get(field)?;
            let mut acc = HashSet::new();
            for key in keys {
                if let Some(ids) = field_map.get(&key) {
                    acc.extend(ids.iter().copied());
                }
            }
            if acc.is_empty() {
                None
            } else {
                Some(acc)
            }
        }
    }

    /// Convenience to build a simple equality Field filter.
    fn field_eq(path: &str, value: Json) -> Filter {
        Filter::Field(FieldExpr {
            path: path.to_string(),
            op: CmpOp::Eq(value),
        })
    }

    /// Convenience to build an IN Field filter.
    fn field_in(path: &str, values: Vec<Json>) -> Filter {
        Filter::Field(FieldExpr {
            path: path.to_string(),
            op: CmpOp::In(values),
        })
    }

    // ─────────────────────────────────────────────────────────────────────
    // collect_indexable_constraints tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn collect_indexable_constraints_picks_eq_and_in_on_indexed_fields_under_and() {
        let config = IndexConfig::new(["kind", "front_matter.tags"]);

        let filter = Filter::And(vec![
            field_eq("kind", json!("post")),
            field_in("front_matter.tags", vec![json!("rust"), json!("wasm")]),
            // Non-indexed field should be ignored:
            field_eq("draft", json!(false)),
        ]);

        let constraints = super::collect_indexable_constraints(&filter, &config);
        assert_eq!(constraints.len(), 2);

        // We don't care about order; just presence.
        let mut has_kind = false;
        let mut has_tags = false;

        for c in constraints {
            match c {
                IndexConstraint::Eq { field, value } => {
                    assert_eq!(field, "kind");
                    assert_eq!(value, json!("post"));
                    has_kind = true;
                }
                IndexConstraint::In { field, values } => {
                    assert_eq!(field, "front_matter.tags");
                    assert_eq!(values, vec![json!("rust"), json!("wasm")]);
                    has_tags = true;
                }
            }
        }

        assert!(has_kind);
        assert!(has_tags);
    }

    #[test]
    fn collect_indexable_constraints_ignores_constraints_under_or() {
        let config = IndexConfig::new(["kind"]);

        let filter = Filter::Or(vec![
            field_eq("kind", json!("post")),
            field_eq("kind", json!("page")),
        ]);

        let constraints = super::collect_indexable_constraints(&filter, &config);
        assert!(
            constraints.is_empty(),
            "constraints under OR must not be used for indexing"
        );
    }

    #[test]
    fn collect_indexable_constraints_empty_when_no_indexed_fields() {
        let config = IndexConfig::new(Vec::<&str>::new());

        let filter = Filter::And(vec![
            field_eq("kind", json!("post")),
            field_eq("draft", json!(false)),
        ]);

        let constraints = super::collect_indexable_constraints(&filter, &config);
        assert!(constraints.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────
    // execute / execute_query tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn execute_full_scan_when_no_constraints() {
        // No indexes at all.
        let config = IndexConfig::new(Vec::<&str>::new());

        // Three docs, two posts and one page.
        let docs = vec![
            json!({ "kind": "post", "draft": false }),
            json!({ "kind": "page", "draft": false }),
            json!({ "kind": "post", "draft": true }),
        ];
        let store = TestStore::new(docs);

        // Index backend that never answers anything (like having no indexes).
        let index = TestIndex::new();

        // Filter: kind == "post".
        let filter = field_eq("kind", json!("post"));
        let opts = FindOptions::default();

        let planner = QueryPlanner::new(&config);
        let results = planner
            .execute(&store, &index, &filter, &opts)
            .expect("query execute should succeed");

        // Both posts should match because eval_filter will see kind == "post"
        // and there is no index-based pruning.
        let ids: std::collections::HashSet<_> = results.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&0));
        assert!(ids.contains(&2));
    }

    #[test]
    fn execute_uses_index_constraints_when_available() {
        // Index on "kind".
        let config = IndexConfig::new(["kind"]);

        let docs = vec![
            json!({ "kind": "post", "draft": false }),
            json!({ "kind": "page", "draft": false }),
            json!({ "kind": "post", "draft": true }),
            json!({ "kind": "post", "draft": false }),
        ];
        let store = TestStore::new(docs);

        // Index: "kind" == "post" -> IDs {0, 2, 3}
        let mut index = TestIndex::new();
        index.add_entry("kind", json!("post"), [0, 2, 3]);

        let filter = Filter::And(vec![
            field_eq("kind", json!("post")),
            field_eq("draft", json!(false)),
        ]);

        let opts = FindOptions::default();
        let planner = QueryPlanner::new(&config);
        let results = planner
            .execute(&store, &index, &filter, &opts)
            .expect("query execute should succeed");

        // Filter: kind == "post" AND draft == false
        // Among the candidate IDs {0,2,3}, doc 0 and 3 match, doc 2 is draft=true.
        let ids: HashSet<_> = results.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&0));
        assert!(ids.contains(&3));
        assert!(!ids.contains(&2));
    }

    #[test]
    fn execute_query_is_convenience_wrapper() {
        let config = IndexConfig::new(["kind"]);

        let docs = vec![
            json!({ "kind": "post", "draft": false }),
            json!({ "kind": "page", "draft": false }),
        ];
        let store = TestStore::new(docs);

        let mut index = TestIndex::new();
        index.add_entry("kind", json!("post"), [0]);

        let filter = field_eq("kind", json!("post"));
        let opts = FindOptions::default();

        let results = execute_query(&config, &store, &index, &filter, &opts)
            .expect("execute_query should succeed");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 0);
        assert_eq!(results[0].doc["kind"], json!("post"));
    }

    #[test]
    fn execute_ignores_or_constraints_for_index_but_still_filters_correctly() {
        let config = IndexConfig::new(["kind"]);

        let docs = vec![
            json!({ "kind": "post", "lang": "en" }),
            json!({ "kind": "page", "lang": "en" }),
            json!({ "kind": "post", "lang": "fr" }),
        ];
        let store = TestStore::new(docs);

        // Index for kind == "post" -> {0, 2}
        let mut index = TestIndex::new();
        index.add_entry("kind", json!("post"), [0, 2]);

        // Filter: kind == "post" OR lang == "en"
        // Indexable part under Or is ignored, we only index on And context.
        let filter = Filter::Or(vec![
            field_eq("kind", json!("post")),
            field_eq("lang", json!("en")),
        ]);

        let opts = FindOptions::default();
        let planner = QueryPlanner::new(&config);
        let results = planner
            .execute(&store, &index, &filter, &opts)
            .expect("query execute should succeed");

        // Expected: all docs match because:
        //  - doc 0: kind=post
        //  - doc 1: lang=en
        //  - doc 2: kind=post
        assert_eq!(results.len(), 3);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Sorting tests (apply_sort / json_cmp)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn apply_sort_sorts_by_single_key_ascending() {
        let mut results = vec![
            QueryResult {
                id: 0,
                doc: json!({ "order": 3 }),
            },
            QueryResult {
                id: 1,
                doc: json!({ "order": 1 }),
            },
            QueryResult {
                id: 2,
                doc: json!({ "order": 2 }),
            },
        ];

        let opts = FindOptions {
            sort: vec![("order".to_string(), 1)],
            limit: None,
            skip: None,
        };

        apply_sort(&mut results, &opts);
        let orders: Vec<_> = results
            .iter()
            .map(|r| r.doc["order"].as_i64().unwrap())
            .collect();
        assert_eq!(orders, vec![1, 2, 3]);
    }

    #[test]
    fn apply_sort_sorts_by_single_key_descending() {
        let mut results = vec![
            QueryResult {
                id: 0,
                doc: json!({ "order": 1 }),
            },
            QueryResult {
                id: 1,
                doc: json!({ "order": 3 }),
            },
            QueryResult {
                id: 2,
                doc: json!({ "order": 2 }),
            },
        ];

        let opts = FindOptions {
            sort: vec![("order".to_string(), -1)],
            limit: None,
            skip: None,
        };

        apply_sort(&mut results, &opts);
        let orders: Vec<_> = results
            .iter()
            .map(|r| r.doc["order"].as_i64().unwrap())
            .collect();
        assert_eq!(orders, vec![3, 2, 1]);
    }

    #[test]
    fn apply_sort_handles_missing_fields_last() {
        let mut results = vec![
            QueryResult {
                id: 0,
                doc: json!({ "order": 2 }),
            },
            QueryResult {
                id: 1,
                doc: json!({}),
            },
            QueryResult {
                id: 2,
                doc: json!({ "order": 1 }),
            },
        ];

        let opts = FindOptions {
            sort: vec![("order".to_string(), 1)],
            limit: None,
            skip: None,
        };

        apply_sort(&mut results, &opts);
        let ids: Vec<_> = results.iter().map(|r| r.id).collect();
        // Docs with order: 1, 2 come first; missing field last.
        assert_eq!(ids, vec![2, 0, 1]);
    }

    #[test]
    fn json_cmp_orders_none_after_some_and_compares_types() {
        // None vs Some
        assert_eq!(json_cmp(None, None), Ordering::Equal);
        assert_eq!(json_cmp(None, Some(&json!(1))), Ordering::Greater);
        assert_eq!(json_cmp(Some(&json!(1)), None), Ordering::Less);

        // Strings
        assert_eq!(
            json_cmp(Some(&json!("a")), Some(&json!("b"))),
            Ordering::Less
        );
        assert_eq!(
            json_cmp(Some(&json!("b")), Some(&json!("a"))),
            Ordering::Greater
        );

        // Numbers
        assert_eq!(json_cmp(Some(&json!(1)), Some(&json!(2))), Ordering::Less);
        assert_eq!(
            json_cmp(Some(&json!(2)), Some(&json!(1))),
            Ordering::Greater
        );

        // Bools
        assert_eq!(
            json_cmp(Some(&json!(false)), Some(&json!(true))),
            Ordering::Less
        );
        assert_eq!(
            json_cmp(Some(&json!(true)), Some(&json!(false))),
            Ordering::Greater
        );

        // Mixed types → Equal
        assert_eq!(
            json_cmp(Some(&json!("1")), Some(&json!(1))),
            Ordering::Equal
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // apply_skip_limit tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn apply_skip_limit_basic_cases() {
        let results: Vec<QueryResult<usize>> = (0..5)
            .map(|id| QueryResult {
                id,
                doc: json!({ "id": id }),
            })
            .collect();

        // No skip/limit.
        let slice = apply_skip_limit(results.clone(), None, None);
        assert_eq!(slice.len(), 5);
        assert_eq!(slice[0].id, 0);
        assert_eq!(slice[4].id, 4);

        // Limit only.
        let slice = apply_skip_limit(results.clone(), None, Some(2));
        assert_eq!(slice.len(), 2);
        assert_eq!(slice[0].id, 0);
        assert_eq!(slice[1].id, 1);

        // Skip only.
        let slice = apply_skip_limit(results.clone(), Some(2), None);
        assert_eq!(slice.len(), 3);
        assert_eq!(slice[0].id, 2);
        assert_eq!(slice[2].id, 4);

        // Skip + limit.
        let slice = apply_skip_limit(results.clone(), Some(1), Some(2));
        assert_eq!(slice.len(), 2);
        assert_eq!(slice[0].id, 1);
        assert_eq!(slice[1].id, 2);

        // Skip beyond length -> empty.
        let slice = apply_skip_limit(results.clone(), Some(10), Some(2));
        assert!(slice.is_empty());
    }

    #[test]
    fn apply_sort_no_sort_keys_keeps_order() {
        let mut results = vec![
            QueryResult {
                id: 0,
                doc: json!({ "order": 3 }),
            },
            QueryResult {
                id: 1,
                doc: json!({ "order": 1 }),
            },
        ];

        let opts = FindOptions::default();

        apply_sort(&mut results, &opts);
        let ids: Vec<_> = results.iter().map(|r| r.id).collect();

        // With no sort keys, order should remain as-is.
        assert_eq!(ids, vec![0, 1]);
    }
}
