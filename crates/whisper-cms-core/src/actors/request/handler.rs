use askama::Template;
use async_trait::async_trait;
use axum::{
    extract::State,
    http::StatusCode,
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Form, Router,
};
use base64::{engine::general_purpose, Engine as _};
use hmac::{Hmac, Mac};
use hyper::HeaderMap;
use rand::Rng;
use serde::Deserialize;
use sha2::Sha256;
use tower_http::services::ServeDir;

use axum_extra::extract::cookie::{Cookie, CookieJar};
use tracing::{debug, info};

use axum::{body::Body, extract::Request};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::{service_fn, Service, ServiceExt};

use crate::actors::{
    config::ValidatedPassword,
    request::{Checkpoint, RequestError},
};
#[derive(Debug)]
pub struct RequestManager {
    state: Arc<ManagerState>,
    address: String,
    port: u16,
}

impl RequestManager {
    #[tracing::instrument(skip_all)]
    pub fn new(password: ValidatedPassword, address: String, port: u16) -> RequestManager {
        // Start in Booting state
        let initial_handler = Box::new(ReqHandler::Booting(BootingHandler));
        let state = Arc::new(ManagerState {
            //phase: ManagerPhase::Booting,
            manager: Arc::new(SessionManager::new(password)),
            handler: tokio::sync::RwLock::new(initial_handler),
        });
        RequestManager {
            state,
            address,
            port,
        }
    }

    #[tracing::instrument(skip_all)]
    pub async fn boot(&mut self, checkpoint: &Checkpoint) -> Result<(), RequestError> {
        self.state.transition_to(ManagerPhase::Booting).await?;

        match checkpoint {
            Checkpoint::Deferred => self.state.transition_to(ManagerPhase::Configuring).await?,
            Checkpoint::Configured => self.state.transition_to(ManagerPhase::Installing).await?,
            Checkpoint::Installed => self.state.transition_to(ManagerPhase::Serving).await?,
            Checkpoint::Provisioned => self.state.transition_to(ManagerPhase::Serving).await?,
        }

        // Fallback router
        let router = Router::new()
            .fallback(Self::dispatch_request)
            .with_state(self.state.clone());

        let ip: IpAddr = self.address.parse()?;
        let addr = SocketAddr::new(ip, self.port);

        info!("Listening on http://{}", addr);

        // Use hyper 1.6.0 compatible server setup
        let listener = tokio::net::TcpListener::bind(addr).await?;
        debug!("listener: {:?}", &listener);
        let serve = axum::serve(listener, router.into_make_service());
        debug!("serving: {serve:?}");
        tokio::spawn(async { serve.await });
        debug!("awaiting");

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn dispatch_request(
        State(mgr_state): State<Arc<ManagerState>>,
        req: Request<Body>,
    ) -> impl IntoResponse {
        debug!("{mgr_state:?} is handling {req:?}");
        let handler = mgr_state.handler.read().await;
        let router = handler.router().await;
        router.oneshot(req).await
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ManagerPhase {
    Booting,
    Configuring,
    Installing,
    Serving,
}

#[derive(Debug)]
pub struct ManagerState {
    //pub phase: ManagerPhase,
    manager: Arc<SessionManager>,
    pub handler: tokio::sync::RwLock<Box<ReqHandler>>,
}

impl ManagerState {
    #[tracing::instrument(skip_all)]
    pub async fn transition_to(&self, next: ManagerPhase) -> Result<(), RequestError> {
        let mut handler_guard = self.handler.write().await;
        let mut old_handler =
            std::mem::replace(&mut *handler_guard, Box::new(ReqHandler::Noop(NoopHandler)));
        old_handler.on_exit().await?;

        let mut new_handler: Box<ReqHandler> = match next {
            ManagerPhase::Booting => Box::new(ReqHandler::Booting(BootingHandler)),
            ManagerPhase::Configuring => Box::new(ReqHandler::Configuring(ConfiguringHandler {
                session_manager: self.manager.clone(),
            })),
            ManagerPhase::Installing => Box::new(ReqHandler::Installing(InstallingHandler)),
            ManagerPhase::Serving => Box::new(ReqHandler::Serving(ServingHandler)),
        };

        new_handler.on_enter().await?;
        *handler_guard = new_handler;
        Ok(())
    }
}

#[async_trait]
pub trait RequestHandler: Send + Sync {
    async fn router(&self) -> Router;
    async fn on_enter(&mut self) -> Result<(), RequestError>;
    async fn on_exit(&mut self) -> Result<(), RequestError>;
}

#[derive(Debug)]
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

    async fn on_enter(&mut self) -> Result<(), RequestError> {
        match self {
            ReqHandler::Noop(h) => h.on_enter().await,
            ReqHandler::Booting(h) => h.on_enter().await,
            ReqHandler::Configuring(h) => h.on_enter().await,
            ReqHandler::Installing(h) => h.on_enter().await,
            ReqHandler::Serving(h) => h.on_enter().await,
        }
    }

    async fn on_exit(&mut self) -> Result<(), RequestError> {
        match self {
            ReqHandler::Noop(h) => h.on_exit().await,
            ReqHandler::Booting(h) => h.on_exit().await,
            ReqHandler::Configuring(h) => h.on_exit().await,
            ReqHandler::Installing(h) => h.on_exit().await,
            ReqHandler::Serving(h) => h.on_exit().await,
        }
    }
}
#[derive(Debug)]
pub struct NoopHandler;

#[async_trait]
impl RequestHandler for NoopHandler {
    #[tracing::instrument(skip_all)]
    async fn router(&self) -> Router {
        Router::new()
    }
    #[tracing::instrument(skip_all)]
    async fn on_enter(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
}

#[derive(Debug)]
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
    async fn on_enter(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct ConfiguringHandler {
    pub session_manager: Arc<SessionManager>,
}

#[async_trait]
impl RequestHandler for ConfiguringHandler {
    #[tracing::instrument(skip_all)]
    async fn router(&self) -> Router {
        Router::new()
            .fallback_service(
                ServeDir::new("config-app").not_found_service(spa_index("config-app/index.html")),
            )
            // public
            .route("/", get(show_configure))
            .route("/login", post(login))
            // protected
            .route(
                "/save",
                post(save_configuration).route_layer(axum::middleware::from_fn_with_state(
                    self.session_manager.clone(),
                    require_session,
                )),
            )
            .with_state(self.session_manager.clone())
    }
    #[tracing::instrument(skip_all)]
    async fn on_enter(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
}

#[derive(Debug)]
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
    async fn on_enter(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
}

#[derive(Debug)]
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
    async fn on_enter(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
    #[tracing::instrument(skip_all)]
    async fn on_exit(&mut self) -> Result<(), RequestError> {
        Ok(())
    }
}

#[derive(Template)]
#[template(path = "home.html")]
pub struct HomeTemplate {
    pub title: String,
    pub message: String,
}

#[tracing::instrument(skip_all)]
async fn home_page() -> Result<impl IntoResponse, RequestError> {
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

#[derive(Template)]
#[template(path = "configure.html")]
pub struct ConfigureTemplate {
    pub error: Option<String>,
}
#[derive(Template)]
#[template(path = "configure_full.html")]
pub struct ConfigureFullTemplate {
    pub config_form: String,
}

#[derive(Template)]
#[template(path = "db_config.html")]
pub struct DbConfigTemplate;

#[derive(Deserialize)]
pub struct LoginForm {
    pub password: String,
}

#[tracing::instrument(skip_all)]
pub async fn show_configure(
    State(session): State<Arc<SessionManager>>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
) -> Html<String> {
    if let Some(cookie) = jar.get("session") {
        let fingerprint = fingerprint_from_headers(&headers);
        debug!("CONFIGURE REQUEST");
        if session.validate_session(cookie.value(), &fingerprint) {
            // If this is an HTMX request, return partial
            if headers.contains_key("HX-Request") {
                debug!("HTMX REQUEST");
                return Html(DbConfigTemplate.render().unwrap());
            }

            debug!("NON-HTMX REQUEST");
            // Otherwise, return the full page with the config form embedded
            let partial = DbConfigTemplate.render().unwrap();
            return Html(
                ConfigureFullTemplate {
                    config_form: partial,
                }
                .render()
                .unwrap(),
            );
        }
    }

    // Otherwise show the login screen
    Html(ConfigureTemplate { error: None }.render().unwrap())
}

#[tracing::instrument(skip_all)]
pub async fn login(
    jar: CookieJar,
    State(session): State<Arc<SessionManager>>,
    headers: axum::http::HeaderMap,
    Form(form): Form<LoginForm>,
) -> (CookieJar, Html<String>) {
    if session.verify_password(&form.password) {
        // Build fingerprint and session cookie
        let fingerprint = fingerprint_from_headers(&headers);
        let cookie_value = session.create_cookie(&fingerprint);

        // Attach session cookie using CookieJar
        let updated_jar = jar.add(
            Cookie::build(("session", cookie_value))
                .http_only(true)
                .path("/")
                .build(),
        );

        // Return DbConfig form (HTMX will swap this in)
        (updated_jar, Html(DbConfigTemplate.render().unwrap()))
    } else {
        // Invalid password: just return login template with error, no new cookies
        (
            jar,
            Html(
                ConfigureTemplate {
                    error: Some("Invalid password".to_string()),
                }
                .render()
                .unwrap(),
            ),
        )
    }
}

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct SessionManager {
    password: ValidatedPassword,
    sessions: Arc<std::sync::RwLock<HashMap<String, (u64, String)>>>, // token -> (expiry, fingerprint)
}

impl SessionManager {
    #[tracing::instrument(skip_all)]
    pub fn new(password: ValidatedPassword) -> Self {
        Self {
            password,
            sessions: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    #[tracing::instrument(skip_all)]
    pub fn verify_password(&self, attempt: &str) -> bool {
        self.password.verify(attempt)
    }

    #[tracing::instrument(skip_all)]
    pub fn create_cookie(&self, fingerprint: &str) -> String {
        type HmacSha256 = Hmac<sha2::Sha256>;

        let token: [u8; 32] = rand::thread_rng().gen();
        let token_b64 = base64::engine::general_purpose::STANDARD.encode(&token);

        let expiry = SystemTime::now()
            .checked_add(Duration::from_secs(600))
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut mac = HmacSha256::new_from_slice(self.password.as_hashed().as_bytes()).unwrap();
        mac.update(format!("{token_b64}:{expiry}").as_bytes());
        let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        // Store and return token
        self.sessions
            .write()
            .unwrap()
            .insert(token_b64.clone(), (expiry, fingerprint.to_string()));

        format!("{token_b64}:{expiry}:{sig}")
    }

    #[tracing::instrument(skip_all)]
    fn validate_session(&self, cookie: &str, fingerprint: &str) -> bool {
        let parts: Vec<&str> = cookie.split(':').collect();
        if parts.len() != 3 {
            return false;
        }

        let (token, expiry_str, sig) = (parts[0], parts[1], parts[2]);

        // verify signature
        let mut mac = HmacSha256::new_from_slice(&self.password.as_hashed().as_bytes()).unwrap();
        mac.update(format!("{token}:{expiry_str}").as_bytes());
        if mac
            .verify_slice(&general_purpose::STANDARD.decode(sig).unwrap_or_default())
            .is_err()
        {
            return false;
        }

        // verify expiry
        let expiry = expiry_str.parse::<u64>().unwrap_or(0);
        if SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            > expiry
        {
            return false;
        }

        // verify fingerprint
        if let Some((_, saved_fingerprint)) = self.sessions.read().unwrap().get(token) {
            return saved_fingerprint == fingerprint;
        }
        false
    }
}

/// Axum middleware to enforce sessions
#[tracing::instrument(skip_all)]
pub async fn require_session(
    State(session): State<Arc<SessionManager>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let fingerprint = fingerprint_from_headers(req.headers());

    if let Some(cookie_header) = req.headers().get("cookie") {
        if let Some(raw_session) = cookie_header
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("session="))
        {
            if session.validate_session(raw_session, &fingerprint) {
                return Ok(next.run(req).await);
            }
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

#[tracing::instrument(skip_all)]
fn fingerprint_from_headers(headers: &HeaderMap) -> String {
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown-ip");
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown-ua");
    format!("{ip}-{ua}")
}

#[derive(serde::Deserialize)]
pub struct ConfigForm {
    pub db_type: String, // "embedded" or "server"
    pub db_host: Option<String>,
    pub db_port: Option<u16>,
    pub db_username: Option<String>,
    pub db_password: Option<String>,
    pub db_name: Option<String>,
    pub pool_size: Option<u8>,
}

pub async fn save_configuration(
    jar: CookieJar,
    State(session): State<Arc<SessionManager>>,
    headers: HeaderMap,
    Form(form): Form<ConfigForm>,
) -> (CookieJar, Html<String>) {
    // Get the session cookie (same API)
    if let Some(cookie) = jar.get("session") {
        let fingerprint = fingerprint_from_headers(&headers);

        if !session.validate_session(cookie.value(), &fingerprint) {
            return (
                jar,
                Html("<div class='text-red-600'>Unauthorized: Invalid session</div>".to_string()),
            );
        }
    } else {
        return (
            jar,
            Html("<div class='text-red-600'>Unauthorized: No session found</div>".to_string()),
        );
    }

    // Process the form based on db_type
    if form.db_type == "embedded" {
        // Save embedded DB configuration
        // TODO: persist to disk
        return (
            jar,
            Html("<div class='text-green-600'>Embedded database selected and saved successfully!</div>".to_string()),
        );
    } else {
        // Validate required server fields
        if form.db_host.is_none() || form.db_port.is_none() {
            return (
                jar,
                Html(
                    "<div class='text-red-600'>Please fill in all required server fields</div>"
                        .to_string(),
                ),
            );
        }

        // TODO: test database connection + persist config
        return (
            jar,
            Html("<div class='text-green-600'>Database server configuration saved successfully!</div>".to_string()),
        );
    }
}
#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use super::*;
    use http_body_util::BodyExt;
    use std::fs;
    use tempfile::NamedTempFile;

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
        let password =
            ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();
        let mut handler = ConfiguringHandler {
            //config_file: Arc::new(temp_config_file(password.clone())),
            session_manager: Arc::new(SessionManager::new(password)),
        };
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
        let password =
            ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();
        let mut handlers = vec![
            ReqHandler::Noop(NoopHandler),
            ReqHandler::Booting(BootingHandler),
            ReqHandler::Configuring(ConfiguringHandler {
                //config_file: Arc::new(temp_config_file(password.clone())),
                session_manager: Arc::new(SessionManager::new(password)),
            }),
            ReqHandler::Installing(InstallingHandler),
            ReqHandler::Serving(ServingHandler),
        ];

        for handler in &mut handlers {
            assert!(handler.on_enter().await.is_ok());
        }
    }

    #[tokio::test]
    async fn req_handler_on_exit_all_variants() {
        let password =
            ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();
        let mut handlers = vec![
            ReqHandler::Noop(NoopHandler),
            ReqHandler::Booting(BootingHandler),
            ReqHandler::Configuring(ConfiguringHandler {
                //config_file: Arc::new(temp_config_file(password.clone())),
                session_manager: Arc::new(SessionManager::new(password)),
            }),
            ReqHandler::Installing(InstallingHandler),
            ReqHandler::Serving(ServingHandler),
        ];

        for handler in &mut handlers {
            assert!(handler.on_exit().await.is_ok());
        }
    }

    #[tokio::test]
    async fn req_handler_router_all_variants() {
        let password =
            ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();
        let handlers = vec![
            ReqHandler::Noop(NoopHandler),
            ReqHandler::Booting(BootingHandler),
            ReqHandler::Configuring(ConfiguringHandler {
                //config_file: Arc::new(temp_config_file(password.clone())),
                session_manager: Arc::new(SessionManager::new(password)),
            }),
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
