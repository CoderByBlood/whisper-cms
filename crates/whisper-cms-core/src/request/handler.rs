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
    async fn router(&self) -> Router {
        Router::new()
    }
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
}

pub struct BootingHandler;

#[async_trait]
impl RequestHandler for BootingHandler {
    async fn router(&self) -> Router {
        Router::new().fallback(axum::routing::any(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Html("<h1>Server is booting. Please try again shortly.</h1>"),
            )
        }))
    }
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
}

pub struct ConfiguringHandler;

#[async_trait]
impl RequestHandler for ConfiguringHandler {
    async fn router(&self) -> Router {
        Router::new()
            .nest_service("/", ServeDir::new("static/config-spa"))
            .fallback(axum::routing::any(|| async {
                match tokio::fs::read_to_string("static/config-spa/index.html").await {
                    Ok(content) => Html(content).into_response(),
                    Err(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to load index",
                    ).into_response(),
                }
            }))
    }
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
}

pub struct InstallingHandler;

#[async_trait]
impl RequestHandler for InstallingHandler {
    async fn router(&self) -> Router {
        Router::new()
            .nest_service("/", ServeDir::new("static/install-spa"))
            .fallback(axum::routing::any(|| async {
                match tokio::fs::read_to_string("static/install-spa/index.html").await {
                    Ok(content) => Html(content).into_response(),
                    Err(_) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to load index",
                    ).into_response(),
                }
            }))
    }
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
}

pub struct ServingHandler;

#[async_trait]
impl RequestHandler for ServingHandler {
    async fn router(&self) -> Router {
        Router::new()
            .route("/", get(home_page))
            .nest_service("/static", ServeDir::new("static"))
            .fallback(axum::routing::any(|| async {
                (StatusCode::NOT_FOUND, "Page not found").into_response()
            }))
    }
    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        Ok(())
    }
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