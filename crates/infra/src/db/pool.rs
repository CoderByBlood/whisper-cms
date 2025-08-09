
use anyhow::Result;
use sqlx::{Pool, Any, any::AnyPoolOptions};
pub async fn connect(database_url: &str) -> Result<Pool<Any>> {
    Ok(AnyPoolOptions::new().max_connections(5).connect(database_url).await?)
}
