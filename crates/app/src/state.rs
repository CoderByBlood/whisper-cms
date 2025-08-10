use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct AppState {
    pub plan: Arc<RwLock<Option<types::InstallPlan>>>,
    pub progress: Arc<RwLock<Option<tokio::sync::broadcast::Sender<crate::install::progress::Msg>>>>,
    pub phase: Arc<crate::phase::PhaseState>, // NEW
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            plan: Arc::new(RwLock::new(None)),
            progress: Arc::new(RwLock::new(None)),
            phase: crate::phase::PhaseState::new(),
        }
    }
}