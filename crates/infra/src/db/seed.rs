use anyhow::Result;
use crate::db::Conn;

pub async fn baseline(_conn: &Conn) -> Result<()> {
    // optional: insert baseline records
    Ok(())
}