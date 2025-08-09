use std::sync::{Arc, RwLock};
use types::InstallPlan;

#[derive(Clone, Default)]
pub struct AppState {
    pub plan: Arc<RwLock<Option<InstallPlan>>>,
}