use std::sync::{atomic::{AtomicBool, Ordering}, Arc, RwLock};
use tokio::sync::broadcast;
use types::InstallPlan;

#[derive(Clone)]
pub struct AppState {
    installed: Arc<AtomicBool>,
    pub plan: Arc<RwLock<Option<InstallPlan>>>,
    // Set to Some(sender) when a run starts; SSE subscribers will subscribe to it.
    pub progress: Arc<RwLock<Option<broadcast::Sender<crate::install::progress::Msg>>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            installed: Arc::new(AtomicBool::new(false)),
            plan: Arc::new(RwLock::new(None)),
            progress: Arc::new(RwLock::new(None)),
        }
    }
}

impl AppState {
  pub fn set_installed(&self, v: bool) { self.installed.store(v, Ordering::Relaxed); }
  pub fn is_installed(&self) -> bool { self.installed.load(Ordering::Relaxed) }
}