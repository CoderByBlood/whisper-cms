
use anyhow::Result; use sqlx::AnyPool;
pub async fn ready(_pool: &AnyPool) -> Result<()> { Ok(()) }
