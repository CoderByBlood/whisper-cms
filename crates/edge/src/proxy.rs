// crates/edge/src/proxy.rs

use actix_web::dev::{ServerHandle, ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::{App, HttpServer};
use adapt::runtime::bootstrap::RuntimeHandles;
use adapt::runtime::RuntimeError;
use http::{Response, StatusCode};
use parking_lot::RwLock;
use pingora::apps::http_app::{HttpServer as PingoraHttpServer, ServeHttp};
use pingora::prelude::*;
use pingora::protocols::http::server::Session as HttpSession;
use pingora::protocols::raw_connect::ConnectProxyError;
use pingora::proxy::{http_proxy_service, ProxyHttp, Session as ProxySession};
use pingora::server::Server;
use pingora::services::listening::Service as ListeningService;
use pingora::upstreams::peer::HttpPeer;
use serve::indexer::DocContextError;

use std::{
    fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;

use domain::setting::Settings;

use crate::db::tantivy::ContentIndexError;
use crate::fs::ext::ThemeBinding;
use crate::fs::index::FrontMatterIndexError;
use crate::router::build_app_router;

/// Shared state: which loopback port is currently "active" for the WebServer.
///
/// Pingora's proxy reads this to decide where to send traffic.
#[derive(Debug)]
pub struct BackendState {
    inner: RwLock<SocketAddr>,
}

impl BackendState {
    fn new(initial: SocketAddr) -> Self {
        Self {
            inner: RwLock::new(initial),
        }
    }

    pub fn get(&self) -> SocketAddr {
        *self.inner.read()
    }

    pub fn set(&self, addr: SocketAddr) {
        *self.inner.write() = addr;
    }
}

#[derive(Debug, Error)]
pub enum EdgeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    // Note: Server::new returns Result<Server, Box<pingora::Error>>
    #[error("Pingora error: {0}")]
    Pingora(#[from] Box<pingora::Error>),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Channel closed")]
    Channel,

    #[error("Proxy error: {0}")]
    Proxy(#[from] ConnectProxyError),

    #[error("Content Index Error {0}")]
    ContentIndex(#[from] ContentIndexError),

    #[error("FrontMatter Index Error {0}")]
    FrontMatterIndexError(#[from] FrontMatterIndexError),

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("Runtime error: {0}")]
    Runtime(#[from] RuntimeError),

    #[error("DocContextError error: {0}")]
    DocContextError(#[from] DocContextError),

    #[error("Other: {0}")]
    Other(String),
}

/// Handle for controlling the Actix WebServer (hot reload, shutdown).
pub struct WebServerHandle {
    current_backend: Arc<BackendState>,
    /// Handle for the *current* Actix HttpServer.
    server_handle: Arc<RwLock<Option<ServerHandle>>>,

    loopback_ip: IpAddr,
    port_a: u16,
    port_b: u16,
}

impl WebServerHandle {
    fn new(
        current_backend: Arc<BackendState>,
        server_handle: ServerHandle,
        loopback_ip: IpAddr,
        port_a: u16,
        port_b: u16,
    ) -> Self {
        Self {
            current_backend,
            server_handle: Arc::new(RwLock::new(Some(server_handle))),
            loopback_ip,
            port_a,
            port_b,
        }
    }

    /// Compute the "other" port (A/B flip).
    fn next_port(&self) -> u16 {
        let current = self.current_backend.get().port();
        if current == self.port_a {
            self.port_b
        } else {
            self.port_a
        }
    }

    /// Start an Actix WebServer on the "other" port, then atomically flip
    /// Pingora to it and gracefully stop the old one.
    ///
    /// `make_app` is a closure that builds the Actix `App` (so you can change
    /// routes/config). We make this generic over the inner `ServiceFactory`
    /// type `T` to satisfy `HttpServer::new`.
    #[tracing::instrument(skip_all)]
    pub async fn hot_reload<F, T>(&self, make_app: F) -> Result<(), EdgeError>
    where
        F: Fn() -> App<T> + Clone + Send + 'static,
        T: ServiceFactory<
                ServiceRequest,
                Config = (),
                Response = ServiceResponse,
                Error = actix_web::Error,
                InitError = (),
            > + 'static,
    {
        let new_port = self.next_port();
        let new_addr = SocketAddr::from((self.loopback_ip, new_port));

        tracing::info!("Starting new Actix WebServer on {new_addr}");

        let server = HttpServer::new(make_app).bind(new_addr)?.run();

        let new_handle = server.handle();

        tokio::spawn(async move {
            if let Err(err) = server.await {
                tracing::error!("Actix (new) server error: {err}");
            }
        });

        // Flip backend for Pingora
        tracing::info!("Switching Pingora backend to {new_addr}");
        self.current_backend.set(new_addr);

        // Stop the old Actix server (gracefully).
        if let Some(old) = self.server_handle.write().take() {
            old.stop(true).await;
        }

        // Replace with new handle
        *self.server_handle.write() = Some(new_handle);

        Ok(())
    }

    /// Gracefully stop the current WebServer.
    #[tracing::instrument(skip_all)]
    pub async fn shutdown(&self) {
        if let Some(handle) = self.server_handle.write().take() {
            handle.stop(true).await;
        }
    }
}

/// Proxy implementation: HTTPS EdgeController → Actix WebServer.
pub struct EdgeProxy {
    backend: Arc<BackendState>,
}

impl EdgeProxy {
    pub fn new(backend: Arc<BackendState>) -> Self {
        Self { backend }
    }
}

#[async_trait::async_trait]
impl ProxyHttp for EdgeProxy {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {
        ()
    }

    async fn upstream_peer(
        &self,
        _session: &mut ProxySession,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        let addr = self.backend.get();
        // No TLS between Pingora and Actix (plain HTTP over loopback)
        let is_tls = false;
        let sni = addr.ip().to_string();
        let peer = Box::new(HttpPeer::new((addr.ip(), addr.port()), is_tls, sni));
        Ok(peer)
    }
}

/// Simple Pingora HTTP server that just issues HTTP→HTTPS redirects.
///
/// Listens on `edge_http`, redirects to `https://host:port/path`.
pub struct RedirectApp {
    external_https_port: u16,
}

impl RedirectApp {
    pub fn new(external_https_port: u16) -> Self {
        Self {
            external_https_port,
        }
    }
}

#[async_trait::async_trait]
impl ServeHttp for RedirectApp {
    #[tracing::instrument(skip_all)]
    async fn response(&self, http_session: &mut HttpSession) -> Response<Vec<u8>> {
        let req = http_session.req_header();

        // Try to reuse Host header; fallback to "localhost"
        let host = req
            .headers
            .get("Host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");

        let path_and_query = req
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");

        let location = if self.external_https_port == 443 {
            format!("https://{host}{path_and_query}")
        } else {
            format!(
                "https://{host}:{}{}",
                self.external_https_port, path_and_query
            )
        };

        let mut builder = Response::builder().status(StatusCode::MOVED_PERMANENTLY);
        builder = builder.header("Location", location);

        builder.body(Vec::new()).unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Vec::new())
                .unwrap()
        })
    }
}

/// Aggregate runtime: owns Pingora EdgeController and Actix WebServer.
pub struct EdgeRuntime {
    /// Join handle for the Pingora server thread.
    pingora_thread: std::thread::JoinHandle<()>,

    /// Handle to control the WebServer (hot reload / shutdown).
    web_handle: WebServerHandle,
}

impl EdgeRuntime {
    /// Initialize everything and return a handle.
    ///
    /// * Validates cert_dir contains at least one file.
    /// * If **no TLS certificates are found**, we:
    ///   - **Do not bind any EdgeController listeners**
    ///   - **Still start the WebServer** (on loopback) so you can configure/fix certs.
    #[tracing::instrument(skip_all)]
    pub async fn start(
        root: PathBuf,
        settings: Settings,
        handles: RuntimeHandles,
        bindings: Vec<ThemeBinding>,
    ) -> Result<Self, EdgeError> {
        // Derive runtime values from Settings
        let cert_dir = root.join(settings.cert.dir);
        let edge_ip = settings.edge.ip;
        let edge_http = SocketAddr::from((edge_ip, settings.edge.http_port));
        let edge_https = SocketAddr::from((edge_ip, settings.edge.https_port));

        let loopback_ip = settings.loopback.ip;
        let web_port_a = settings.loopback.port_a;
        let web_port_b = settings.loopback.port_b;
        let external_https_port = settings.edge.https_port;

        // 1) Cert directory check (require at least one file)
        let has_cert = cert_dir_has_files(&cert_dir)?;
        if !has_cert {
            tracing::warn!(
                "No TLS certificates found in {:?}; EdgeController will NOT bind listeners, \
                 but WebServer will still start.",
                cert_dir
            );
        }

        // 2) Start initial Actix WebServer (port A)
        let initial_addr = SocketAddr::from((loopback_ip, web_port_a));
        let backend_state = Arc::new(BackendState::new(initial_addr));

        let handles_for_server = handles.clone();
        let bindings_for_server = bindings.clone();

        tracing::info!("Actix WebServer started on {}", initial_addr);

        let server = HttpServer::new(move || {
            App::new().service(build_app_router(
                root.clone(),
                handles_for_server.clone(),
                bindings_for_server.clone(),
            ))
        })
        .bind(initial_addr)?
        .run();

        let server_handle = server.handle();

        tokio::spawn(async move {
            if let Err(err) = server.await {
                tracing::error!("Actix WebServer error: {err}");
            }
        });

        let web_handle = WebServerHandle::new(
            backend_state.clone(),
            server_handle,
            loopback_ip,
            web_port_a,
            web_port_b,
        );

        // Copy cert_dir for the Pingora thread
        let cert_dir_for_pingora = cert_dir.clone();

        // 3) Start Pingora EdgeController on a dedicated thread
        let pingora_thread = std::thread::spawn(move || {
            if let Err(err) = run_pingora_edge(
                backend_state,
                cert_dir_for_pingora,
                has_cert,
                edge_http,
                edge_https,
                external_https_port,
            ) {
                eprintln!("Pingora EdgeController failed: {err}");
            }
        });

        Ok(Self {
            pingora_thread,
            web_handle,
        })
    }

    /// Access the WebServer handle to hot-reload routes/config.
    pub fn web_handle(&self) -> &WebServerHandle {
        &self.web_handle
    }

    /// Stop Actix and wait for Pingora thread to exit.
    pub async fn shutdown(self) {
        self.web_handle.shutdown().await;
        // For now, we don’t send any shutdown signal to Pingora; you’d usually
        // send a process signal (SIGTERM) and let Pingora’s graceful shutdown kick in.
        let _ = self.pingora_thread.join();
    }
}

/// Implementation detail: start Pingora EdgeController.
#[tracing::instrument(skip_all)]
fn run_pingora_edge(
    backend_state: Arc<BackendState>,
    cert_dir_for_pingora: PathBuf,
    has_cert: bool,
    edge_http: SocketAddr,
    edge_https: SocketAddr,
    external_https_port: u16,
) -> Result<(), EdgeError> {
    // Use Pingora's default options (no explicit config file / CLI opts).
    let mut server =
        Server::new(None::<pingora::server::configuration::Opt>).map_err(EdgeError::Pingora)?;
    server.bootstrap();

    let mut services: Vec<Box<dyn pingora::services::Service>> = Vec::new();

    if has_cert {
        // Discover a cert/key pair in the cert directory.
        let pair = pick_cert_key_pair(&cert_dir_for_pingora)?;
        if let Some((cert_path, key_path)) = pair {
            tracing::info!(
                "Using TLS cert: {} and key: {} for Pingora",
                cert_path.display(),
                key_path.display()
            );

            let cert_str = cert_path
                .to_str()
                .ok_or_else(|| EdgeError::Config("Non-UTF8 cert path".to_string()))?;
            let key_str = key_path
                .to_str()
                .ok_or_else(|| EdgeError::Config("Non-UTF8 key path".to_string()))?;

            // HTTPS proxy service (EdgeController → Actix) with TLS termination
            let proxy = EdgeProxy::new(backend_state);
            let mut proxy_service = http_proxy_service(&server.configuration, proxy);

            // Bind a TLS listener on edge_https.
            proxy_service
                .add_tls(edge_https.to_string().as_str(), cert_str, key_str)
                .map_err(|e| {
                    EdgeError::Config(format!("Failed to add TLS listener on {}: {e}", edge_https))
                })?;

            services.push(Box::new(proxy_service));

            // HTTP → HTTPS redirect service
            let redirect_app = RedirectApp::new(external_https_port);
            let http_server = PingoraHttpServer::new_app(redirect_app);
            let mut redirect_service =
                ListeningService::new("http_redirect".to_string(), http_server);

            redirect_service.add_tcp(edge_http.to_string().as_str());
            services.push(Box::new(redirect_service));
        } else {
            tracing::warn!(
                "TLS cert directory {:?} has files, but no usable (pem/crt, key) pair was found; \
                 EdgeController will not bind listeners.",
                cert_dir_for_pingora
            );
        }
    } else {
        // No certs: don’t bind any EdgeController listeners
        tracing::warn!("Pingora EdgeController not binding any listeners (no TLS certs)");
    }

    if !services.is_empty() {
        server.add_services(services);
        server.run_forever(); // blocks in this thread
    } else {
        // If no services, just keep the thread alive (or return Ok(()) if you prefer).
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    #[allow(unreachable_code)]
    Ok(())
}

/// Return true if `cert_dir` contains at least one regular file.
fn cert_dir_has_files(dir: &Path) -> Result<bool, EdgeError> {
    if !dir.exists() {
        return Ok(false);
    }
    let mut entries = std::fs::read_dir(dir)?;
    Ok(entries.any(|e| {
        e.ok()
            .and_then(|entry| entry.metadata().ok())
            .map(|m| m.is_file())
            .unwrap_or(false)
    }))
}

/// Try to pick a cert + key pair from the directory.
///
/// Heuristic:
///   * first file with extension in { "pem", "crt" } -> cert
///   * first file with extension == "key"            -> key
fn pick_cert_key_pair(dir: &Path) -> Result<Option<(PathBuf, PathBuf)>, EdgeError> {
    if !dir.exists() {
        return Ok(None);
    }

    let mut cert: Option<PathBuf> = None;
    let mut key: Option<PathBuf> = None;

    for entry in fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        match ext.as_str() {
            "pem" | "crt" if cert.is_none() => {
                tracing::debug!("Discovered candidate TLS cert: {}", path.display());
                cert = Some(path);
            }
            "key" if key.is_none() => {
                tracing::debug!("Discovered candidate TLS key: {}", path.display());
                key = Some(path);
            }
            _ => {}
        };

        if cert.is_some() && key.is_some() {
            break;
        }
    }

    Ok(match (cert, key) {
        (Some(c), Some(k)) => Some((c, k)),
        _ => None,
    })
}
