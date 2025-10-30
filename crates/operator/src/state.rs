use crate::phase::PhaseState;
use crate::progress::Msg;
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};
use tokio::sync::broadcast;
use types::InstallPlan;

#[derive(Clone)]
pub struct OperState {
    pub paths: infra::config::paths::Paths,   // <— add this
    pub auth_dir: PathBuf,
    pub plan: Arc<RwLock<Option<InstallPlan>>>,
    pub progress: Arc<RwLock<Option<broadcast::Sender<Msg>>>>,
    pub phase: Arc<PhaseState>,
    site_dir: PathBuf,
}

impl OperState {
    pub fn new(site_dir: impl Into<PathBuf>) -> Self {
        let site_dir = site_dir.into();
        let paths = infra::config::paths::Paths::new(site_dir.clone());

        let auth_dir = std::env::var_os("WHISPERCMS_AUTH_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| site_dir.join(".whisper/auth"));

        Self {
            paths,
            auth_dir,
            plan: Arc::new(RwLock::new(None)),
            progress: Arc::new(RwLock::new(None)),
            phase: PhaseState::new(),
            site_dir,
        }
    }

    pub fn site_dir(&self) -> &PathBuf {
        &self.site_dir
    }
}