// crates/infra/src/db/conn.rs
use anyhow::{anyhow, Result};
use libsql::{Builder, Connection};
use std::path::Path;

pub type Conn = Connection;

#[tracing::instrument(skip_all)]
pub async fn connect(database_url: &str) -> Result<Conn> {
    if database_url.starts_with("sqlite://") || !database_url.contains("://") {
        let path = database_url
            .strip_prefix("sqlite://")
            .unwrap_or(database_url);
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Builder::new_local(path).build().await?;
        return Ok(db.connect()?);
    }
    if database_url.starts_with("libsql://") {
        let token = std::env::var("LIBSQL_AUTH_TOKEN")
            .or_else(|_| std::env::var("TURSO_AUTH_TOKEN"))
            .map_err(|_| anyhow!("LIBSQL_AUTH_TOKEN (or TURSO_AUTH_TOKEN) not set"))?;
        let db = Builder::new_remote(database_url.to_string(), token)
            .build()
            .await?;
        return Ok(db.connect()?);
    }
    Err(anyhow!("unsupported database_url: {database_url}"))
}
