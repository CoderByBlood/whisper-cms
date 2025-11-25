// crates/adapt/src/mql/query.rs

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
