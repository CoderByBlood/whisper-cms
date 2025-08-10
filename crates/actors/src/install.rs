use anyhow::Result;
use types::InstallPlan;

pub struct Coordinator;

impl Coordinator {
    #[tracing::instrument(skip_all)]
    pub fn new() -> Self {
        Self
    }
    #[tracing::instrument(skip_all)]
    pub async fn run(&self, _plan: InstallPlan) -> Result<()> {
        Ok(())
    }
}
