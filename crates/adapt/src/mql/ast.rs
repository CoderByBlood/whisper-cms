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
