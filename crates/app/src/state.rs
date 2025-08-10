use crate::install::progress::Msg;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast::Sender;

#[derive(Clone)]
pub struct AppState {
    pub plan: Arc<RwLock<Option<types::InstallPlan>>>,
    pub progress: Arc<RwLock<Option<Sender<Msg>>>>,
    pub phase: Arc<crate::phase::PhaseState>, // NEW
}

impl Default for AppState {
    #[tracing::instrument(skip_all)]
    fn default() -> Self {
        Self {
            plan: Arc::new(RwLock::new(None)),
            progress: Arc::new(RwLock::new(None)),
            phase: crate::phase::PhaseState::new(),
        }
    }
}
