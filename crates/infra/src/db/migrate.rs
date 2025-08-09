
use anyhow::Result;
use sqlx::{migrate::Migrator, AnyPool};
static MIGRATOR: Migrator = sqlx::migrate!("./migrations/ops");
pub async fn run(pool: &AnyPool) -> Result<()> { MIGRATOR.run(pool).await?; Ok(()) }
