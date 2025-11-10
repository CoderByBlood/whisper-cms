use crate::file::FileService;
use domain::setting::Settings;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

/// Shared app context and error
pub struct AppCtx {
    root: Option<PathBuf>,
    settings: Option<Arc<Settings>>,
    file_service: Option<Arc<FileService>>,
}

impl AppCtx {
    pub fn new() -> Self {
        Self {
            root: None,
            settings: None,
            file_service: None,
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self
            .root
            .as_ref()
            .expect("Root directory not set")
            .as_path()
    }

    pub fn set_root(mut self, root: &Path) -> Self {
        self.root = Some(root.to_path_buf());
        self
    }

    pub fn settings(&self) -> &Settings {
        &self.settings.as_ref().expect("Settings not set")
    }

    pub fn set_settings(mut self, settings: Settings) -> Self {
        self.settings = Some(Arc::new(settings));
        self
    }

    pub fn file_service(&self) -> &FileService {
        &self.file_service.as_ref().expect("File service not set")
    }

    pub fn set_file_service(mut self, file_service: FileService) -> Self {
        self.file_service = Some(Arc::new(file_service));
        self
    }
}

#[derive(Error, Debug)]
pub enum AppError {
    /// Regex error
    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    /// Underlying I/O
    #[error("I/O error while reading: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Msg(String),
}
