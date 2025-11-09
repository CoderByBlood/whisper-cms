mod sqlite;

use serve::db::{DatabaseError, DatabaseService, SqlValue};
use sqlite::{Bind, SqliteDbError};
use sqlx::{Column, Row, Sqlite, Type, TypeInfo, ValueRef};
use std::collections::HashMap;
use std::future::Future;
use std::sync::LazyLock;

const _SRV: DatabaseService =
    DatabaseService::from_fns(exec_batch_write, exec_fetch_all, checkpoint_wal);

// ─────────────────────────────────────────────────────────────────────────────
// Robust sync bridge: a private global current-thread runtime
// We never assume the caller is inside Tokio (avoids deadlocks/panics).
// ─────────────────────────────────────────────────────────────────────────────
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build upsert port runtime")
});

pub type Result<T> = std::result::Result<T, DatabaseError>;

#[inline]
fn run_async<F: Future>(fut: F) -> F::Output {
    RUNTIME.block_on(fut)
}

// ─────────────────────────────────────────────────────────────────────────────

pub fn exec_batch_write(db_url: &str, statements: Vec<(String, Vec<SqlValue>)>) -> Result<usize> {
    run_async(async {
        // Map adapter-level SqlValue → infra-level Bind
        let mapped: Vec<(String, Vec<Bind>)> = statements
            .into_iter()
            .map(|(sql, vals)| (sql, vals.into_iter().map(sqlvalue_to_bind).collect()))
            .collect();

        Ok(sqlite::exec_batch_write(db_url, mapped).await?)
    })
}

pub fn exec_fetch_all(
    db_url: &str,
    statement: (String, Vec<SqlValue>),
) -> Result<Vec<HashMap<String, SqlValue>>> {
    run_async(async {
        // Map adapter-level SqlValue → infra-level Bind
        let (sql, vals) = statement;
        let mapped = (
            sql,
            vals.into_iter().map(sqlvalue_to_bind).collect::<Vec<_>>(),
        );

        // Execute via sqlite.rs (returns Vec<SqliteRow>)
        let rows = sqlite::exec_fetch_all(db_url, mapped).await?;
        let mut out = Vec::with_capacity(rows.len());

        for row in rows {
            let mut row_map = HashMap::with_capacity(row.columns().len());

            for col in row.columns() {
                let name = col.name();

                // 1) NULL short-circuit
                let raw = row.try_get_raw(name).map_err(SqliteDbError::src)?;
                if raw.is_null() {
                    row_map.insert(name.to_string(), SqlValue::Null);
                    continue;
                }

                // 2) Declared and runtime type info
                let declared = col.type_info(); // schema-declared
                let runtime = raw.type_info(); // actual value
                let decl_up = declared.name().to_ascii_uppercase();

                // 3) Declared-type hints
                let declared_is_bool = decl_up.contains("BOOL"); // BOOLEAN / BOOL
                let declared_is_int = decl_up.contains("INT"); // INT / INTEGER / SMALLINT / BIGINT
                let declared_is_big = decl_up.contains("BIG"); // BIGINT etc.

                // Heuristic: treat "id" columns as PK-like → prefer i64
                let is_pkish_id = name.eq_ignore_ascii_case("id");

                // 4) Decode
                let val: SqlValue = if declared_is_bool {
                    let b: bool = row.try_get(name).map_err(SqliteDbError::src)?;
                    SqlValue::Bool(b)
                } else if <i64 as Type<Sqlite>>::compatible(&runtime) {
                    // INTEGER affinity in SQLite
                    if declared_is_int && !declared_is_big && !is_pkish_id {
                        // Prefer i32 for “normal” integer columns when it fits.
                        match row.try_get::<i32, _>(name) {
                            Ok(v32) => SqlValue::Int(v32),
                            Err(_) => {
                                let v64: i64 = row.try_get(name).map_err(SqliteDbError::src)?;
                                SqlValue::Long(v64)
                            }
                        }
                    } else {
                        // BIGINT, or id/PK-like, or any other case → i64
                        let v64: i64 = row.try_get(name).map_err(SqliteDbError::src)?;
                        SqlValue::Long(v64)
                    }
                } else if <f64 as Type<Sqlite>>::compatible(&runtime) {
                    let f: f64 = row.try_get(name).map_err(SqliteDbError::src)?;
                    SqlValue::Double(f)
                } else if <String as Type<Sqlite>>::compatible(&runtime) {
                    let s: String = row.try_get(name).map_err(SqliteDbError::src)?;
                    SqlValue::Text(s)
                } else if <Vec<u8> as Type<Sqlite>>::compatible(&runtime) {
                    let b: Vec<u8> = row.try_get(name).map_err(SqliteDbError::src)?;
                    SqlValue::Blob(b)
                } else {
                    return Err(SqliteDbError::msg(format!(
                        "unhandled SQLite type for column `{}`: declared={:?}, runtime={:?}",
                        name, declared, runtime
                    ))
                    .into());
                };

                row_map.insert(name.to_string(), val);
            }

            out.push(row_map);
        }

        Ok(out)
    })
}

pub fn checkpoint_wal(db_url: &str) -> Result<()> {
    run_async(async { Ok(sqlite::checkpoint_wal(db_url).await?) })
}

// -------------------- helpers --------------------

fn sqlvalue_to_bind(v: SqlValue) -> Bind {
    match v {
        SqlValue::Null => Bind::Null,
        SqlValue::Text(s) => Bind::Text(s),
        SqlValue::Int(i) => Bind::Integer(i as i64), // narrow → wide
        SqlValue::Long(i) => Bind::Integer(i),
        SqlValue::Bool(b) => Bind::Integer(if b { 1 } else { 0 }), // SQLite bool as INTEGER
        SqlValue::Blob(b) => Bind::Blob(b),
        SqlValue::Float(f) => Bind::Real(f as f64), // promote f32 → f64
        SqlValue::Double(f) => Bind::Real(f),
        SqlValue::Json(s) => Bind::Text(s), // JSON stored as TEXT (json1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn temp_db_url(name: &str) -> (tempfile::TempDir, String) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join(format!("{name}.db"));
        let url = format!("sqlite://{}", path.to_string_lossy());
        (dir, url)
    }

    #[test]
    fn create_table_insert_and_select_roundtrip_all_types() {
        let (_dir, db_url) = temp_db_url("port_roundtrip");

        // CREATE TABLE (note JSON stored as TEXT, BOOLEAN as INTEGER 0/1)
        let create = (
            r#"
            CREATE TABLE IF NOT EXISTS t(
                id       INTEGER PRIMARY KEY,
                name     TEXT NOT NULL,
                active   BOOLEAN NOT NULL,
                small_i  INTEGER,
                big_i    INTEGER,
                price_f  REAL,
                score_d  REAL,
                payload  BLOB,
                meta     TEXT,    -- JSON-as-TEXT
                note     TEXT     -- may be NULL
            )
            "#
            .to_string(),
            vec![],
        );
        let n = exec_batch_write(&db_url, vec![create]).expect("create ok");
        assert_eq!(n, 0, "DDL reports 0 rows changed");

        // Insert one row with all types covered; JSON as a string
        let blob = vec![1u8, 2, 3, 4, 5];
        let insert = (
            "INSERT INTO t(id,name,active,small_i,big_i,price_f,score_d,payload,meta,note) \
             VALUES (?,?,?,?,?,?,?,?,?,?)"
                .to_string(),
            vec![
                SqlValue::Long(1),                     // id
                SqlValue::Text("alice".into()),        // name
                SqlValue::Bool(true),                  // active
                SqlValue::Int(7),                      // small_i
                SqlValue::Long(1_i64 << 40),           // big_i large > i32
                SqlValue::Float(3.5),                  // price_f (f32 promoted to f64 on write)
                SqlValue::Double(9.25),                // score_d
                SqlValue::Blob(blob.clone()),          // payload
                SqlValue::Json(r#"{"k":"v"}"#.into()), // meta as TEXT
                SqlValue::Null,                        // note -> NULL
            ],
        );
        let n = exec_batch_write(&db_url, vec![insert]).expect("insert ok");
        assert_eq!(n, 1);

        // Select back via adapter (tests NULL-first, type matches, mapping)
        let rows =
            exec_fetch_all(&db_url, ("SELECT * FROM t".to_string(), vec![])).expect("fetch ok");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];

        assert_eq!(row.get("id"), Some(&SqlValue::Long(1)));
        assert_eq!(row.get("name"), Some(&SqlValue::Text("alice".into())));
        assert_eq!(row.get("active"), Some(&SqlValue::Bool(true)));
        assert_eq!(row.get("small_i"), Some(&SqlValue::Int(7)));
        assert_eq!(row.get("big_i"), Some(&SqlValue::Long(1_i64 << 40)));
        assert_eq!(row.get("price_f"), Some(&SqlValue::Double(3.5_f64)));
        assert_eq!(row.get("score_d"), Some(&SqlValue::Double(9.25)));
        assert_eq!(row.get("payload"), Some(&SqlValue::Blob(blob)));
        // JSON comes back as Text (by design)
        assert_eq!(
            row.get("meta"),
            Some(&SqlValue::Text(r#"{"k":"v"}"#.into()))
        );
        // NULL mapping
        assert_eq!(row.get("note"), Some(&SqlValue::Null));
    }

    #[test]
    fn large_integer_prefers_long() {
        let (_dir, db_url) = temp_db_url("port_large_int");

        exec_batch_write(
            &db_url,
            vec![(
                "CREATE TABLE IF NOT EXISTS t(x INTEGER)".to_string(),
                vec![],
            )],
        )
        .expect("create ok");

        let val = 9_000_000_000_i64; // beyond i32
        exec_batch_write(
            &db_url,
            vec![(
                "INSERT INTO t(x) VALUES (?)".to_string(),
                vec![SqlValue::Long(val)],
            )],
        )
        .expect("insert ok");

        let rows =
            exec_fetch_all(&db_url, ("SELECT x FROM t".to_string(), vec![])).expect("fetch ok");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("x"), Some(&SqlValue::Long(val)));
    }

    #[test]
    fn null_handling_is_first_class() {
        let (_dir, db_url) = temp_db_url("port_nulls");

        exec_batch_write(
            &db_url,
            vec![(
                "CREATE TABLE IF NOT EXISTS t(a INTEGER, b TEXT)".to_string(),
                vec![],
            )],
        )
        .expect("create ok");

        exec_batch_write(
            &db_url,
            vec![(
                "INSERT INTO t(a,b) VALUES (?,?)".to_string(),
                vec![SqlValue::Null, SqlValue::Null],
            )],
        )
        .expect("insert ok");

        let rows =
            exec_fetch_all(&db_url, ("SELECT a,b FROM t".to_string(), vec![])).expect("fetch ok");
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.get("a"), Some(&SqlValue::Null));
        assert_eq!(r.get("b"), Some(&SqlValue::Null));
    }

    #[test]
    fn bad_sql_surfaces_error_from_exec_batch_write() {
        let (_dir, db_url) = temp_db_url("port_bad_sql");

        // Bad statement -> should return Err from adapter (no panic)
        let err = exec_batch_write(
            &db_url,
            vec![("INSRT INTO nope VALUES (1)".to_string(), vec![])],
        )
        .err();
        assert!(err.is_some(), "expected an error for bad SQL");
    }

    #[test]
    fn fetch_all_on_missing_table_returns_error() {
        let (_dir, db_url) = temp_db_url("port_fetch_bad");

        // No table exists; SELECT should error
        let err = exec_fetch_all(&db_url, ("SELECT * FROM nope".to_string(), vec![])).err();
        assert!(
            err.is_some(),
            "expected an error when selecting from missing table"
        );
    }

    #[test]
    fn checkpoint_wal_passthrough_ok() {
        let (_dir, db_url) = temp_db_url("port_ckpt");

        exec_batch_write(
            &db_url,
            vec![(
                "CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY, v TEXT)".to_string(),
                vec![],
            )],
        )
        .expect("create ok");

        // Some writes (so checkpoint actually has something to consider)
        for i in 0..50 {
            exec_batch_write(
                &db_url,
                vec![(
                    "INSERT INTO t(id,v) VALUES (?,?)".to_string(),
                    vec![SqlValue::Long(i), SqlValue::Text(format!("v{i}"))],
                )],
            )
            .expect("insert ok");
        }

        // WAL checkpoint through the adapter
        checkpoint_wal(&db_url).expect("checkpoint ok");

        // Ensure DB file exists and is non-empty (rough sanity)
        let path = db_url.trim_start_matches("sqlite://");
        let meta = fs::metadata(path).expect("db file exists");
        assert!(meta.len() > 0);
    }

    #[test]
    fn non_pk_integer_widens_when_out_of_i32_range() {
        let (_dir, db_url) = temp_db_url("port_widen_nonpk");

        exec_batch_write(
            &db_url,
            vec![(
                "CREATE TABLE IF NOT EXISTS t(n INTEGER NOT NULL)".to_string(),
                vec![],
            )],
        )
        .expect("create ok");

        // value just beyond i32::MAX should come back as Long
        let wide = (i32::MAX as i64) + 1;
        exec_batch_write(
            &db_url,
            vec![(
                "INSERT INTO t(n) VALUES (?)".to_string(),
                vec![SqlValue::Long(wide)],
            )],
        )
        .expect("insert ok");

        let rows = exec_fetch_all(&db_url, ("SELECT n FROM t".to_string(), vec![])).expect("fetch");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("n"), Some(&SqlValue::Long(wide)));
    }

    #[test]
    fn boolean_declared_column_maps_to_bool_but_integer_0_1_stays_numeric() {
        let (_dir, db_url) = temp_db_url("port_bool_vs_int");

        exec_batch_write(
            &db_url,
            vec![(
                "CREATE TABLE IF NOT EXISTS t(b BOOLEAN, i INTEGER)".to_string(),
                vec![],
            )],
        )
        .expect("create ok");

        exec_batch_write(
            &db_url,
            vec![(
                "INSERT INTO t(b,i) VALUES (?,?)".to_string(),
                vec![SqlValue::Bool(true), SqlValue::Int(1)],
            )],
        )
        .expect("insert ok");

        let rows =
            exec_fetch_all(&db_url, ("SELECT b,i FROM t".to_string(), vec![])).expect("fetch");
        assert_eq!(rows.len(), 1);
        let r = &rows[0];

        // Declared BOOLEAN should be decoded as Bool(true)
        assert_eq!(r.get("b"), Some(&SqlValue::Bool(true)));

        // Declared INTEGER with 0/1 should remain numeric, not Bool
        assert_eq!(r.get("i"), Some(&SqlValue::Int(1)));
    }
}
