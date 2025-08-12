// crates/infra/src/db/conn.rs
use anyhow::{anyhow, Result};
use libsql::{Builder, Connection};
use std::path::Path;

pub type Conn = Connection;

#[tracing::instrument(skip_all)]
pub async fn connect(database_url: &str) -> Result<Conn> {
    connect_with_token(database_url, None).await
}

#[tracing::instrument(skip_all)]
pub async fn connect_with_token(database_url: &str, token_opt: Option<&str>) -> Result<Conn> {
    // Embedded sqlite file or bare path
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

    // Remote libsql
    if database_url.starts_with("libsql://") || database_url.starts_with("https://") || database_url.starts_with("http://") {
        // Prefer the passed-in token; fall back to env for compatibility
        let token = match token_opt {
            Some(t) if !t.is_empty() => t.to_owned(),
            _ => std::env::var("LIBSQL_AUTH_TOKEN")
                    .or_else(|_| std::env::var("TURSO_AUTH_TOKEN"))
                    .map_err(|_| anyhow!("LIBSQL_AUTH_TOKEN (or TURSO_AUTH_TOKEN) not set and no token provided"))?,
        };

        let db = Builder::new_remote(database_url.to_string(), token).build().await?;
        return Ok(db.connect()?);
    }

    Err(anyhow!("unsupported database_url: {database_url}"))
}