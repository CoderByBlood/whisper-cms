// handler/mod.rs
use askama::Template;
use async_trait::async_trait;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use thiserror::Error;
use tower_http::services::ServeDir;

use axum::{body::Body, extract::Request};
use std::path::PathBuf;
use tower::{service_fn, Service};

use crate::request::ManagerError;

#[async_trait]
pub trait RequestHandler: Send + Sync {
    async fn router(&self) -> Router;
    async fn on_enter(&mut self) -> Result<(), ManagerError>;
    async fn on_exit(&mut self) -> Result<(), ManagerError>;
}

pub struct NoopHandler;

#[async_trait]
impl RequestHandler for NoopHandler {
    #[tracing::instrument(skip_all)]
    async fn router(&self) -> Router {
        Router::new()
    }
    #[tracing::instrument(skip_all)]
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
}

pub struct BootingHandler;

#[async_trait]
impl RequestHandler for BootingHandler {
    #[tracing::instrument(skip_all)]
    async fn router(&self) -> Router {
        Router::new().fallback(axum::routing::any(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Html("<h1>Server is booting. Please try again shortly.</h1>"),
            )
        }))
    }
    #[tracing::instrument(skip_all)]
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
}

pub struct ConfiguringHandler;

#[async_trait]
impl RequestHandler for ConfiguringHandler {
    #[tracing::instrument(skip_all)]
    async fn router(&self) -> Router {
        Router::new().fallback_service(
            ServeDir::new("static/config-spa")
                .not_found_service(spa_index("static/config-spa/index.html")),
        )
    }
    #[tracing::instrument(skip_all)]
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
}

pub struct InstallingHandler;

#[async_trait]
impl RequestHandler for InstallingHandler {
    #[tracing::instrument(skip_all)]
    async fn router(&self) -> Router {
        Router::new().fallback_service(
            ServeDir::new("static/install-spa")
                .not_found_service(spa_index("static/install-spa/index.html")),
        )
    }
    #[tracing::instrument(skip_all)]
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
}

pub struct ServingHandler;

#[async_trait]
impl RequestHandler for ServingHandler {
    #[tracing::instrument(skip_all)]
    async fn router(&self) -> Router {
        Router::new()
            .route("/", get(home_page))
            .nest_service("/static", ServeDir::new("static"))
            .fallback(axum::routing::any(|| async {
                (StatusCode::NOT_FOUND, "Page not found").into_response()
            }))
    }
    #[tracing::instrument(skip_all)]
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
}

#[derive(Template)]
#[template(path = "home.html")]
pub struct HomeTemplate {
    pub title: String,
    pub message: String,
}

async fn home_page() -> Result<impl IntoResponse, ManagerError> {
    let template = HomeTemplate {
        title: "Welcome".into(),
        message: "This is rendered with Askama.".into(),
    };

    Ok(Html(template.render()?))
}

#[tracing::instrument(skip_all)]
fn spa_index(
    index_path: &str,
) -> impl Service<
    Request<Body>,
    Response = axum::response::Response,
    Error = std::convert::Infallible,
    Future = impl Send + 'static, // 👈 ensure the Future is Send
> + Clone
       + Send
       + 'static {
    let path = PathBuf::from(index_path);
    service_fn(move |_req: Request<Body>| {
        let path = path.clone();
        async move {
            let result = tokio::fs::read_to_string(path).await;
            let response = match result {
                Ok(content) => Html(content).into_response(),
                Err(_) => (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to load index",
                )
                    .into_response(),
            };
            Ok::<_, std::convert::Infallible>(response)
        }
    })
}
