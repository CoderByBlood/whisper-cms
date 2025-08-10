use axum::{routing::get, Router};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::ServiceExt as TowerServiceExt;
use tower_http::services::ServeDir; // for .oneshot()

use crate::install::{
    actions::{post_config, post_run},
    progress::sse_progress,
    routes::{get_config, get_done, get_home, get_maint, get_run, get_welcome},
};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Install,
    Serve,
}

#[derive(Debug)]
enum Handler {
    Noop,
    Boot,
    Install,
    Serve,
}

impl Handler {
    #[tracing::instrument(skip_all)]
    async fn router(&self, app: &AppState) -> Router {
        match self {
            Handler::Noop => Router::new(),
            Handler::Boot => boot_router(),
            Handler::Install => install_router(app),
            Handler::Serve => serve_router(app),
        }
    }
    #[tracing::instrument(skip_all)]
    async fn on_enter(&mut self, _app: &AppState) -> anyhow::Result<()> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self, _app: &AppState) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tracing::instrument(skip_all)]
fn boot_router() -> Router {
    Router::new()
        .nest_service("/static", ServeDir::new("crates/app/static"))
        .route("/", get(get_maint))
        .fallback(get(get_maint))
}

#[tracing::instrument(skip_all)]
fn install_router(app: &AppState) -> Router {
    Router::new()
        .nest_service("/static", ServeDir::new("crates/app/static"))
        .route("/install", get(get_welcome).post(post_run))
        .route("/install/config", get(get_config).post(post_config))
        .route("/install/run", get(get_run).post(post_run))
        .route("/install/progress", get(sse_progress))
        .route("/install/done", get(get_done))
        .route("/", get(get_maint))
        .fallback(get(get_maint))
        .with_state(app.clone())
}

#[tracing::instrument(skip_all)]
fn serve_router(app: &AppState) -> Router {
    Router::new()
        .nest_service("/static", ServeDir::new("crates/app/static"))
        .route("/", get(get_home))
        .fallback(get(get_home))
        .with_state(app.clone())
}

#[derive(Debug)]
pub struct PhaseState {
    handler: RwLock<Handler>,
}

impl PhaseState {
    #[tracing::instrument(skip_all)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            handler: RwLock::new(Handler::Boot),
        })
    }

    /// One-way swap of the active router; no dynamic dispatch; no awaits under the lock.
    #[tracing::instrument(skip_all)]
    pub async fn transition_to(&self, app: &AppState, next: Phase) -> anyhow::Result<()> {
        // Swap to Noop quickly, then drop the lock before awaiting.
        let mut guard = self.handler.write().await;
        let mut old = std::mem::replace(&mut *guard, Handler::Noop);
        drop(guard);

        old.on_exit(app).await?;

        let mut new = match next {
            Phase::Install => Handler::Install,
            Phase::Serve => Handler::Serve,
        };
        new.on_enter(app).await?;

        let mut guard = self.handler.write().await;
        *guard = new;
        Ok(())
    }

    /// Dispatch a request to the current phase’s router.
    /// Read-lock is released before awaiting the inner service.
    #[tracing::instrument(skip_all)]
    pub async fn dispatch(
        &self,
        app: AppState,
        req: axum::http::Request<axum::body::Body>,
    ) -> axum::response::Response {
        let router = {
            let h = self.handler.read().await;
            h.router(&app).await
        };
        router
            .oneshot(req)
            .await
            .expect("phase router should be infallible")
    }
}
