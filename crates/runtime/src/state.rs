#[derive(Clone)]
pub struct RunState {
//    pub ops: libsql::Connection,     // or pool
//    pub content: libsql::Connection, // or pool
    pub data_dir: std::path::PathBuf,
    pub db_url: String,
}
impl RunState {
    #[tracing::instrument(skip_all)]
    pub fn new(data_dir: std::path::PathBuf, db_url: String) -> Self {
        Self { data_dir, db_url }
    }
}
