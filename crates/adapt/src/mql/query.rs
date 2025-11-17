use super::ast::{CmpOp, FieldExpr, Filter, FindOptions};
use super::error::QueryError;
use super::eval::eval_filter;
use super::index::{IndexBackend, IndexConfig, JsonStore};
use serde_json::Value as Json;
use std::cmp::Ordering;
use std::collections::HashSet;

/// Result of a query: (document id, document JSON reference).
pub struct QueryResult<'a, Id> {
    pub id: Id,
    pub doc: &'a Json,
}

/// A simple query planner that extracts indexable constraints from a Filter
/// and uses them to ask the index backend for candidate document ids.
pub struct QueryPlanner<'a> {
    index_config: &'a IndexConfig,
}

impl<'a> QueryPlanner<'a> {
    pub fn new(index_config: &'a IndexConfig) -> Self {
        Self { index_config }
    }

    /// Execute a planned query:
    ///
    /// 1. Analyze the filter to find indexable constraints.
    /// 2. Use the index to get candidate ids (intersection of index hits).
    /// 3. If no indexable constraints exist, fall back to a full scan of all ids.
    /// 4. Load docs from the JsonStore and evaluate the full filter via `eval_filter`.
    /// 5. Apply sort + skip + limit in-memory.
    pub fn execute<'b, S, I>(
        &self,
        store: &'b S,
        index: &I,
        filter: &Filter,
        opts: &FindOptions,
    ) -> Result<Vec<QueryResult<'b, S::Id>>, QueryError>
    where
        S: JsonStore,
        I: IndexBackend<Id = S::Id>,
    {
        // 1. Collect indexable constraints.
        let constraints = collect_indexable_constraints(filter, self.index_config);

        // 2. Choose candidate ids.
        let candidate_ids: Vec<S::Id> = if constraints.is_empty() {
            // No usable index predicates → full scan.
            store.all_ids().into_iter().collect()
        } else {
            // For each constraint, ask the index for matching ids, then intersect.
            let mut iter = constraints
                .into_iter()
                .map(|c| lookup_ids_for_constraint(index, &c))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter();

            if let Some(first) = iter.next() {
                let mut acc = first;
                for set in iter {
                    acc = acc.intersection(&set).cloned().collect::<HashSet<S::Id>>();
                }
                acc.into_iter().collect()
            } else {
                Vec::new()
            }
        };

        // 3. Load docs and evaluate the full filter to ensure correctness.
        let mut docs: Vec<QueryResult<'b, S::Id>> = candidate_ids
            .into_iter()
            .filter_map(|id| store.get(id).map(|doc| QueryResult { id, doc }))
            .filter(|qr| eval_filter(filter, qr.doc))
            .collect();

        // 4. Sort & paginate.
        apply_sort(&mut docs, &opts.sort);
        let sliced = apply_skip_limit(docs, opts.skip, opts.limit);

        Ok(sliced)
    }
}

/// A single indexable constraint of the form:
///   field == value   or   field IN [values...]
#[derive(Debug, Clone)]
enum IndexConstraint {
    Eq { field: String, value: Json },
    In { field: String, values: Vec<Json> },
}

/// Traverse the Filter tree and collect indexable constraints based on the
/// provided IndexConfig.
fn collect_indexable_constraints(filter: &Filter, config: &IndexConfig) -> Vec<IndexConstraint> {
    let mut out = Vec::new();
    collect_indexable_constraints_inner(filter, config, &mut out);
    out
}

fn collect_indexable_constraints_inner(
    filter: &Filter,
    config: &IndexConfig,
    out: &mut Vec<IndexConstraint>,
) {
    match filter {
        Filter::And(list) | Filter::Or(list) => {
            for f in list {
                collect_indexable_constraints_inner(f, config, out);
            }
        }
        Filter::Field(FieldExpr { path, op }) => {
            if !config.is_indexed(path) {
                return;
            }

            match op {
                CmpOp::Eq(v) => out.push(IndexConstraint::Eq {
                    field: path.clone(),
                    value: v.clone(),
                }),
                CmpOp::In(values) => out.push(IndexConstraint::In {
                    field: path.clone(),
                    values: values.clone(),
                }),
                _ => {
                    // Non-indexable op (>, <, exists, etc.) – ignore here.
                }
            }
        }
    }
}

/// Ask the index backend for ids matching a single constraint.
fn lookup_ids_for_constraint<I>(
    index: &I,
    c: &IndexConstraint,
) -> Result<HashSet<I::Id>, QueryError>
where
    I: IndexBackend,
{
    match c {
        IndexConstraint::Eq { field, value } => index
            .lookup_eq(field, value)
            .ok_or_else(|| QueryError::Other("Error in eqaulity test".into())),
        IndexConstraint::In { field, values } => index
            .lookup_in(field, values)
            .ok_or_else(|| QueryError::Other("Error in In test".into())),
    }
}

/// Apply sort clauses in-place.
///
/// `sort` is a Vec<(field_path, dir)> where dir is 1 (asc) or -1 (desc).
fn apply_sort<'a, Id>(docs: &mut [QueryResult<'a, Id>], sort: &[(String, i8)]) {
    if sort.is_empty() || docs.len() <= 1 {
        return;
    }

    docs.sort_by(|a, b| {
        for (field, dir) in sort {
            let ord = compare_field(a.doc, b.doc, field);
            if ord != Ordering::Equal {
                return if *dir >= 0 { ord } else { ord.reverse() };
            }
        }
        Ordering::Equal
    });
}

/// Compare a single field across two docs.
fn compare_field(a: &Json, b: &Json, field: &str) -> Ordering {
    use serde_json::Value as J;

    let va = field_value(a, field);
    let vb = field_value(b, field);

    match (va, vb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(J::Number(na)), Some(J::Number(nb))) => {
            let fa = na.as_f64().unwrap_or(f64::NAN);
            let fb = nb.as_f64().unwrap_or(f64::NAN);
            fa.partial_cmp(&fb).unwrap_or(Ordering::Equal)
        }
        (Some(J::String(sa)), Some(J::String(sb))) => sa.cmp(sb),
        (Some(J::Bool(ba)), Some(J::Bool(bb))) => ba.cmp(bb),
        // Fallback: debug representation compare.
        (Some(va), Some(vb)) => format!("{:?}", va).cmp(&format!("{:?}", vb)),
    }
}

/// Resolve a dotted path into a nested JSON value.
fn field_value<'a>(doc: &'a Json, path: &str) -> Option<&'a Json> {
    let mut current = doc;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Apply skip + limit to a vector, returning a new owned Vec.
fn apply_skip_limit<'a, Id>(
    mut docs: Vec<QueryResult<'a, Id>>,
    skip: Option<usize>,
    limit: Option<usize>,
) -> Vec<QueryResult<'a, Id>> {
    let total = docs.len();
    let start = skip.unwrap_or(0).min(total);

    // Hard internal cap for safety; callers can pass a smaller explicit limit.
    const MAX_LIMIT: usize = 1000;
    let requested = limit.unwrap_or(MAX_LIMIT).min(MAX_LIMIT);
    let end = (start + requested).min(total);

    if start >= end {
        return Vec::new();
    }

    docs.drain(start..end).collect()
}

/// Convenience wrapper for typical usage:
///
/// - Parse filter & options from JSON.
/// - Plan + execute query.
/// - Return docs.
pub fn execute_query<'a, S, I>(
    store: &'a S,
    index: &I,
    index_config: &IndexConfig,
    filter_json: &Json,
    options_json: &Json,
) -> Result<Vec<QueryResult<'a, S::Id>>, QueryError>
where
    S: JsonStore,
    I: IndexBackend<Id = S::Id>,
{
    use super::parser::{parse_filter, parse_find_options};

    let filter = parse_filter(filter_json)?;
    let opts = parse_find_options(options_json)?;
    let planner = QueryPlanner::new(index_config);
    planner.execute(store, index, &filter, &opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Convenience to build QueryResult values.
    fn make_qr<'a, Id: Copy>(id: Id, doc: &'a Json) -> QueryResult<'a, Id> {
        QueryResult { id, doc }
    }

    // ─────────────────────────────────────────────────────────────
    // field_value
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn field_value_finds_top_level_field() {
        let doc = json!({"a": 1, "b": "x"});
        let v = field_value(&doc, "a");
        assert_eq!(v, Some(&json!(1)));

        let v2 = field_value(&doc, "b");
        assert_eq!(v2, Some(&json!("x")));
    }

    #[test]
    fn field_value_finds_nested_dotted_path() {
        let doc = json!({
            "front_matter": {
                "tags": ["rust", "cms"],
                "meta": { "author": "phil" }
            }
        });

        assert_eq!(
            field_value(&doc, "front_matter.tags"),
            Some(&json!(["rust", "cms"]))
        );
        assert_eq!(
            field_value(&doc, "front_matter.meta.author"),
            Some(&json!("phil"))
        );
    }

    #[test]
    fn field_value_missing_segment_returns_none() {
        let doc = json!({ "a": { "b": 1 } });

        // Missing top-level
        assert_eq!(field_value(&doc, "does_not_exist"), None);

        // Missing nested
        assert_eq!(field_value(&doc, "a.c"), None);
        assert_eq!(field_value(&doc, "a.b.c"), None);
    }

    #[test]
    fn field_value_empty_path_returns_none() {
        let doc = json!({ "a": 1 });
        // Splitting "" yields one empty segment -> doc.get("") is None.
        assert_eq!(field_value(&doc, ""), None);
    }

    // ─────────────────────────────────────────────────────────────
    // compare_field
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn compare_field_numbers() {
        let a = json!({ "n": 1 });
        let b = json!({ "n": 2 });

        assert_eq!(compare_field(&a, &b, "n"), Ordering::Less);
        assert_eq!(compare_field(&b, &a, "n"), Ordering::Greater);
        assert_eq!(
            compare_field(&a, &json!({ "n": 1.0 }), "n"),
            Ordering::Equal
        );
    }

    #[test]
    fn compare_field_strings() {
        let a = json!({ "s": "apple" });
        let b = json!({ "s": "banana" });

        assert_eq!(compare_field(&a, &b, "s"), Ordering::Less);
        assert_eq!(compare_field(&b, &a, "s"), Ordering::Greater);
        assert_eq!(
            compare_field(&a, &json!({ "s": "apple" }), "s"),
            Ordering::Equal
        );
    }

    #[test]
    fn compare_field_bools() {
        let a = json!({ "b": false });
        let b = json!({ "b": true });

        assert_eq!(compare_field(&a, &b, "b"), Ordering::Less);
        assert_eq!(compare_field(&b, &a, "b"), Ordering::Greater);
        assert_eq!(
            compare_field(&a, &json!({ "b": false }), "b"),
            Ordering::Equal
        );
    }

    #[test]
    fn compare_field_missing_vs_present() {
        let a = json!({}); // no "x"
        let b = json!({ "x": 1 });

        assert_eq!(compare_field(&a, &b, "x"), Ordering::Less);
        assert_eq!(compare_field(&b, &a, "x"), Ordering::Greater);
    }

    #[test]
    fn compare_field_both_missing_equal() {
        let a = json!({ "foo": 1 });
        let b = json!({ "bar": 2 });

        assert_eq!(compare_field(&a, &b, "x"), Ordering::Equal);
    }

    #[test]
    fn compare_field_mismatched_types_fallback_debug() {
        let a = json!({ "v": 1 });
        let b = json!({ "v": "1" });

        // Not a panic; falls back to debug representation comparison
        let ord = compare_field(&a, &b, "v");
        // We don't assert exact ordering (depends on debug formatting),
        // just that it produces a stable Ordering value (i.e. doesn't panic).
        assert!(matches!(
            ord,
            Ordering::Less | Ordering::Equal | Ordering::Greater
        ));
    }

    // ─────────────────────────────────────────────────────────────
    // apply_sort
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn apply_sort_no_sort_or_single_doc_is_noop() {
        let doc = json!({ "x": 10 });
        let mut docs = vec![make_qr(1u32, &doc)];

        apply_sort(&mut docs, &[]);
        assert_eq!(docs[0].id, 1);

        // With one doc and a sort spec, should still be fine
        apply_sort(&mut docs, &[("x".to_string(), 1)]);
        assert_eq!(docs[0].id, 1);
    }

    #[test]
    fn apply_sort_single_key_ascending() {
        let d1 = json!({ "x": 2 });
        let d2 = json!({ "x": 1 });
        let d3 = json!({ "x": 3 });

        let mut docs = vec![make_qr(1u32, &d1), make_qr(2u32, &d2), make_qr(3u32, &d3)];

        apply_sort(&mut docs, &[("x".to_string(), 1)]);

        let ids: Vec<u32> = docs.iter().map(|qr| qr.id).collect();
        assert_eq!(ids, vec![2, 1, 3]); // 1,2,3 by x -> ids 2,1,3
    }

    #[test]
    fn apply_sort_single_key_descending() {
        let d1 = json!({ "x": 2 });
        let d2 = json!({ "x": 1 });
        let d3 = json!({ "x": 3 });

        let mut docs = vec![make_qr(1u32, &d1), make_qr(2u32, &d2), make_qr(3u32, &d3)];

        apply_sort(&mut docs, &[("x".to_string(), -1)]);

        let ids: Vec<u32> = docs.iter().map(|qr| qr.id).collect();
        assert_eq!(ids, vec![3, 1, 2]); // 3,2,1 by x -> ids 3,1,2
    }

    #[test]
    fn apply_sort_multiple_keys() {
        let d1 = json!({ "a": 1, "b": 2 });
        let d2 = json!({ "a": 1, "b": 1 });
        let d3 = json!({ "a": 0, "b": 5 });

        let mut docs = vec![make_qr(1u32, &d1), make_qr(2u32, &d2), make_qr(3u32, &d3)];

        // sort by a asc, then b asc
        apply_sort(&mut docs, &[("a".to_string(), 1), ("b".to_string(), 1)]);

        let ids: Vec<u32> = docs.iter().map(|qr| qr.id).collect();
        // a: 0 -> id 3, a: 1 & b: 1 -> id 2, a: 1 & b: 2 -> id 1
        assert_eq!(ids, vec![3, 2, 1]);
    }

    #[test]
    fn apply_sort_missing_fields_ordering() {
        let d1 = json!({ "x": 1 });
        let d2 = json!({}); // missing x

        let mut docs = vec![make_qr(1u32, &d1), make_qr(2u32, &d2)];

        // Ascending: missing comes first (None < Some(_))
        apply_sort(&mut docs, &[("x".to_string(), 1)]);
        let ids: Vec<u32> = docs.iter().map(|qr| qr.id).collect();
        assert_eq!(ids, vec![2, 1]);

        // Descending: missing comes last
        apply_sort(&mut docs, &[("x".to_string(), -1)]);
        let ids: Vec<u32> = docs.iter().map(|qr| qr.id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    // ─────────────────────────────────────────────────────────────
    // apply_skip_limit
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn apply_skip_limit_basic() {
        let docs_data: Vec<Json> = (0..5).map(|n| json!({ "n": n })).collect();
        let docs_vec: Vec<QueryResult<'_, u32>> = docs_data
            .iter()
            .enumerate()
            .map(|(i, j)| make_qr(i as u32, j))
            .collect();

        // skip 1, limit 2 -> ids 1,2
        let sliced = apply_skip_limit(docs_vec, Some(1), Some(2));
        let ids: Vec<u32> = sliced.iter().map(|qr| qr.id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn apply_skip_limit_no_skip_no_limit_defaults_to_internal_cap() {
        // Small test to ensure "no skip/limit" just returns everything if below cap.
        let docs_data: Vec<Json> = (0..10).map(|n| json!({ "n": n })).collect();
        let docs_vec: Vec<QueryResult<'_, u32>> = docs_data
            .iter()
            .enumerate()
            .map(|(i, j)| make_qr(i as u32, j))
            .collect();

        let sliced = apply_skip_limit(docs_vec, None, None);
        let ids: Vec<u32> = sliced.iter().map(|qr| qr.id).collect();
        assert_eq!(ids, (0u32..10u32).collect::<Vec<_>>());
    }

    #[test]
    fn apply_skip_limit_skip_beyond_length_returns_empty() {
        let docs_data: Vec<Json> = (0..3).map(|n| json!({ "n": n })).collect();
        let docs_vec: Vec<QueryResult<'_, u32>> = docs_data
            .iter()
            .enumerate()
            .map(|(i, j)| make_qr(i as u32, j))
            .collect();

        let sliced = apply_skip_limit(docs_vec, Some(10), Some(5));
        assert!(sliced.is_empty());
    }

    #[test]
    fn apply_skip_limit_zero_limit_returns_empty() {
        let docs_data: Vec<Json> = (0..5).map(|n| json!({ "n": n })).collect();
        let docs_vec: Vec<QueryResult<'_, u32>> = docs_data
            .iter()
            .enumerate()
            .map(|(i, j)| make_qr(i as u32, j))
            .collect();

        // Explicit limit = 0 currently means "no results".
        let sliced = apply_skip_limit(docs_vec, Some(0), Some(0));
        assert!(sliced.is_empty());
    }

    #[test]
    fn apply_skip_limit_does_not_exceed_internal_max_limit() {
        // Build more than MAX_LIMIT docs (internal const is 1000).
        let num_docs = 1200;
        let docs_data: Vec<Json> = (0..num_docs).map(|n| json!({ "n": n })).collect();
        let docs_vec: Vec<QueryResult<'_, u32>> = docs_data
            .iter()
            .enumerate()
            .map(|(i, j)| make_qr(i as u32, j))
            .collect();

        let sliced = apply_skip_limit(docs_vec, None, Some(num_docs)); // ask for more than cap
                                                                       // We can't directly read MAX_LIMIT here, but we know it is <= num_docs.
                                                                       // We assert it's less than or equal to 1000 (per code) and > 0.
        assert!(sliced.len() <= 1000);
        assert!(!sliced.is_empty());
    }
}
