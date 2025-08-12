use anyhow::Result;
use libsql::params;
use crate::db::conn::Conn;
use domain::config::admin::AdminConfig;
use time::OffsetDateTime;

#[tracing::instrument(skip_all)]
pub async fn baseline(
    conn: &Conn,
    admin: &AdminConfig,
    site_name: &str,
    base_url: &str,
    timezone: &str,
) -> Result<()> {
    let now = OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "now".into());

    // admin user (idempotent)
    conn.execute(
        r#"
        INSERT OR IGNORE INTO users (username, password_hash, created_at)
        VALUES (?1, ?2, ?3);
        "#,
        params!["admin", admin.password_hash.as_str(), now.as_str()],
    ).await?;

    // site row (id=1) (idempotent)
    conn.execute(
        r#"
        INSERT OR IGNORE INTO site (id, name, base_url, timezone, created_at)
        VALUES (1, ?1, ?2, ?3, ?4);
        "#,
        params![site_name, base_url, timezone, now.as_str()],
    ).await?;

    Ok(())
}