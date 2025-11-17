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
