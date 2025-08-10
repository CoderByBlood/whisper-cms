use std::sync::{Arc, RwLock};
use tokio::sync::broadcast::Sender;

use crate::{phase::PhaseState, progress::Msg};

#[derive(Clone)]
pub struct OperState {
    pub plan: Arc<RwLock<Option<types::InstallPlan>>>,
    pub progress: Arc<RwLock<Option<Sender<Msg>>>>,
    pub phase: Arc<PhaseState>,
}

impl OperState {
    #[tracing::instrument(skip_all)]
    pub fn new() -> Self {
        Self {
            plan: Arc::new(RwLock::new(None)),
            progress: Arc::new(RwLock::new(None)),
            phase: PhaseState::new(),
        }
    }
}
