
use anyhow::Result;
use types::InstallPlan;

pub struct Coordinator;

impl Coordinator {
    pub fn new() -> Self { Self }
    pub async fn run(&self, _plan: InstallPlan) -> Result<()> { Ok(()) }
}
