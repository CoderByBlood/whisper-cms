use std::{collections::HashMap, error::Error as StdError};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, DatabaseError>;

#[derive(Debug, Error)]
pub enum DatabaseError {
    /// A simple message-only error.
    #[error("Call failed: {0}")]
    Message(String),

    /// Generic catch-all that preserves the original error’s display text and chain.
    #[error(transparent)]
    Mapped(#[from] Box<dyn StdError + Send + Sync + 'static>),
}

pub trait DbError: StdError + Send + Sync + 'static {}

// Blanket conversion: lets `?` lift *any* error into TheirError::Other
impl<E> From<E> for DatabaseError
where
    E: DbError,
{
    fn from(e: E) -> Self {
        DatabaseError::Mapped(Box::new(e))
    }
}

/// Logical bind values your infra can translate to its own binder.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Text(String),
    Int(i32),
    Long(i64),
    Bool(bool),
    Blob(Vec<u8>),
    Float(f32),
    Double(f64),
    Json(String), // stored as TEXT on SQLite
}
// ─────────────────────────────────────────────────────────────────────────────
// DatabaseService — vtable-free bundle of public DB functions (fn pointers)
// ─────────────────────────────────────────────────────────────────────────────

/// Thin, zero-dyn service that exposes database operations via `fn` pointers.
/// Default wiring points at this module's public functions.
#[derive(Clone, Copy)]
pub struct DatabaseService {
    pub exec_batch_write: fn(&str, Vec<(String, Vec<SqlValue>)>) -> Result<usize>,
    pub exec_fetch_all: fn(&str, (String, Vec<SqlValue>)) -> Result<Vec<HashMap<String, SqlValue>>>,
    pub checkpoint_wal: fn(&str) -> Result<()>,
}

impl DatabaseService {
    /// Build from explicit function pointers (useful for tests/fakes).
    pub const fn from_fns(
        exec_batch_write: fn(&str, Vec<(String, Vec<SqlValue>)>) -> Result<usize>,
        exec_fetch_all: fn(&str, (String, Vec<SqlValue>)) -> Result<Vec<HashMap<String, SqlValue>>>,
        checkpoint_wal: fn(&str) -> Result<()>,
    ) -> Self {
        Self {
            exec_batch_write,
            exec_fetch_all,
            checkpoint_wal,
        }
    }

    // Optional convenience wrappers:

    #[inline]
    pub fn write_batch(
        &self,
        db_url: &str,
        statements: Vec<(String, Vec<SqlValue>)>,
    ) -> Result<usize> {
        (self.exec_batch_write)(db_url, statements)
    }

    #[inline]
    pub fn fetch_all(
        &self,
        db_url: &str,
        statement: (String, Vec<SqlValue>),
    ) -> Result<Vec<HashMap<String, SqlValue>>> {
        (self.exec_fetch_all)(db_url, statement)
    }

    #[inline]
    pub fn checkpoint(&self, db_url: &str) -> Result<()> {
        (self.checkpoint_wal)(db_url)
    }
}
