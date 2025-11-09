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
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fmt;

    // A tiny custom error that implements DbError so we can test the blanket From<E> -> DatabaseError::Mapped
    #[derive(Debug)]
    struct MyErr;
    impl fmt::Display for MyErr {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "my-err")
        }
    }
    impl StdError for MyErr {}
    impl DbError for MyErr {}

    // Convenience alias used in a few tests
    type Rows = Vec<HashMap<String, SqlValue>>;

    // ─────────────────────────────────────────────────────────────────────
    // POSITIVE: Service dispatch succeeds and parameters are correctly passed
    // ─────────────────────────────────────────────────────────────────────

    fn write_ok_expect_mem_ok(url: &str, stmts: Vec<(String, Vec<SqlValue>)>) -> Result<usize> {
        assert_eq!(url, "mem://ok");
        assert_eq!(stmts.len(), 2);

        // 1st statement shape/content
        assert_eq!(stmts[0].0, "INSERT A");
        assert_eq!(
            stmts[0].1,
            vec![SqlValue::Int(1), SqlValue::Text("a".into())]
        );

        // 2nd statement shape/content
        assert_eq!(stmts[1].0, "INSERT B");
        assert_eq!(stmts[1].1, vec![SqlValue::Long(2), SqlValue::Bool(true)]);

        Ok(stmts.len())
    }

    fn fetch_ok_expect_mem_ok(url: &str, stmt: (String, Vec<SqlValue>)) -> Result<Rows> {
        assert_eq!(url, "mem://ok");
        assert_eq!(stmt.0, "SELECT * FROM t WHERE id = ?");
        assert_eq!(stmt.1, vec![SqlValue::Long(2)]);

        let mut row = HashMap::new();
        row.insert("id".to_string(), SqlValue::Long(2));
        row.insert("name".to_string(), SqlValue::Text("alice".into()));
        row.insert("active".to_string(), SqlValue::Bool(true));
        row.insert("pi".to_string(), SqlValue::Double(3.14));
        row.insert("note".to_string(), SqlValue::Null);
        Ok(vec![row])
    }

    fn ckpt_ok_expect_mem_ok(url: &str) -> Result<()> {
        assert_eq!(url, "mem://ok");
        Ok(())
    }

    #[test]
    fn service_dispatch_and_data_flow_ok() {
        let svc = DatabaseService::from_fns(
            write_ok_expect_mem_ok,
            fetch_ok_expect_mem_ok,
            ckpt_ok_expect_mem_ok,
        );

        // write
        let wrote = svc
            .write_batch(
                "mem://ok",
                vec![
                    (
                        "INSERT A".to_string(),
                        vec![SqlValue::Int(1), SqlValue::Text("a".into())],
                    ),
                    (
                        "INSERT B".to_string(),
                        vec![SqlValue::Long(2), SqlValue::Bool(true)],
                    ),
                ],
            )
            .expect("write ok");
        assert_eq!(wrote, 2);

        // fetch
        let rows = svc
            .fetch_all(
                "mem://ok",
                (
                    "SELECT * FROM t WHERE id = ?".to_string(),
                    vec![SqlValue::Long(2)],
                ),
            )
            .expect("fetch ok");
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.get("id"), Some(&SqlValue::Long(2)));
        assert_eq!(r.get("name"), Some(&SqlValue::Text("alice".into())));
        assert_eq!(r.get("active"), Some(&SqlValue::Bool(true)));
        assert_eq!(r.get("pi"), Some(&SqlValue::Double(3.14)));
        assert_eq!(r.get("note"), Some(&SqlValue::Null));

        // checkpoint
        svc.checkpoint("mem://ok").expect("ckpt ok");
    }

    // ─────────────────────────────────────────────────────────────────────
    // NEGATIVE: Message errors propagate as Message(...)
    // ─────────────────────────────────────────────────────────────────────

    fn write_err_message(_url: &str, _stmts: Vec<(String, Vec<SqlValue>)>) -> Result<usize> {
        Err(DatabaseError::Message("boom-write".into()))
    }
    fn fetch_noop(_url: &str, _stmt: (String, Vec<SqlValue>)) -> Result<Rows> {
        Ok(Vec::new())
    }
    fn ckpt_noop(_url: &str) -> Result<()> {
        Ok(())
    }

    #[test]
    fn write_propagates_message_error() {
        let svc = DatabaseService::from_fns(write_err_message, fetch_noop, ckpt_noop);
        let err = svc
            .write_batch("mem://any", vec![("INSERT X".into(), vec![])])
            .unwrap_err();
        match err {
            DatabaseError::Message(s) => assert_eq!(s, "boom-write"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn fetch_err_message(_url: &str, _stmt: (String, Vec<SqlValue>)) -> Result<Rows> {
        Err(DatabaseError::Message("boom-fetch".into()))
    }

    #[test]
    fn fetch_propagates_message_error() {
        let svc = DatabaseService::from_fns(write_ok_expect_mem_ok, fetch_err_message, ckpt_noop);
        let err = svc
            .fetch_all("mem://ok", ("SELECT".into(), vec![]))
            .unwrap_err();
        match err {
            DatabaseError::Message(s) => assert_eq!(s, "boom-fetch"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn ckpt_err_message(_url: &str) -> Result<()> {
        Err(DatabaseError::Message("boom-ckpt".into()))
    }

    #[test]
    fn checkpoint_propagates_message_error() {
        let svc = DatabaseService::from_fns(
            write_ok_expect_mem_ok,
            fetch_ok_expect_mem_ok,
            ckpt_err_message,
        );
        let err = svc.checkpoint("mem://ok").unwrap_err();
        match err {
            DatabaseError::Message(s) => assert_eq!(s, "boom-ckpt"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // NEGATIVE: DbError -> DatabaseError::Mapped via blanket From<E>
    // ─────────────────────────────────────────────────────────────────────

    fn write_err_mapped(_url: &str, _stmts: Vec<(String, Vec<SqlValue>)>) -> Result<usize> {
        let e = MyErr;
        Err(DatabaseError::from(e))
    }

    #[test]
    fn write_propagates_mapped_error() {
        let svc = DatabaseService::from_fns(write_err_mapped, fetch_noop, ckpt_noop);
        let err = svc
            .write_batch("mem://any", vec![("INSERT".into(), vec![])])
            .unwrap_err();
        // We specifically expect the transparent Mapped variant; Display should be "my-err"
        match err {
            DatabaseError::Mapped(e) => assert_eq!(e.to_string(), "my-err"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn fetch_err_mapped(_url: &str, _stmt: (String, Vec<SqlValue>)) -> Result<Rows> {
        Err(DatabaseError::from(MyErr))
    }

    #[test]
    fn fetch_propagates_mapped_error() {
        let svc = DatabaseService::from_fns(write_ok_expect_mem_ok, fetch_err_mapped, ckpt_noop);
        let err = svc
            .fetch_all("mem://ok", ("SELECT".into(), vec![]))
            .unwrap_err();
        match err {
            DatabaseError::Mapped(e) => assert_eq!(e.to_string(), "my-err"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn ckpt_err_mapped(_url: &str) -> Result<()> {
        Err(DatabaseError::from(MyErr))
    }

    #[test]
    fn checkpoint_propagates_mapped_error() {
        let svc = DatabaseService::from_fns(
            write_ok_expect_mem_ok,
            fetch_ok_expect_mem_ok,
            ckpt_err_mapped,
        );
        let err = svc.checkpoint("mem://ok").unwrap_err();
        match err {
            DatabaseError::Mapped(e) => assert_eq!(e.to_string(), "my-err"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // SqlValue sanity & equality checks (no shared state)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn sqlvalue_equality_and_variants_smoke() {
        assert_eq!(SqlValue::Null, SqlValue::Null);
        assert_eq!(SqlValue::Text("x".into()), SqlValue::Text("x".into()));
        assert_ne!(SqlValue::Text("x".into()), SqlValue::Text("y".into()));

        assert_eq!(SqlValue::Int(7), SqlValue::Int(7));
        assert_ne!(SqlValue::Int(7), SqlValue::Int(8));

        assert_eq!(SqlValue::Long(1), SqlValue::Long(1));
        assert_ne!(SqlValue::Long(1), SqlValue::Long(2));

        assert_eq!(SqlValue::Bool(true), SqlValue::Bool(true));
        assert_ne!(SqlValue::Bool(true), SqlValue::Bool(false));

        assert_eq!(SqlValue::Float(1.0), SqlValue::Float(1.0));
        assert_eq!(SqlValue::Double(1.5), SqlValue::Double(1.5));
        assert_ne!(SqlValue::Double(1.5), SqlValue::Double(1.6));

        assert_eq!(SqlValue::Blob(vec![1, 2]), SqlValue::Blob(vec![1, 2]));
        assert_ne!(SqlValue::Blob(vec![1, 2]), SqlValue::Blob(vec![2, 1]));

        assert_eq!(
            SqlValue::Json("{\"a\":1}".into()),
            SqlValue::Json("{\"a\":1}".into())
        );
        assert_ne!(SqlValue::Json("x".into()), SqlValue::Json("y".into()));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Service helpers (wrappers) simply forward to the function pointers
    // ─────────────────────────────────────────────────────────────────────

    fn write_echo_len(_url: &str, stmts: Vec<(String, Vec<SqlValue>)>) -> Result<usize> {
        Ok(stmts.len())
    }
    fn fetch_echo_empty(_url: &str, _stmt: (String, Vec<SqlValue>)) -> Result<Rows> {
        Ok(vec![])
    }
    fn ckpt_ok(_url: &str) -> Result<()> {
        Ok(())
    }

    #[test]
    fn wrapper_methods_forward_calls() {
        let svc = DatabaseService::from_fns(write_echo_len, fetch_echo_empty, ckpt_ok);

        let n = svc
            .write_batch(
                "mem://x",
                vec![
                    ("SQL1".into(), vec![SqlValue::Int(1)]),
                    ("SQL2".into(), vec![]),
                    ("SQL3".into(), vec![SqlValue::Null]),
                ],
            )
            .expect("write");
        assert_eq!(n, 3);

        let rows = svc
            .fetch_all("mem://x", ("SELECT 1".into(), vec![]))
            .expect("fetch");
        assert!(rows.is_empty());

        svc.checkpoint("mem://x").expect("ckpt");
    }
}
