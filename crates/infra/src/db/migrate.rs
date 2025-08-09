use crate::db::Conn;
use anyhow::Result;
use std::{fs, path::Path};

/// Execute every `*.sql` in `migrations/ops` in name order, each as a transaction.
pub async fn run(conn: &Conn) -> Result<()> {
    let dir = Path::new("migrations/ops");
    if !dir.exists() {
        // no migrations yet; treat as success
        return Ok(());
    }

    let mut entries = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "sql").unwrap_or(false))
        .collect::<Vec<_>>();
    entries.sort_by_key(|e| e.path());

    for e in entries {
        let sql = fs::read_to_string(e.path())?;
        // Atomically execute the whole file
        conn.execute_transactional_batch(&sql).await?;
    }
    Ok(())
}
