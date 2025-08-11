use crate::phase::PhaseState;
use crate::progress::Msg;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use tokio::sync::broadcast;
use types::InstallPlan;

#[derive(Clone)]
pub struct OperState {
    site_dir: PathBuf,
    pub auth_dir: PathBuf,
    pub plan: Arc<RwLock<Option<InstallPlan>>>,
    pub progress: Arc<RwLock<Option<broadcast::Sender<Msg>>>>,
    pub phase: Arc<PhaseState>,
}

impl OperState {
    #[tracing::instrument(skip_all)]
    pub fn new(site_dir: impl Into<PathBuf>) -> Self {
        let site_dir = site_dir.into();
        let auth_dir = std::env::var_os("WHISPERCMS_AUTH_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| site_dir.join(".whisper/auth"));

        Self {
            site_dir,
            auth_dir,
            plan: Arc::new(RwLock::new(None)),
            progress: Arc::new(RwLock::new(None)),
            phase: PhaseState::new(),
        }
    }

    #[allow(dead_code)]
    #[tracing::instrument(skip_all)]
    pub fn site_dir(&self) -> &Path {
        &self.site_dir
    }
}
