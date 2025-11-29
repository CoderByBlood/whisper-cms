pub mod ast;
pub mod error;
pub mod eval;
pub mod index;
pub mod parser;
pub mod query;

pub use ast::{CmpOp, FieldExpr, Filter, FindOptions};
pub use error::QueryError;
pub use eval::eval_filter;
pub use index::{
    IndexBackend,
    IndexConfig,
    // These will exist once you add the skeleton in `index.rs`:
    JsonStore,
};
pub use query::{execute_query, QueryPlanner, QueryResult};
