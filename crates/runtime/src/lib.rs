pub mod app;
pub mod router;
pub mod state;
pub mod home;

/// Start the server (library entrypoint)
#[tracing::instrument(skip_all)]
pub async fn run(cfg: app::RunCfg) -> anyhow::Result<()> {
    app::run(cfg).await
}