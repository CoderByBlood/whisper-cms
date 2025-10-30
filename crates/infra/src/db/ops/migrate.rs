use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};
use libsql::params;
use crate::db::conn::Conn;
use crate::config::paths;
use std::{fs, path::PathBuf};

static EMBEDDED_MIGRATIONS: Dir =
    include_dir!("$CARGO_MANIFEST_DIR/src/db/ops/migrations");

#[tracing::instrument(skip_all)]
pub async fn run(conn: &Conn) -> Result<()> {
    ensure_table(conn).await?;

    let mut migrations = load_disk(paths::schema_ops_dir())?;
    if migrations.is_empty() {
        migrations = load_embedded()?;
    }

    for (name, sql) in migrations {
        if applied(conn, &name).await? {
            continue;
        }
        apply_sql(conn, &sql).await
            .with_context(|| format!("apply migration {}", name))?;
        record(conn, &name).await?;
    }
    Ok(())
}

async fn ensure_table(conn: &Conn) -> Result<()> {
    conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS schema_migrations (
          version    TEXT PRIMARY KEY,
          applied_at TEXT NOT NULL
        );
        "#,
        (),
    ).await?;
    Ok(())
}

async fn applied(conn: &Conn, version: &str) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM schema_migrations WHERE version = ?1 LIMIT 1",
            params![version],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

async fn record(conn: &Conn, version: &str) -> Result<()> {
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "now".into());

    conn.execute(
        "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
        params![version, now.as_str()],
    ).await?;
    Ok(())
}

async fn apply_sql(conn: &Conn, sql: &str) -> Result<()> {
    conn.execute(sql, ()).await?;
    Ok(())
}

fn load_disk(dir: PathBuf) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    if !dir.exists() { return Ok(out); }
    let mut entries: Vec<_> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sql") { continue; }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let sql = fs::read_to_string(&path)?;
        out.push((name, sql));
    }
    Ok(out)
}

fn load_embedded() -> Result<Vec<(String, String)>> {
    let mut files: Vec<_> = EMBEDDED_MIGRATIONS.files().collect();
    files.sort_by_key(|f| f.path());
    let mut out = Vec::new();
    for f in files {
        let name = f.path().file_name().unwrap().to_string_lossy().to_string();
        let sql = f.contents_utf8().context("migration not utf-8")?.to_owned();
        out.push((name, sql));
    }
    Ok(out)
}