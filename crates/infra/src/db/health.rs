use crate::db::Conn;
use anyhow::Result;

/// Lightweight readiness check: run a trivial SELECT successfully.
#[tracing::instrument(skip_all)]
pub async fn ready(conn: &Conn) -> Result<()> {
    let mut rows = conn.query("SELECT 1", ()).await?;
    // touch the first row to force execution
    let _ = rows.next().await?;
    Ok(())
}

/// Optional: deeper file integrity check (slower than `ready`).
#[allow(dead_code)]
#[tracing::instrument(skip_all)]
pub async fn quick_check(conn: &Conn) -> Result<()> {
    let mut rows = conn.query("PRAGMA quick_check", ()).await?;
    // `quick_check` returns one or more rows; "ok" means healthy
    while let Some(row) = rows.next().await? {
        let s: String = row.get(0)?;
        if s != "ok" {
            anyhow::bail!("libsql quick_check reported: {s}");
        }
    }
    Ok(())
}
