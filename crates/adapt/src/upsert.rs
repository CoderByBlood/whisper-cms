//! SQLite DDL + UPSERT builder and executor using SeaQuery 0.32.x.
//! - Builds SQL with SeaQuery and executes via an abstract async trait (`UpsertDbExec`).
//! - Ensures table exists exactly once (concurrency-safe) before upsert.
//! - Clean Architecture: no SeaQuery types leak through the trait.
//! - Arity mismatches return an error (no panics).
//! - Optional mock generation with `mockall::automock`.

use async_trait::async_trait;
use sea_query::{
    Alias, ColumnDef, Expr, Iden, OnConflict, Query, SimpleExpr, SqliteQueryBuilder, Table,
};
use std::{error::Error as StdError, fmt};
use tokio::sync::OnceCell;

// ─────────────────────────────────────────────────────────────────────────────
// Port / trait: infra implements this. No SeaQuery types leak here.
// ─────────────────────────────────────────────────────────────────────────────

/// Logical bind values your infra can translate to its own binder.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Text(String),
    Int(i32),
    Bool(bool),
    Blob(Vec<u8>),
    Float(f32),
    Double(f64),
    Json(String), // stored as TEXT on SQLite
}

#[cfg_attr(test, mockall::automock(type Error = ();))]
#[async_trait]
pub trait UpsertDbExec: Send + Sync {
    type Error: fmt::Debug + Send + Sync + 'static;

    /// Execute the provided SQL statements atomically if possible.
    /// Each pair is (sql, flattened binds in row-major order).
    async fn exec_batch_write(
        &self,
        statements: Vec<(String, Vec<SqlValue>)>,
    ) -> Result<usize, Self::Error>;

    /// Optional: request a WAL checkpoint.
    async fn checkpoint_wal(&self) -> Result<(), Self::Error>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Module errors (adapter-side) — generic over the DB error type.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum UpsertError<DE> {
    InvalidRow {
        table: String,
        expected: usize,
        got: usize,
    },
    BuildSql {
        msg: String,
    },
    Db(DE),
}
impl<DE: fmt::Display> fmt::Display for UpsertError<DE> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRow {
                table,
                expected,
                got,
            } => write!(
                f,
                "Invalid row for table '{table}': expected {expected}, got {got}"
            ),
            Self::BuildSql { msg } => write!(f, "Failed to build SQL: {msg}"),
            Self::Db(e) => write!(f, "DB operation failed: {e}"),
        }
    }
}
impl<DE: StdError + 'static> StdError for UpsertError<DE> {}

// ─────────────────────────────────────────────────────────────────────────────
// Declarative schema (data-only)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ColType {
    Text,
    Integer,
    Boolean,
    Binary,
    Float,
    Double,
    Json, // fine for SQLite (json1 works over TEXT)
    Custom(&'static str),
}

#[derive(Debug, Clone)]
pub struct ColumnSpec {
    pub name: &'static str,
    pub ty: ColType,
    pub is_pk: bool,
    pub is_not_null: bool,
    pub is_unique: bool,
    pub default: Option<SqlValue>,
}

#[derive(Debug, Clone)]
pub struct TableSpec {
    pub name: &'static str,
    pub columns: Vec<ColumnSpec>,
    /// If `Some`, the FIRST entry is used as the single conflict target.
    /// If `None`, we derive ONE column: PK first, else first UNIQUE, else first column.
    pub conflict_target: Option<Vec<&'static str>>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Upsert façade (SQLite dialect), backed by the UpsertDbExec port
// ─────────────────────────────────────────────────────────────────────────────

pub struct SqliteUpsert<'a, DB: UpsertDbExec> {
    db: &'a DB,
    init: OnceCell<()>,
}

impl<'a, DB: UpsertDbExec> SqliteUpsert<'a, DB> {
    pub fn new(db: &'a DB) -> Self {
        Self {
            db,
            init: OnceCell::new(),
        }
    }

    /// Ensure CREATE TABLE IF NOT EXISTS runs exactly once (concurrency-safe).
    async fn ensure_table(&self, spec: &TableSpec) -> Result<(), UpsertError<DB::Error>> {
        self.init
            .get_or_try_init(|| async {
                let sql = build_create_table_sql(spec);
                self.db
                    .exec_batch_write(vec![(sql, vec![])])
                    .await
                    .map(|_| ())
                    .map_err(UpsertError::Db)
            })
            .await
            .map(|_| ())
    }

    /// INSERT ... ON CONFLICT (<single-col>) DO UPDATE SET ... (or DO NOTHING if nothing to update)
    /// Returns total rows affected (as reported by the DB impl).
    pub async fn upsert_rows(
        &self,
        spec: &TableSpec,
        rows: &[Vec<SqlValue>],
    ) -> Result<usize, UpsertError<DB::Error>> {
        // 1) Ensure table exists first (idempotent)
        self.ensure_table(spec).await?;

        // 2) No-op if empty input
        if rows.is_empty() {
            return Ok(0);
        }

        // 3) Pre-validate shapes (no panic on arity mismatch)
        let expected = spec.columns.len();
        for (i, r) in rows.iter().enumerate() {
            if r.len() != expected {
                return Err(UpsertError::InvalidRow {
                    table: format!("{}[{}]", spec.name, i),
                    expected,
                    got: r.len(),
                });
            }
        }

        // 4) Build UPSERT SQL (placeholders) and flatten binds in row-major order
        let sql = build_upsert_sql(spec, rows).map_err(|e| UpsertError::BuildSql { msg: e })?;
        let binds: Vec<SqlValue> = rows.iter().flat_map(|r| r.clone()).collect();

        // 5) Execute via port
        self.db
            .exec_batch_write(vec![(sql, binds)])
            .await
            .map_err(UpsertError::Db)
    }

    /// Optional passthrough.
    pub async fn checkpoint_wal(&self) -> Result<(), UpsertError<DB::Error>> {
        self.db.checkpoint_wal().await.map_err(UpsertError::Db)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
fn build_create_table_sql(spec: &TableSpec) -> String {
    let mut stmt = Table::create();
    stmt.table(Id(spec.name)).if_not_exists();
    for col in &spec.columns {
        stmt.col(make_column_def(col));
    }
    stmt.to_string(SqliteQueryBuilder)
}

/// Choose exactly ONE conflict column:
/// 1) If `spec.conflict_target` provided, use its FIRST entry.
/// 2) Else the FIRST PK column.
/// 3) Else the FIRST UNIQUE column.
/// 4) Else the FIRST column in the table.
fn derive_single_conflict_col(spec: &TableSpec) -> Option<&'static str> {
    if let Some(cols) = &spec.conflict_target {
        return cols.first().copied();
    }
    if let Some(pk) = spec.columns.iter().find(|c| c.is_pk) {
        return Some(pk.name);
    }
    if let Some(uq) = spec.columns.iter().find(|c| c.is_unique) {
        return Some(uq.name);
    }
    spec.columns.first().map(|c| c.name)
}

fn build_upsert_sql(spec: &TableSpec, rows: &[Vec<SqlValue>]) -> Result<String, String> {
    let mut ins = Query::insert();
    ins.into_table(Id(spec.name));

    let col_idens = spec.columns.iter().map(|c| Id(c.name));
    ins.columns(col_idens);

    for r in rows {
        let exprs = r.iter().map(sqlvalue_to_expr);
        ins.values(exprs).map_err(|e| e.to_string())?;
    }

    // Single-column conflict target (per SQLite rules)
    let conflict_col = derive_single_conflict_col(spec);

    // Update all columns except the conflict target
    let update_cols: Vec<Id> = match conflict_col {
        Some(conf) => spec
            .columns
            .iter()
            .filter(|c| c.name != conf)
            .map(|c| Id(c.name))
            .collect(),
        None => vec![],
    };

    if let Some(conf) = conflict_col {
        if update_cols.is_empty() {
            // Nothing to update; prefer DO NOTHING to avoid invalid UPDATE
            ins.on_conflict(OnConflict::column(Id(conf)).do_nothing().to_owned());
        } else {
            ins.on_conflict(
                OnConflict::column(Id(conf))
                    .update_columns(update_cols)
                    .to_owned(),
            );
        }
    }
    Ok(ins.to_string(SqliteQueryBuilder))
}

fn make_column_def(spec: &ColumnSpec) -> ColumnDef {
    let mut cd = ColumnDef::new(Id(spec.name));
    match spec.ty {
        ColType::Text => {
            cd.text();
        }
        ColType::Integer => {
            cd.integer();
        }
        ColType::Boolean => {
            cd.boolean();
        }
        ColType::Binary => {
            cd.binary();
        }
        ColType::Float => {
            cd.float();
        }
        ColType::Double => {
            cd.double();
        }
        ColType::Json => {
            cd.json();
        } // SQLite: alias of TEXT, fine
        ColType::Custom(t) => {
            cd.custom(Alias::new(t));
        }
    }
    if spec.is_pk {
        cd.primary_key();
    }
    if spec.is_not_null {
        cd.not_null();
    }
    if spec.is_unique {
        cd.unique_key();
    }
    if let Some(default) = &spec.default {
        cd.default(sqlvalue_to_expr(default));
    }
    cd
}

fn sqlvalue_to_expr(v: &SqlValue) -> SimpleExpr {
    match v {
        SqlValue::Null => Expr::val(Option::<i32>::None).into(),
        SqlValue::Text(s) => Expr::val(s.clone()).into(),
        SqlValue::Int(i) => Expr::val(*i).into(),
        SqlValue::Bool(b) => Expr::val(*b).into(),
        SqlValue::Blob(b) => Expr::val(b.clone()).into(),
        SqlValue::Float(f) => Expr::val(*f).into(),
        SqlValue::Double(f) => Expr::val(*f).into(),
        // Store JSON as TEXT for SQLite (json1 operates on TEXT)
        SqlValue::Json(s) => Expr::val(s.clone()).into(),
    }
}

/// Identifier wrapper for dynamic names.
#[derive(Clone, Copy, Debug)]
struct Id(&'static str);
impl Iden for Id {
    fn unquoted(&self, s: &mut dyn fmt::Write) {
        let _ = write!(s, "{}", self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockall::Sequence;

    // ----- helpers -----------------------------------------------------------

    fn spec_users_conflict_none() -> TableSpec {
        // id is pk+unique; derivation should resolve to "id" only (single target)
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
            conflict_target: None,
        }
    }

    fn spec_users_conflict_explicit() -> TableSpec {
        let mut s = spec_users_conflict_none();
        s.conflict_target = Some(vec!["id"]); // first entry used
        s
    }

    fn is_create_stmt(stmts: &[(String, Vec<SqlValue>)]) -> bool {
        stmts.len() == 1 && stmts[0].0.to_lowercase().contains("create table")
    }

    fn is_insert_stmt(stmts: &[(String, Vec<SqlValue>)]) -> bool {
        stmts.len() == 1 && stmts[0].0.to_lowercase().starts_with("insert into")
    }

    // ----- tests -------------------------------------------------------------

    #[tokio::test]
    async fn empty_rows_only_creates_table_and_returns_zero() {
        let mut mock = MockUpsertDbExec::new();

        mock.expect_exec_batch_write()
            .times(1)
            .withf(|stmts| is_create_stmt(stmts))
            .returning(|_| Ok(0));

        let up = SqliteUpsert::new(&mock);
        let spec = spec_users_conflict_none();

        let n = up.upsert_rows(&spec, &[]).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn table_init_once_then_multiple_inserts() {
        let mut mock = MockUpsertDbExec::new();
        let mut seq = Sequence::new();

        mock.expect_exec_batch_write()
            .once()
            .in_sequence(&mut seq)
            .withf(|stmts| is_create_stmt(stmts))
            .returning(|_| Ok(0));

        mock.expect_exec_batch_write()
            .once()
            .in_sequence(&mut seq)
            .withf(|stmts| is_insert_stmt(stmts))
            .returning(|_| Ok(1));

        mock.expect_exec_batch_write()
            .once()
            .in_sequence(&mut seq)
            .withf(|stmts| is_insert_stmt(stmts))
            .returning(|_| Ok(1));

        let up = SqliteUpsert::new(&mock);
        let spec = spec_users_conflict_none();

        let n1 = up
            .upsert_rows(
                &spec,
                &[vec![
                    SqlValue::Int(1),
                    SqlValue::Text("a@example.com".into()),
                    SqlValue::Text("Alice".into()),
                ]],
            )
            .await
            .unwrap();
        assert_eq!(n1, 1);

        let n2 = up
            .upsert_rows(
                &spec,
                &[vec![
                    SqlValue::Int(2),
                    SqlValue::Text("b@example.com".into()),
                    SqlValue::Text("Bob".into()),
                ]],
            )
            .await
            .unwrap();
        assert_eq!(n2, 1);
    }

    #[tokio::test]
    async fn arity_mismatch_returns_error_and_does_not_call_insert() {
        let mut mock = MockUpsertDbExec::new();
        let mut seq = Sequence::new();

        mock.expect_exec_batch_write()
            .once()
            .in_sequence(&mut seq)
            .withf(|stmts| is_create_stmt(stmts))
            .returning(|_| Ok(0));

        let up = SqliteUpsert::new(&mock);
        let spec = spec_users_conflict_explicit();

        let err = up
            .upsert_rows(
                &spec,
                &[vec![
                    SqlValue::Int(1),
                    SqlValue::Text("only-two@x.com".into()),
                ]],
            )
            .await
            .unwrap_err();

        match err {
            UpsertError::InvalidRow { expected, got, .. } => {
                assert_eq!(expected, 3);
                assert_eq!(got, 2);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn conflict_target_derives_single_and_updates_non_conflict_columns() {
        let mut mock = MockUpsertDbExec::new();

        fn norm_sql(s: &str) -> String {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        }

        mock.expect_exec_batch_write()
            .times(2) // DDL + DML
            .withf(|stmts| {
                if is_create_stmt(stmts) {
                    return true;
                }
                if is_insert_stmt(stmts) {
                    let raw = &stmts[0].0;
                    let n = norm_sql(raw);

                    // Expect ON CONFLICT(id)
                    let has_conflict = n.contains("onconflictid");

                    // Expect DO UPDATE SET email=excluded.email, name=excluded.name (order may vary)
                    let updates_email = n.contains("doupdatesetemailexcludedemail");
                    let also_updates_name = n.contains("nameexcludedname");

                    // binds flattened in row-major order
                    let binds_ok = stmts[0].1
                        == vec![
                            SqlValue::Int(1),
                            SqlValue::Text("alpha@x.com".into()),
                            SqlValue::Text("Alpha".into()),
                        ];

                    return has_conflict && updates_email && also_updates_name && binds_ok;
                }
                false
            })
            .returning(|_| Ok(1));

        let up = SqliteUpsert::new(&mock);
        let spec = spec_users_conflict_none(); // derives to single "id"

        up.upsert_rows(
            &spec,
            &[vec![
                SqlValue::Int(1),
                SqlValue::Text("alpha@x.com".into()),
                SqlValue::Text("Alpha".into()),
            ]],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn bind_flattening_order_multi_rows_is_row_major() {
        let mut mock = MockUpsertDbExec::new();

        mock.expect_exec_batch_write()
            .times(2) // DDL + DML
            .withf(|stmts| {
                if is_create_stmt(stmts) {
                    return true;
                }
                if is_insert_stmt(stmts) {
                    let binds = &stmts[0].1;
                    let want = vec![
                        // row 1
                        SqlValue::Int(1),
                        SqlValue::Text("a@x".into()),
                        SqlValue::Text("A".into()),
                        // row 2
                        SqlValue::Int(2),
                        SqlValue::Text("b@x".into()),
                        SqlValue::Text("B".into()),
                    ];
                    return *binds == want;
                }
                false
            })
            .returning(|_| Ok(2));

        let up = SqliteUpsert::new(&mock);
        let spec = spec_users_conflict_none();

        let n = up
            .upsert_rows(
                &spec,
                &[
                    vec![
                        SqlValue::Int(1),
                        SqlValue::Text("a@x".into()),
                        SqlValue::Text("A".into()),
                    ],
                    vec![
                        SqlValue::Int(2),
                        SqlValue::Text("b@x".into()),
                        SqlValue::Text("B".into()),
                    ],
                ],
            )
            .await
            .unwrap();

        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn db_error_propagates_as_upserterror_db() {
        let mut mock = MockUpsertDbExec::new();
        let mut seq = Sequence::new();

        mock.expect_exec_batch_write()
            .once()
            .in_sequence(&mut seq)
            .withf(|stmts| is_create_stmt(stmts))
            .returning(|_| Ok(0));

        mock.expect_exec_batch_write()
            .once()
            .in_sequence(&mut seq)
            .withf(|stmts| is_insert_stmt(stmts))
            .returning(|_| Err(())); // mock error type is ()

        let up = SqliteUpsert::new(&mock);
        let spec = spec_users_conflict_explicit();

        let err = up
            .upsert_rows(
                &spec,
                &[vec![
                    SqlValue::Int(10),
                    SqlValue::Text("err@x.com".into()),
                    SqlValue::Text("Err".into()),
                ]],
            )
            .await
            .unwrap_err();

        match err {
            UpsertError::Db(()) => {} // expected
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn checkpoint_passthrough_success() {
        let mut mock = MockUpsertDbExec::new();
        mock.expect_checkpoint_wal().once().returning(|| Ok(()));

        let up = SqliteUpsert::new(&mock);
        up.checkpoint_wal().await.unwrap();
    }
}
