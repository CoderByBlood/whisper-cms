use crate::file::FileService;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

/// Shared app context and error
pub struct AppCtx {
    root: PathBuf,
    file_service: Arc<FileService>,
}

impl AppCtx {
    pub fn new(root_dir: &Path, file_service: FileService) -> Self {
        Self {
            root: root_dir.to_path_buf(),
            file_service: Arc::new(file_service),
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root.as_path()
    }

    pub fn file_service(&self) -> &FileService {
        &self.file_service
    }
}

#[derive(Error, Debug)]
pub enum AppError {
    #[error("{0}")]
    Msg(String),
}
