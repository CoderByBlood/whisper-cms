//! db.rs — DB infrastructure (read-only pool + single writer actor per DB URL).
//! - Singleton read-only SqlitePool per DB URL.
//! - Single writer actor per DB URL executing batched SQL in one IMMEDIATE txn.
//! - Public API hides actor details; consumers use exec_batch_write()/checkpoint_wal().

use adapt::db::DbError;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    ConnectOptions, SqliteConnection, SqlitePool,
};
use std::{
    collections::HashMap,
    str::FromStr,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};
use thiserror::Error;
use tokio::sync::OnceCell;

#[derive(Debug, Error)]
pub enum SqliteDbError {
    #[error("Connection failed: {0}")]
    Connect(#[from] sqlx::Error),

    #[error("Setup/migration failed: {0}")]
    Setup(sqlx::Error),

    #[error("SQL execution failed: {0}")]
    Execute(sqlx::Error),

    #[error("Actor call failed: {0}")]
    ActorCall(String),

    #[error("Error message: {0}")]
    Message(String),
}

impl SqliteDbError {
    pub fn src(source: sqlx::Error) -> Self {
        SqliteDbError::Execute(source)
    }
    pub fn msg(msg: String) -> Self {
        SqliteDbError::Message(msg)
    }
}

impl DbError for SqliteDbError {}

pub type Result<T, E = SqliteDbError> = std::result::Result<T, E>;

/// ─────────────────────────────────────────────────────────────────────────────
/// Read-only pools (singleton per DB URL)
/// ─────────────────────────────────────────────────────────────────────────────

static READ_POOLS: LazyLock<Mutex<HashMap<String, Arc<OnceCell<SqlitePool>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

async fn build_read_only_pool_inner(db_url: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(db_url.strip_prefix("sqlite://").unwrap_or(db_url))
        .map_err(|e| SqliteDbError::ActorCall { msg: e.to_string() })?
        .read_only(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .min_connections(0)
        .max_connections(3) // SQLite likes small pools
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(opts)
        .await
        .context(ConnectSnafu)?;

    Ok(pool)
}

/// Public: get (lazily create) the shared **read-only** pool for this `db_url`.
pub(crate) async fn get_read_only_pool(db_url: &str) -> Result<SqlitePool> {
    let cell_arc = {
        let mut map = READ_POOLS.lock().unwrap();
        map.entry(db_url.to_string())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone()
    };

    let pool_ref = cell_arc
        .get_or_try_init(|| async { build_read_only_pool_inner(db_url).await })
        .await?;
    Ok(pool_ref.clone())
}

/// ─────────────────────────────────────────────────────────────────────────────
/// Writer actor (one per DB URL) via per-URL OnceCell registry
/// Actor details are NOT exposed publicly.
/// ─────────────────────────────────────────────────────────────────────────────

/// Generic bind values for write batches (schema-agnostic).
#[derive(Debug, Clone)]
pub enum Bind {
    Text(String),
    Integer(i64),
    Real(f64),
    Blob(Vec<u8>),
    Null,
}

/// Internal messages for the writer actor (crate-visible only).
#[derive(Debug)]
pub(crate) enum DbWriteMsg {
    /// Execute a batch of (SQL, binds) statements inside a single transaction.
    /// Reply returns total rows_affected across all statements.
    ExecBatch {
        statements: Vec<(String, Vec<Bind>)>,
        reply: Option<RpcReplyPort<Result<usize>>>,
    },
    /// Trigger a WAL checkpoint (useful after big bursts).
    CheckpointWal {
        reply: Option<RpcReplyPort<Result<()>>>,
    },
}

struct WriterActor;

#[derive(Debug)]
struct WriterState {
    conn: SqliteConnection,
}

impl Default for WriterActor {
    fn default() -> Self {
        WriterActor
    }
}

#[async_trait]
impl Actor for WriterActor {
    type Msg = DbWriteMsg;
    type State = WriterState;
    type Arguments = String; // db_url

    async fn pre_start(
        &self,
        _me: ActorRef<Self::Msg>,
        db_url: String,
    ) -> std::result::Result<Self::State, ActorProcessingErr> {
        // Build/create the DB file if needed
        let filename = db_url
            .strip_prefix("sqlite://")
            .unwrap_or(&db_url)
            .to_string();
        let mut conn = SqliteConnectOptions::from_str(&filename)
            .map_err(to_actor_err)?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5))
            .connect()
            .await
            .map_err(to_actor_err)?;

        // Additional pragmatic PRAGMAs (safe defaults for WAL workloads)
        // Note: use best-effort; if they fail, bubble via SetupSnafu.
        sqlx::query("PRAGMA synchronous = NORMAL")
            .execute(&mut conn)
            .await
            .context(SetupSnafu)?;
        sqlx::query("PRAGMA temp_store = MEMORY")
            .execute(&mut conn)
            .await
            .context(SetupSnafu)?;
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut conn)
            .await
            .context(SetupSnafu)?;

        Ok(WriterState { conn })
    }

    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        msg: DbWriteMsg,
        state: &mut WriterState,
    ) -> std::result::Result<(), ActorProcessingErr> {
        match msg {
            DbWriteMsg::ExecBatch { statements, reply } => {
                let res = exec_batch(&mut state.conn, statements).await;
                if let Some(rp) = reply {
                    let _ = rp.send(res);
                }
            }
            DbWriteMsg::CheckpointWal { reply } => {
                let res = async {
                    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                        .execute(&mut state.conn)
                        .await
                        .context(ExecuteSnafu)?;
                    Ok::<(), SqliteDbError>(())
                }
                .await;
                if let Some(rp) = reply {
                    let _ = rp.send(res);
                }
            }
        }
        Ok(())
    }
}

/// Execute a batch inside `BEGIN IMMEDIATE … COMMIT`, with **rollback on first error**.
async fn exec_batch(
    conn: &mut SqliteConnection,
    statements: Vec<(String, Vec<Bind>)>,
) -> Result<usize> {
    let mut total = 0usize;

    // Acquire the write lock up-front.
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *conn)
        .await
        .context(ExecuteSnafu)?;

    for (sql, binds) in statements {
        let mut q = sqlx::query(&sql);
        for b in binds {
            q = match b {
                Bind::Text(s) => q.bind(s),
                Bind::Integer(i) => q.bind(i),
                Bind::Real(r) => q.bind(r),
                Bind::Blob(b) => q.bind(b),
                Bind::Null => {
                    let none: Option<i32> = None;
                    q.bind(none)
                }
            };
        }

        match q.execute(&mut *conn).await {
            Ok(res) => total += res.rows_affected() as usize,
            Err(e) => {
                // Best-effort rollback; surface the original error
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                return Err(SqliteDbError::Execute { source: e });
            }
        }
    }

    sqlx::query("COMMIT")
        .execute(&mut *conn)
        .await
        .context(ExecuteSnafu)?;

    Ok(total)
}

/// Per-URL writer actor registry (singleton per DB URL).
static WRITERS: LazyLock<Mutex<HashMap<String, Arc<OnceCell<ActorRef<DbWriteMsg>>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Internal: ensure (find or spawn) the writer actor for `db_url`.
pub(crate) async fn ensure_writer(db_url: &str) -> Result<ActorRef<DbWriteMsg>> {
    let cell_arc = {
        let mut map = WRITERS.lock().unwrap();
        map.entry(db_url.to_string())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone()
    };

    let ar_ref = cell_arc
        .get_or_try_init(|| async {
            let (ar, _jh) = ractor::spawn::<WriterActor>(db_url.to_string())
                .await
                .map_err(|e| SqliteDbError::ActorCall { msg: e.to_string() })?;
            Ok::<ActorRef<DbWriteMsg>, SqliteDbError>(ar)
        })
        .await?;

    Ok(ar_ref.clone())
}

/// Public: execute a batch of SQL statements **atomically** for the given DB URL.
pub async fn exec_batch_write(db_url: &str, statements: Vec<(String, Vec<Bind>)>) -> Result<usize> {
    let actor = ensure_writer(db_url).await?;
    // ractor::call! (sync in 0.15)
    let outer = ractor::call!(actor, |rp| DbWriteMsg::ExecBatch {
        statements,
        reply: Some(rp)
    });
    let inner: Result<usize> =
        outer.map_err(|e| SqliteDbError::ActorCall { msg: e.to_string() })?;
    inner
}

/// Public: request a WAL checkpoint (TRUNCATE) for the given DB URL.
pub async fn checkpoint_wal(db_url: &str) -> Result<()> {
    let actor = ensure_writer(db_url).await?;
    let outer = ractor::call!(actor, |rp| DbWriteMsg::CheckpointWal { reply: Some(rp) });
    let inner: Result<()> = outer.map_err(|e| SqliteDbError::ActorCall { msg: e.to_string() })?;
    inner
}

/// Public: execute a batch of SQL statements **atomically** for the given DB URL.
pub async fn exec_fetch_all(
    db_url: &str,
    statement: (String, Vec<Bind>),
) -> Result<Vec<sqlx::sqlite::SqliteRow>> {
    let (sql, binds) = statement;
    let mut q = sqlx::query(&sql);
    for b in binds {
        q = match b {
            Bind::Text(s) => q.bind(s),
            Bind::Integer(i) => q.bind(i),
            Bind::Real(r) => q.bind(r),
            Bind::Blob(b) => q.bind(b),
            Bind::Null => {
                let none: Option<i32> = None;
                q.bind(none)
            }
        };
    }

    let pool = get_read_only_pool(db_url).await?;
    match q.fetch_all(&pool).await {
        Ok(rows) => Ok(rows),
        Err(e) => Err(SqliteDbError::Execute { source: e }),
    }
}

/// Small helper to convert any displayable error into ActorProcessingErr.
fn to_actor_err(e: impl std::fmt::Display) -> ActorProcessingErr {
    ActorProcessingErr::from(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snafu::ResultExt;
    use sqlx::Executor;
    use tempfile::tempdir;
    use tokio::time::{timeout, Duration};

    // ── helpers ───────────────────────────────────────────────────────────────

    // Create a unique sqlite:// URL in a temp dir (dir kept alive by return value)
    fn temp_db_url(name: &str) -> (tempfile::TempDir, String) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join(format!("{name}.db"));
        let url = format!("sqlite://{}", path.to_string_lossy());
        (dir, url)
    }

    // ── tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn read_only_pool_is_read_only_and_wal() -> Result<()> {
        let (_dir, db_url) = temp_db_url("pool_ro");

        // Prepare schema with a writable connection (create file + WAL/PRAGMAs).
        {
            let mut conn = sqlx::sqlite::SqliteConnectOptions::from_str(&db_url)
                .map_err(|e| SqliteDbError::ActorCall { msg: e.to_string() })?
                .create_if_missing(true)
                .foreign_keys(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .connect()
                .await
                .context(ConnectSnafu)?;
            conn.execute("PRAGMA journal_mode = WAL")
                .await
                .context(ExecuteSnafu)?;
            conn.execute("PRAGMA foreign_keys = ON")
                .await
                .context(ExecuteSnafu)?;
        }

        // Get the read-only pool (singleton)
        let pool = get_read_only_pool(&db_url).await?;
        let pool2 = get_read_only_pool(&db_url).await?;

        // Both pools can read
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master")
            .fetch_one(&pool)
            .await
            .context(ExecuteSnafu)?;
        let count2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master")
            .fetch_one(&pool2)
            .await
            .context(ExecuteSnafu)?;
        assert_eq!(count, count2);

        // Writes via the read-only pool should fail
        let write_res = sqlx::query("CREATE TABLE t(x INTEGER)")
            .execute(&pool)
            .await;
        assert!(write_res.is_err(), "read-only pool allowed a write!");

        Ok(())
    }

    #[tokio::test]
    async fn basic_exec_flow_and_reads() -> Result<()> {
        let (_dir, db_url) = temp_db_url("basic_exec");

        // Create table via public write API
        exec_batch_write(
            &db_url,
            vec![(
                r#"CREATE TABLE IF NOT EXISTS docs(
                    id TEXT PRIMARY KEY,
                    content TEXT NOT NULL,
                    updated_at INTEGER NOT NULL
                )"#
                .to_string(),
                vec![],
            )],
        )
        .await?;

        // Insert a row
        let n = exec_batch_write(
            &db_url,
            vec![(
                "INSERT INTO docs(id,content,updated_at) VALUES(?,?,?)".to_string(),
                vec![
                    Bind::Text("alpha".into()),
                    Bind::Text("hello".into()),
                    Bind::Integer(123),
                ],
            )],
        )
        .await?;
        assert_eq!(n, 1);

        // Read via read-only pool
        let pool = get_read_only_pool(&db_url).await?;
        let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM docs")
            .fetch_one(&pool)
            .await
            .context(ExecuteSnafu)?;
        assert_eq!(cnt, 1);

        Ok(())
    }

    #[tokio::test]
    async fn exec_batch_error_rolls_back() -> Result<()> {
        let (_dir, db_url) = temp_db_url("exec_error_rb");

        exec_batch_write(
            &db_url,
            vec![(
                "CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY, val TEXT NOT NULL)"
                    .to_string(),
                vec![],
            )],
        )
        .await?;

        // First ok, second invalid -> whole batch rolls back (no partial write)
        let res = exec_batch_write(
            &db_url,
            vec![
                (
                    "INSERT INTO t(id,val) VALUES(?,?)".to_string(),
                    vec![Bind::Integer(1), Bind::Text("ok".into())],
                ),
                (
                    "INSERT INTO ttt(id,val) VALUES(?,?)".to_string(),
                    vec![Bind::Integer(2), Bind::Text("nope".into())],
                ),
            ],
        )
        .await;

        assert!(matches!(res, Err(SqliteDbError::Execute { .. })));

        let pool = get_read_only_pool(&db_url).await?;
        let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
            .fetch_one(&pool)
            .await
            .context(ExecuteSnafu)?;
        assert_eq!(cnt, 0);

        Ok(())
    }

    #[tokio::test]
    async fn checkpoint_wal_ok() -> Result<()> {
        let (_dir, db_url) = temp_db_url("checkpoint");
        exec_batch_write(
            &db_url,
            vec![(
                "CREATE TABLE IF NOT EXISTS t(x INTEGER PRIMARY KEY, y TEXT)".to_string(),
                vec![],
            )],
        )
        .await?;

        exec_batch_write(
            &db_url,
            (0..50)
                .map(|i| {
                    (
                        "INSERT INTO t(x,y) VALUES(?,?)".to_string(),
                        vec![Bind::Integer(i), Bind::Text(format!("v{i}"))],
                    )
                })
                .collect(),
        )
        .await?;

        checkpoint_wal(&db_url).await?;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_write_bursts_result_in_expected_counts() -> Result<()> {
        let (_dir, db_url) = temp_db_url("concurrent_bursts");

        exec_batch_write(
            &db_url,
            vec![(
                "CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY, val INTEGER NOT NULL)"
                    .to_string(),
                vec![],
            )],
        )
        .await?;

        let burst = |start: i64, n: i64| {
            let url = db_url.clone();
            async move {
                exec_batch_write(
                    &url,
                    (0..n)
                        .map(|k| {
                            (
                                "INSERT INTO t(id,val) VALUES(?,?)".to_string(),
                                vec![Bind::Integer(start + k), Bind::Integer(start + k)],
                            )
                        })
                        .collect(),
                )
                .await
            }
        };

        let f1 = burst(1, 100);
        let f2 = burst(101, 100);
        let f3 = burst(201, 100);

        let joined = timeout(Duration::from_secs(10), async { tokio::join!(f1, f2, f3) })
            .await
            .map_err(|_| SqliteDbError::ActorCall {
                msg: "timeout waiting for bursts".into(),
            })?;

        let (r1, r2, r3) = joined;
        assert_eq!(r1?, 100);
        assert_eq!(r2?, 100);
        assert_eq!(r3?, 100);

        let pool = get_read_only_pool(&db_url).await?;
        let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
            .fetch_one(&pool)
            .await
            .context(ExecuteSnafu)?;
        assert_eq!(cnt, 300);

        Ok(())
    }

    #[tokio::test]
    async fn bind_types_work_and_null_is_accepted() -> Result<()> {
        let (_dir, db_url) = temp_db_url("bind_types");

        exec_batch_write(
            &db_url,
            vec![(
                "CREATE TABLE IF NOT EXISTS v(a TEXT, b INTEGER, c REAL, d BLOB, e TEXT)"
                    .to_string(),
                vec![],
            )],
        )
        .await?;

        let n = exec_batch_write(
            &db_url,
            vec![(
                "INSERT INTO v(a,b,c,d,e) VALUES(?,?,?,?,?)".to_string(),
                vec![
                    Bind::Text("hello".into()),
                    Bind::Integer(42),
                    Bind::Real(3.14),
                    Bind::Blob(vec![1, 2, 3, 4]),
                    Bind::Null,
                ],
            )],
        )
        .await?;
        assert_eq!(n, 1);

        let pool = get_read_only_pool(&db_url).await?;
        let (a, b, c, d_len, e_is_null): (String, i64, f64, i64, Option<String>) =
            sqlx::query_as("SELECT a, b, c, length(d), e FROM v")
                .fetch_one(&pool)
                .await
                .context(ExecuteSnafu)?;
        assert_eq!(a, "hello");
        assert_eq!(b, 42);
        assert!((c - 3.14).abs() < 1e-9);
        assert_eq!(d_len, 4);
        assert!(e_is_null.is_none());

        Ok(())
    }
}
