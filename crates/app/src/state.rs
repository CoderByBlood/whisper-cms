use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use types::InstallPlan;

#[derive(Clone)]
pub struct AppState {
    pub plan: Arc<RwLock<Option<InstallPlan>>>,
    // Set to Some(sender) when a run starts; SSE subscribers will subscribe to it.
    pub progress: Arc<RwLock<Option<broadcast::Sender<crate::install::progress::Msg>>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            plan: Arc::new(RwLock::new(None)),
            progress: Arc::new(RwLock::new(None)),
        }
    }
}