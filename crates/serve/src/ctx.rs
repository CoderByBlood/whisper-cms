use std::path::{Path, PathBuf};
use thiserror::Error;

/// Shared app context and error
pub struct AppCtx {
    dir: PathBuf,
}

impl AppCtx {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn root_dir(&self) -> &Path {
        &self.dir.as_path()
    }
}

#[derive(Error, Debug)]
pub enum AppError {
    #[error("{0}")]
    Msg(String),
}
