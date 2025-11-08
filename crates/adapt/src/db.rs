use snafu::Snafu;
use std::collections::HashMap;
use std::error::Error;

/// Errors (SNAFU)
#[derive(Debug, Snafu)]
pub enum DatabaseError {
    #[snafu(display("Call failed: {msg}"))]
    Message { msg: String },
    // A generic catch-all that preserves the source error and backtrace
    #[snafu(display("{source}"))]
    Mapped {
        #[snafu(source)]
        source: Box<dyn Error + Send + Sync + 'static>,
        // Enable SNAFU backtraces with the "backtrace" feature in Cargo.toml:
        // snafu = { version = "0.8.9", features = ["backtraces"] }
        //#[snafu(backtrace)]
        //backtrace: snafu::Backtrace,
    },
}

pub trait DbError: Error + Send + Sync + 'static {}

// Blanket conversion: lets `?` lift *any* error into TheirError::Other
impl<E> From<E> for DatabaseError
where
    E: DbError,
{
    fn from(e: E) -> Self {
        DatabaseError::Mapped {
            source: Box::new(e),
        }
    }
}

pub type Result<T> = std::result::Result<T, DatabaseError>;
pub type UpsertBatch = fn(db_url: &str, statements: Vec<(String, Vec<SqlValue>)>) -> Result<usize>;
pub type FetchAll =
    fn(db_url: &str, statement: (String, Vec<SqlValue>)) -> Result<Vec<HashMap<String, SqlValue>>>;
pub type CheckpointWAL = fn(db_url: &str) -> Result<()>;

pub enum Database {
    Injected(String, UpsertBatch, FetchAll, CheckpointWAL),
}

impl Database {
    pub async fn upsert_batch(&self, statements: Vec<(String, Vec<SqlValue>)>) -> Result<usize> {
        match self {
            Database::Injected(db_url, upsert_batch, _, _) => upsert_batch(db_url, statements),
        }
    }

    pub async fn fetch_all(
        &self,
        statement: (String, Vec<SqlValue>),
    ) -> Result<Vec<HashMap<String, SqlValue>>> {
        match self {
            Database::Injected(db_url, _, fetch_all, _) => fetch_all(db_url, statement),
        }
    }

    pub async fn checkpoint_wal(&self) -> Result<()> {
        match self {
            Database::Injected(db_url, _, _, checkpoint_wal) => checkpoint_wal(db_url),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fmt;
    use tokio::task::JoinHandle;

    // Local alias for readability
    type Rows = Vec<HashMap<String, super::SqlValue>>;

    // Minimal custom error to exercise DatabaseError::Mapped
    #[derive(Debug)]
    struct MyErr;
    impl fmt::Display for MyErr {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "my-err")
        }
    }
    impl std::error::Error for MyErr {}
    impl super::DbError for MyErr {}

    fn mk_db(
        url: &str,
        upsert: super::UpsertBatch,
        fetch: super::FetchAll,
        ckpt: super::CheckpointWAL,
    ) -> super::Database {
        super::Database::Injected(url.to_string(), upsert, fetch, ckpt)
    }

    // ── Positive: upsert + fetch + checkpoint work end-to-end
    #[tokio::test]
    async fn positive_upsert_fetch_ckpt() {
        fn upsert_ok(_db_url: &str, stmts: Vec<(String, Vec<SqlValue>)>) -> super::Result<usize> {
            // Return number of statements as a deterministic signal
            Ok(stmts.len())
        }
        fn fetch_ok(_db_url: &str, _stmt: (String, Vec<SqlValue>)) -> super::Result<Rows> {
            // Return one predictable row
            let mut row = HashMap::new();
            row.insert("id".into(), SqlValue::Long(1));
            row.insert("name".into(), SqlValue::Text("alice".into()));
            row.insert("ok".into(), SqlValue::Bool(true));
            row.insert("pi".into(), SqlValue::Double(3.14));
            row.insert("note".into(), SqlValue::Null);
            Ok(vec![row])
        }
        fn ckpt_ok(_db_url: &str) -> super::Result<()> {
            Ok(())
        }

        let db = mk_db("mem:///db", upsert_ok, fetch_ok, ckpt_ok);

        // upsert returns number of statements
        let n = db
            .upsert_batch(vec![
                ("INSERT 1".to_string(), vec![SqlValue::Int(7)]),
                ("INSERT 2".to_string(), vec![SqlValue::Text("x".into())]),
            ])
            .await
            .expect("upsert ok");
        assert_eq!(n, 2);

        // fetch returns our row
        let rows = db
            .fetch_all(("SELECT *".to_string(), vec![]))
            .await
            .expect("fetch ok");
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.get("id"), Some(&SqlValue::Long(1)));
        assert_eq!(r.get("name"), Some(&SqlValue::Text("alice".into())));
        assert_eq!(r.get("ok"), Some(&SqlValue::Bool(true)));
        assert_eq!(r.get("pi"), Some(&SqlValue::Double(3.14)));
        assert_eq!(r.get("note"), Some(&SqlValue::Null));

        // checkpoint is callable
        db.checkpoint_wal().await.expect("ckpt ok");
    }

    // ── SqlValue equality smoke test (no shared state)
    #[test]
    fn sqlvalue_equality_smoke() {
        assert_eq!(SqlValue::Int(7), SqlValue::Int(7));
        assert_ne!(SqlValue::Int(7), SqlValue::Int(8));
        assert_eq!(SqlValue::Text("a".into()), SqlValue::Text("a".into()));
        assert_ne!(SqlValue::Text("a".into()), SqlValue::Text("b".into()));
        assert_eq!(SqlValue::Null, SqlValue::Null);
        assert_eq!(SqlValue::Long(1), SqlValue::Long(1));
        assert_ne!(SqlValue::Long(1), SqlValue::Long(2));
        assert_eq!(SqlValue::Bool(true), SqlValue::Bool(true));
        assert_ne!(SqlValue::Bool(true), SqlValue::Bool(false));
        assert_eq!(SqlValue::Double(1.5), SqlValue::Double(1.5));
        assert_ne!(SqlValue::Double(1.5), SqlValue::Double(1.6));
        assert_eq!(SqlValue::Float(1.0), SqlValue::Float(1.0));
    }

    // ── Negative: Message error propagates
    #[tokio::test]
    async fn upsert_propagates_message_error() {
        fn upsert_err(_db_url: &str, _stmts: Vec<(String, Vec<SqlValue>)>) -> super::Result<usize> {
            Err(super::DatabaseError::Message { msg: "boom".into() })
        }
        fn fetch_noop(_db_url: &str, _stmt: (String, Vec<SqlValue>)) -> super::Result<Rows> {
            Ok(Vec::<HashMap<String, SqlValue>>::new())
        }
        fn ckpt_noop(_db_url: &str) -> super::Result<()> {
            Ok(())
        }

        let db = mk_db("mem:///db", upsert_err, fetch_noop, ckpt_noop);
        match db.upsert_batch(vec![]).await.unwrap_err() {
            super::DatabaseError::Message { msg } => assert_eq!(msg, "boom"),
            other => panic!("unexpected error: {:?}", other),
        }
    }

    // ── Negative: Mapped error propagates (From<DbError>)
    #[tokio::test]
    async fn upsert_propagates_mapped_error() {
        fn upsert_err(_db_url: &str, _stmts: Vec<(String, Vec<SqlValue>)>) -> super::Result<usize> {
            let e: MyErr = MyErr;
            Err(e.into())
        }
        fn fetch_noop(_db_url: &str, _stmt: (String, Vec<SqlValue>)) -> super::Result<Rows> {
            Ok(Vec::<HashMap<String, SqlValue>>::new())
        }
        fn ckpt_noop(_db_url: &str) -> super::Result<()> {
            Ok(())
        }

        let db = mk_db("mem:///db", upsert_err, fetch_noop, ckpt_noop);
        let err = db.upsert_batch(vec![]).await.unwrap_err();
        assert_eq!(format!("{}", err), "my-err");
    }

    // ── Negative: fetch error maps
    #[tokio::test]
    async fn fetch_propagates_mapped_error() {
        fn upsert_noop(
            _db_url: &str,
            _stmts: Vec<(String, Vec<SqlValue>)>,
        ) -> super::Result<usize> {
            Ok(0)
        }
        fn fetch_err(_db_url: &str, _stmt: (String, Vec<SqlValue>)) -> super::Result<Rows> {
            Err(MyErr.into())
        }
        fn ckpt_noop(_db_url: &str) -> super::Result<()> {
            Ok(())
        }

        let db = mk_db("mem:///db", upsert_noop, fetch_err, ckpt_noop);
        let err = db.fetch_all(("SELECT".into(), vec![])).await.unwrap_err();
        assert_eq!(format!("{}", err), "my-err");
    }

    // ── Negative: checkpoint error maps
    #[tokio::test]
    async fn checkpoint_propagates_message_error() {
        fn upsert_noop(
            _db_url: &str,
            _stmts: Vec<(String, Vec<SqlValue>)>,
        ) -> super::Result<usize> {
            Ok(0)
        }
        fn fetch_noop(_db_url: &str, _stmt: (String, Vec<SqlValue>)) -> super::Result<Rows> {
            Ok(Vec::<HashMap<String, SqlValue>>::new())
        }
        fn ckpt_err(_db_url: &str) -> super::Result<()> {
            Err(super::DatabaseError::Message {
                msg: "ckpt-fail".into(),
            })
        }

        let db = mk_db("mem:///db", upsert_noop, fetch_noop, ckpt_err);
        match db.checkpoint_wal().await.unwrap_err() {
            super::DatabaseError::Message { msg } => assert_eq!(msg, "ckpt-fail"),
            other => panic!("unexpected error: {:?}", other),
        }
    }

    // ── Reentrancy: spawn many tasks and ensure each returns expected result
    #[tokio::test]
    async fn upsert_is_reentrant_from_many_tasks() {
        use std::sync::Arc;

        fn upsert_ok(_db_url: &str, stmts: Vec<(String, Vec<SqlValue>)>) -> super::Result<usize> {
            // Return len to verify each task runs the injected function
            Ok(stmts.len())
        }
        fn fetch_noop(_db_url: &str, _stmt: (String, Vec<SqlValue>)) -> super::Result<Rows> {
            Ok(Vec::<HashMap<String, SqlValue>>::new())
        }
        fn ckpt_noop(_db_url: &str) -> super::Result<()> {
            Ok(())
        }

        let db = Arc::new(mk_db("mem:///db", upsert_ok, fetch_noop, ckpt_noop));

        let mut handles: Vec<JoinHandle<super::Result<usize>>> = Vec::with_capacity(10);
        for i in 0..10 {
            let db_cloned = Arc::clone(&db);
            let stmts = vec![
                (format!("INSERT {i}a"), vec![SqlValue::Int(i)]),
                (format!("INSERT {i}b"), vec![SqlValue::Text("x".into())]),
            ];
            handles.push(tokio::spawn(
                async move { db_cloned.upsert_batch(stmts).await },
            ));
        }

        // Validate all results; no reliance on shared counters
        let mut oks = 0usize;
        for h in handles {
            let r = h.await.expect("task join ok");
            match r {
                Ok(2) => oks += 1,
                other => panic!("unexpected upsert result: {:?}", other),
            }
        }
        assert_eq!(oks, 10, "all tasks should return Ok(2)");
    }
}
