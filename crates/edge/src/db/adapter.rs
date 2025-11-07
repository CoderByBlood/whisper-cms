// edge/src/db/adapter.rs (or wherever you keep infra adapters)

use crate::db::sqlite; // edge::db::sqlite
use crate::db::sqlite::Bind; // writer-actor Bind type
use adapt::upsert::{SqlValue, UpsertDbExec};
use async_trait::async_trait; // your adapters crate

/// Concrete executor that targets our sqlite.rs (single-writer actor + RO pool).
#[derive(Debug, Clone)]
pub struct EdgeSqliteExec {
    db_url: String,
}

impl EdgeSqliteExec {
    pub fn new(db_url: impl Into<String>) -> Self {
        Self {
            db_url: db_url.into(),
        }
    }

    pub fn db_url(&self) -> &str {
        &self.db_url
    }
}

#[async_trait]
impl UpsertDbExec for EdgeSqliteExec {
    type Error = sqlite::DbError;

    async fn exec_batch_write(
        &self,
        statements: Vec<(String, Vec<SqlValue>)>,
    ) -> Result<usize, Self::Error> {
        // Map adapter-level SqlValue → infra-level Bind
        let mapped: Vec<(String, Vec<Bind>)> = statements
            .into_iter()
            .map(|(sql, vals)| (sql, vals.into_iter().map(sqlvalue_to_bind).collect()))
            .collect();

        sqlite::exec_batch_write(&self.db_url, mapped).await
    }

    async fn checkpoint_wal(&self) -> Result<(), Self::Error> {
        sqlite::checkpoint_wal(&self.db_url).await
    }
}

// -------------------- helpers --------------------

fn sqlvalue_to_bind(v: SqlValue) -> Bind {
    match v {
        SqlValue::Null => Bind::Null,
        SqlValue::Text(s) => Bind::Text(s),
        SqlValue::Int(i) => Bind::Integer(i as i64), // SQLite integer
        SqlValue::Bool(b) => Bind::Integer(if b { 1 } else { 0 }),
        SqlValue::Blob(b) => Bind::Blob(b),
        SqlValue::Float(f) => Bind::Real(f as f64), // promote f32 → f64
        SqlValue::Double(f) => Bind::Real(f),
        SqlValue::Json(s) => Bind::Text(s), // stored as TEXT; json1 works on TEXT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite; // the infra module with get_read_only_pool(...)
    use adapt::upsert::{ColType, ColumnSpec, SqlValue, SqliteUpsert, TableSpec};
    use tempfile::tempdir;

    // ----- helpers -----------------------------------------------------------

    fn spec_users() -> TableSpec {
        TableSpec {
            name: "users",
            columns: vec![
                ColumnSpec {
                    name: "id",
                    ty: ColType::Integer,
                    is_pk: true,
                    is_not_null: true,
                    is_unique: true,
                    default: None,
                },
                ColumnSpec {
                    name: "email",
                    ty: ColType::Text,
                    is_pk: false,
                    is_not_null: true,
                    is_unique: true,
                    default: None,
                },
                ColumnSpec {
                    name: "name",
                    ty: ColType::Text,
                    is_pk: false,
                    is_not_null: false,
                    is_unique: false,
                    default: None,
                },
            ],
            conflict_target: None, // derives to (id, email)
        }
    }

    fn temp_db_url(tag: &str) -> (tempfile::TempDir, String) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join(format!("{tag}.db"));
        let url = format!("sqlite://{}", path.to_string_lossy());
        (dir, url)
    }

    // ----- tests -------------------------------------------------------------

    #[tokio::test]
    async fn e2e_create_then_insert_then_read_via_pool() -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, db_url) = temp_db_url("adapter_create_insert_read");
        let exec = EdgeSqliteExec::new(&db_url);
        let up = SqliteUpsert::new(&exec);
        let spec = spec_users();

        // Create + insert one row through the adapter
        let changed = up
            .upsert_rows(
                &spec,
                &[vec![
                    SqlValue::Int(1),
                    SqlValue::Text("a@example.com".into()),
                    SqlValue::Text("Alice".into()),
                ]],
            )
            .await?;
        assert_eq!(changed, 1);

        // Read back using the read-only pool from sqlite.rs
        let pool = sqlite::get_read_only_pool(&db_url).await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 1);

        let (name,): (String,) = sqlx::query_as("SELECT name FROM users WHERE id = 1")
            .fetch_one(&pool)
            .await?;
        assert_eq!(name, "Alice");
        Ok(())
    }

    #[tokio::test]
    async fn e2e_conflict_updates_non_conflict_columns() -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, db_url) = temp_db_url("adapter_conflict_update");
        let exec = EdgeSqliteExec::new(&db_url);
        let up = SqliteUpsert::new(&exec);
        let spec = spec_users();

        // Seed two rows
        up.upsert_rows(
            &spec,
            &[
                vec![
                    SqlValue::Int(1),
                    SqlValue::Text("a@example.com".into()),
                    SqlValue::Text("Alice".into()),
                ],
                vec![
                    SqlValue::Int(2),
                    SqlValue::Text("b@example.com".into()),
                    SqlValue::Text("Bob".into()),
                ],
            ],
        )
        .await?;

        // Upsert with same (id,email) but different name -> should update name
        let changed = up
            .upsert_rows(
                &spec,
                &[vec![
                    SqlValue::Int(1),
                    SqlValue::Text("a@example.com".into()),
                    SqlValue::Text("Alicia".into()),
                ]],
            )
            .await?;
        assert_eq!(changed, 1); // updated one row

        // Verify: still 2 rows, and id=1 has new name
        let pool = sqlite::get_read_only_pool(&db_url).await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 2);

        let (name,): (String,) = sqlx::query_as("SELECT name FROM users WHERE id = 1")
            .fetch_one(&pool)
            .await?;
        assert_eq!(name, "Alicia");
        Ok(())
    }

    #[tokio::test]
    async fn e2e_empty_rows_is_noop_but_table_exists() -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, db_url) = temp_db_url("adapter_empty_rows");
        let exec = EdgeSqliteExec::new(&db_url);
        let up = SqliteUpsert::new(&exec);
        let spec = spec_users();

        // No rows: should create table and return 0
        let changed = up.upsert_rows(&spec, &[]).await?;
        assert_eq!(changed, 0);

        // Confirm table exists by issuing a SELECT via the read-only pool
        let pool = sqlite::get_read_only_pool(&db_url).await?;
        // Count returns 0 (table exists, no rows)
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn e2e_bool_and_blob_mapping_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, db_url) = temp_db_url("adapter_bool_blob");
        let exec = EdgeSqliteExec::new(&db_url);
        let up = SqliteUpsert::new(&exec);

        // Custom table to exercise Bool and Blob mappings through the adapter
        let spec = TableSpec {
            name: "flags",
            columns: vec![
                ColumnSpec {
                    name: "id",
                    ty: ColType::Integer,
                    is_pk: true,
                    is_not_null: true,
                    is_unique: true,
                    default: None,
                },
                ColumnSpec {
                    name: "ok",
                    ty: ColType::Boolean,
                    is_pk: false,
                    is_not_null: true,
                    is_unique: false,
                    default: None,
                },
                ColumnSpec {
                    name: "bytes",
                    ty: ColType::Binary,
                    is_pk: false,
                    is_not_null: false,
                    is_unique: false,
                    default: None,
                },
            ],
            conflict_target: None,
        };

        let payload = vec![1u8, 2, 3, 4];
        let changed = up
            .upsert_rows(
                &spec,
                &[vec![
                    SqlValue::Int(7),
                    SqlValue::Bool(true),
                    SqlValue::Blob(payload.clone()),
                ]],
            )
            .await?;
        assert_eq!(changed, 1);

        // Read via read-only pool
        let pool = sqlite::get_read_only_pool(&db_url).await?;
        // SQLite stores booleans as 0/1 INTEGER
        let (ok_int, bytes_len): (i64, i64) =
            sqlx::query_as("SELECT ok, length(bytes) FROM flags WHERE id = 7")
                .fetch_one(&pool)
                .await?;
        assert_eq!(ok_int, 1);
        assert_eq!(bytes_len, payload.len() as i64);
        Ok(())
    }

    #[tokio::test]
    async fn e2e_checkpoint_passthrough_does_not_break_reads(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, db_url) = temp_db_url("adapter_checkpoint");
        let exec = EdgeSqliteExec::new(&db_url);
        let up = SqliteUpsert::new(&exec);
        let spec = spec_users();

        up.upsert_rows(
            &spec,
            &[vec![
                SqlValue::Int(1),
                SqlValue::Text("x@x".into()),
                SqlValue::Text("X".into()),
            ]],
        )
        .await?;

        // Through adapter (→ sqlite.rs)
        up.checkpoint_wal().await?;

        let pool = sqlite::get_read_only_pool(&db_url).await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 1);
        Ok(())
    }
}
