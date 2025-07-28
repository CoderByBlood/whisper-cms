// handler/mod.rs
use askama::Template;
use async_trait::async_trait;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
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

pub enum ReqHandler {
    Noop(NoopHandler),
    Booting(BootingHandler),
    Configuring(ConfiguringHandler),
    Installing(InstallingHandler),
    Serving(ServingHandler),
}

#[async_trait]
impl RequestHandler for ReqHandler {
    async fn router(&self) -> Router {
        match self {
            ReqHandler::Noop(h) => h.router().await,
            ReqHandler::Booting(h) => h.router().await,
            ReqHandler::Configuring(h) => h.router().await,
            ReqHandler::Installing(h) => h.router().await,
            ReqHandler::Serving(h) => h.router().await,
        }
    }

    async fn on_enter(&mut self) -> Result<(), ManagerError> {
        match self {
            ReqHandler::Noop(h) => h.on_enter().await,
            ReqHandler::Booting(h) => h.on_enter().await,
            ReqHandler::Configuring(h) => h.on_enter().await,
            ReqHandler::Installing(h) => h.on_enter().await,
            ReqHandler::Serving(h) => h.on_enter().await,
        }
    }

    async fn on_exit(&mut self) -> Result<(), ManagerError> {
        match self {
            ReqHandler::Noop(h) => h.on_exit().await,
            ReqHandler::Booting(h) => h.on_exit().await,
            ReqHandler::Configuring(h) => h.on_exit().await,
            ReqHandler::Installing(h) => h.on_exit().await,
            ReqHandler::Serving(h) => h.on_exit().await,
        }
    }
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

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt; // for `oneshot`


    use super::*;
    use http_body_util::BodyExt;
    use std::fs;
    use tempfile::NamedTempFile; // for .collect().await

    #[tokio::test]
    async fn noop_handler_router() {
        let handler = NoopHandler;
        let app = handler.router().await;

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn noop_handler_lifecycle() {
        let mut handler = NoopHandler;
        assert!(handler.on_enter().await.is_ok());
        assert!(handler.on_exit().await.is_ok());
    }

    #[tokio::test]
    async fn booting_handler_router_status() {
        let handler = BootingHandler;
        let app = handler.router().await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&body_bytes);
        assert!(body.contains("Server is booting"));
    }

    #[tokio::test]
    async fn booting_handler_lifecycle() {
        let mut handler = BootingHandler;
        assert!(handler.on_enter().await.is_ok());
        assert!(handler.on_exit().await.is_ok());
    }

    #[tokio::test]
    async fn configuring_handler_fallback_exists() {
        let index = NamedTempFile::new().unwrap();
        fs::write(index.path(), "<html>config spa</html>").unwrap();
        let index_path = index.path().to_str().unwrap();

        let service = spa_index(index_path);
        let req = Request::builder()
            .uri("/unknown")
            .body(Body::empty())
            .unwrap();
        let response = service.oneshot(req).await.unwrap();

        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);

        assert_eq!(status, StatusCode::OK);
        assert!(text.contains("config spa"));
    }

    #[tokio::test]
    async fn configuring_handler_fallback_missing() {
        let service = spa_index("/this/does/not/exist.html");
        let req = Request::builder()
            .uri("/unknown")
            .body(Body::empty())
            .unwrap();
        let response = service.oneshot(req).await.unwrap();

        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(text.contains("Failed to load index"));
    }

    #[tokio::test]
    async fn configuring_handler_lifecycle() {
        let mut handler = ConfiguringHandler;
        assert!(handler.on_enter().await.is_ok());
        assert!(handler.on_exit().await.is_ok());
    }

    #[tokio::test]
    async fn installing_handler_fallback_exists() {
        let index = NamedTempFile::new().unwrap();
        fs::write(index.path(), "<html>install spa</html>").unwrap();
        let index_path = index.path().to_str().unwrap();

        let service = spa_index(index_path);
        let req = Request::builder()
            .uri("/install")
            .body(Body::empty())
            .unwrap();
        let response = service.oneshot(req).await.unwrap();

        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);

        assert_eq!(status, StatusCode::OK);
        assert!(text.contains("install spa"));
    }

    #[tokio::test]
    async fn installing_handler_fallback_missing() {
        let service = spa_index("/missing/install/index.html");
        let req = Request::builder()
            .uri("/install")
            .body(Body::empty())
            .unwrap();
        let response = service.oneshot(req).await.unwrap();

        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&body);

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(text.contains("Failed to load index"));
    }

    #[tokio::test]
    async fn installing_handler_lifecycle() {
        let mut handler = InstallingHandler;
        assert!(handler.on_enter().await.is_ok());
        assert!(handler.on_exit().await.is_ok());
    }

    #[tokio::test]
    async fn serving_handler_home() {
        let handler = ServingHandler;
        let app = handler.router().await;

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn serving_handler_404() {
        let handler = ServingHandler;
        let app = handler.router().await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn serving_handler_lifecycle() {
        let mut handler = ServingHandler;
        assert!(handler.on_enter().await.is_ok());
        assert!(handler.on_exit().await.is_ok());
    }

    #[tokio::test]
    async fn req_handler_on_enter_all_variants() {
        let mut handlers = vec![
            ReqHandler::Noop(NoopHandler),
            ReqHandler::Booting(BootingHandler),
            ReqHandler::Configuring(ConfiguringHandler),
            ReqHandler::Installing(InstallingHandler),
            ReqHandler::Serving(ServingHandler),
        ];

        for handler in &mut handlers {
            assert!(handler.on_enter().await.is_ok());
        }
    }

    #[tokio::test]
    async fn req_handler_on_exit_all_variants() {
        let mut handlers = vec![
            ReqHandler::Noop(NoopHandler),
            ReqHandler::Booting(BootingHandler),
            ReqHandler::Configuring(ConfiguringHandler),
            ReqHandler::Installing(InstallingHandler),
            ReqHandler::Serving(ServingHandler),
        ];

        for handler in &mut handlers {
            assert!(handler.on_exit().await.is_ok());
        }
    }

    #[tokio::test]
    async fn req_handler_router_all_variants() {
        let handlers = vec![
            ReqHandler::Noop(NoopHandler),
            ReqHandler::Booting(BootingHandler),
            ReqHandler::Configuring(ConfiguringHandler),
            ReqHandler::Installing(InstallingHandler),
            ReqHandler::Serving(ServingHandler),
        ];

        for handler in &handlers {
            let app = handler.router().await;
            let response = app
                .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
                .await;
            assert!(response.is_ok());
        }
    }
}
