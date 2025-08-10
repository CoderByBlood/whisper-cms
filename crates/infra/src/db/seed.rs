use crate::db::Conn;
use anyhow::Result;

#[tracing::instrument(skip_all)]
pub async fn baseline(_conn: &Conn) -> Result<()> {
    // optional: insert baseline records
    Ok(())
}
