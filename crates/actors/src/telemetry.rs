use tracing::{error, info};

pub fn step_started(name: &str) {
    info!(install.step = name, "step started");
}
pub fn step_ok(name: &str) {
    info!(install.step = name, "step ok");
}
pub fn step_failed(name: &str, err: &str) {
    error!(install.step = name, %err, "step failed");
}
