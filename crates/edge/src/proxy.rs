use adapt::runtime::RuntimeError;
use axum::Router;
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
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{net::TcpListener, sync::oneshot};

// Adjust this import path to wherever your Settings type lives
use domain::setting::Settings;

use crate::db::tantivy::ContentIndexError;

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

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("Runtime error: {0}")]
    Runtime(#[from] RuntimeError),

    #[error("Other: {0}")]
    Other(String),
}

/// Handle for controlling the Axum WebServer (hot reload, shutdown).
pub struct WebServerHandle {
    current_backend: Arc<BackendState>,
    /// Shutdown signal for the *current* Axum server.
    shutdown_tx: Arc<RwLock<Option<oneshot::Sender<()>>>>,

    loopback_ip: IpAddr,
    port_a: u16,
    port_b: u16,
}

impl WebServerHandle {
    fn new(
        current_backend: Arc<BackendState>,
        shutdown_tx: oneshot::Sender<()>,
        loopback_ip: IpAddr,
        port_a: u16,
        port_b: u16,
    ) -> Self {
        Self {
            current_backend,
            shutdown_tx: Arc::new(RwLock::new(Some(shutdown_tx))),
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

    /// Start Axum on the "other" port, then atomically flip Pingora to it and
    /// gracefully drain/shutdown the old side.
    ///
    /// `make_router` is a closure that builds the Axum router (so you can change routes/config).
    pub async fn hot_reload<F>(&self, make_router: F) -> Result<(), EdgeError>
    where
        F: Fn() -> Router + Send + 'static,
    {
        let new_port = self.next_port();
        let new_addr = SocketAddr::from((self.loopback_ip, new_port));

        tracing::info!("Starting new Axum WebServer on {new_addr}");

        let listener = TcpListener::bind(new_addr).await?;

        // Spawn new Axum server first
        let (new_shutdown_tx, new_shutdown_rx) = oneshot::channel::<()>();
        let router = make_router();
        let grace = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = new_shutdown_rx.await;
        });

        tokio::spawn(async move {
            if let Err(err) = grace.await {
                tracing::error!("Axum (new) server error: {err}");
            }
        });

        // Flip backend for Pingora
        tracing::info!("Switching Pingora backend to {new_addr}");
        self.current_backend.set(new_addr);

        // Tell the old Axum server to shut down
        if let Some(tx) = self.shutdown_tx.write().take() {
            let _ = tx.send(());
        }

        // Replace shutdown handle with the new one
        *self.shutdown_tx.write() = Some(new_shutdown_tx);

        Ok(())
    }

    /// Gracefully stop the current WebServer.
    pub async fn shutdown(&self) {
        if let Some(tx) = self.shutdown_tx.write().take() {
            let _ = tx.send(());
        }
    }
}

/// Proxy implementation: HTTPS EdgeController → Axum WebServer.
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
        // No TLS between Pingora and Axum (plain HTTP over loopback)
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

/// Aggregate runtime: owns Pingora EdgeController and Axum WebServer.
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
    pub async fn start<F>(settings: Settings, make_router: F) -> Result<Self, EdgeError>
    where
        F: Fn() -> Router + Send + Sync + 'static,
    {
        tracing_subscriber::fmt().with_env_filter("info").init();

        // Derive runtime values from Settings
        let cert_dir = settings.cert.dir;
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

        // 2) Start initial Axum WebServer (port A)
        let initial_addr = SocketAddr::from((loopback_ip, web_port_a));
        let listener = TcpListener::bind(initial_addr).await?;

        let backend_state = Arc::new(BackendState::new(initial_addr));

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let router = make_router();
        let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });

        tokio::spawn(async move {
            if let Err(err) = serve.await {
                tracing::error!("Axum WebServer error: {err}");
            }
        });

        let web_handle = WebServerHandle::new(
            backend_state.clone(),
            shutdown_tx,
            loopback_ip,
            web_port_a,
            web_port_b,
        );

        // 3) Start Pingora EdgeController on a dedicated thread
        let pingora_thread = std::thread::spawn(move || {
            if let Err(err) = run_pingora_edge(
                backend_state,
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

    /// Stop Axum and wait for Pingora thread to exit.
    pub async fn shutdown(self) {
        self.web_handle.shutdown().await;
        // For now, we don’t send any shutdown signal to Pingora; you’d usually
        // send a process signal (SIGTERM) and let Pingora’s graceful shutdown kick in.
        let _ = self.pingora_thread.join();
    }
}

/// Implementation detail: start Pingora EdgeController.
fn run_pingora_edge(
    backend_state: Arc<BackendState>,
    has_cert: bool,
    edge_http: SocketAddr,
    edge_https: SocketAddr,
    external_https_port: u16,
) -> Result<(), EdgeError> {
    // `None` uses default ServerConf (or config from environment / default path).
    let mut server = Server::new(None).map_err(EdgeError::Pingora)?;
    server.bootstrap();

    let mut services: Vec<Box<dyn pingora::services::Service>> = Vec::new();

    if has_cert {
        // HTTPS proxy service (EdgeController → Axum)
        let proxy = EdgeProxy::new(backend_state);
        let mut proxy_service = http_proxy_service(&server.configuration, proxy);
        proxy_service.add_tcp(edge_https.to_string().as_str());
        services.push(Box::new(proxy_service));

        // HTTP → HTTPS redirect service
        let redirect_app = RedirectApp::new(external_https_port);
        let http_server = PingoraHttpServer::new_app(redirect_app);
        let mut redirect_service = ListeningService::new("http_redirect".to_string(), http_server);

        redirect_service.add_tcp(edge_http.to_string().as_str());
        services.push(Box::new(redirect_service));
    } else {
        // No certs: don’t bind any EdgeController listeners
        tracing::warn!("Pingora EdgeController not binding any listeners (no TLS certs)");
    }

    if !services.is_empty() {
        server.add_services(services);
        server.run_forever(); // blocks in this thread
    } else {
        // If no services, just keep the thread alive (or you could return Ok(()) if you prefer).
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }

    // Unreachable if services were non-empty; kept for type completeness.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::time::timeout;

    // Helper to make a unique temp directory under the system temp dir.
    fn make_temp_dir(name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let unique = format!(
            "edge_test_{}_{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        dir.push(unique);
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        dir
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // BackendState
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn backend_state_get_set_roundtrip() {
        let addr1 = SocketAddr::from((Ipv4Addr::LOCALHOST, 1234));
        let addr2 = SocketAddr::from((Ipv4Addr::LOCALHOST, 5678));

        let state = BackendState::new(addr1);
        assert_eq!(state.get(), addr1);

        state.set(addr2);
        assert_eq!(state.get(), addr2);
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // WebServerHandle::next_port behavior
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn webserver_handle_next_port_flips_from_a_to_b() {
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let port_a = 9000;
        let port_b = 9001;

        let initial_addr = SocketAddr::from((loopback, port_a));
        let backend_state = Arc::new(BackendState::new(initial_addr));

        let (shutdown_tx, _shutdown_rx) = oneshot::channel::<()>();
        let handle = WebServerHandle::new(backend_state, shutdown_tx, loopback, port_a, port_b);

        // When current port is A, next_port should be B
        let next = handle.next_port();
        assert_eq!(next, port_b);
    }

    #[test]
    fn webserver_handle_next_port_flips_from_b_to_a() {
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let port_a = 9000;
        let port_b = 9001;

        let initial_addr = SocketAddr::from((loopback, port_b));
        let backend_state = Arc::new(BackendState::new(initial_addr));

        let (shutdown_tx, _shutdown_rx) = oneshot::channel::<()>();
        let handle = WebServerHandle::new(backend_state, shutdown_tx, loopback, port_a, port_b);

        // When current port is B, next_port should be A
        let next = handle.next_port();
        assert_eq!(next, port_a);
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // WebServerHandle::shutdown behavior (negative path: no panic, signal sent)
    // ─────────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn webserver_handle_shutdown_sends_signal() {
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let port_a = 9000;
        let port_b = 9001;

        let initial_addr = SocketAddr::from((loopback, port_a));
        let backend_state = Arc::new(BackendState::new(initial_addr));

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let handle = WebServerHandle::new(backend_state, shutdown_tx, loopback, port_a, port_b);

        // Call shutdown and ensure the receiver gets a signal.
        handle.shutdown().await;

        let res = timeout(Duration::from_millis(200), shutdown_rx).await;
        assert!(
            res.is_ok(),
            "expected shutdown_rx to complete after shutdown()"
        );
        assert!(
            res.unwrap().is_ok(),
            "expected shutdown_rx to receive Ok(())"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // WebServerHandle::hot_reload behavior (positive path)
    // ─────────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn webserver_handle_hot_reload_flips_backend_and_closes_old_shutdown() {
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let port_a = 9100;
        let port_b = 9101;

        // Start with A as the current backend
        let initial_addr = SocketAddr::from((loopback, port_a));
        let backend_state = Arc::new(BackendState::new(initial_addr));

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let handle =
            WebServerHandle::new(backend_state.clone(), shutdown_tx, loopback, port_a, port_b);

        // Hot-reload with a trivial empty router
        handle
            .hot_reload(|| Router::new())
            .await
            .expect("hot_reload should succeed");

        // BackendState should now point to port_b
        let new_addr = backend_state.get();
        assert_eq!(new_addr.port(), port_b);

        // The old shutdown channel should have been signaled
        let res = timeout(Duration::from_millis(200), shutdown_rx).await;
        assert!(
            res.is_ok(),
            "expected old shutdown receiver to complete after hot_reload()"
        );
        assert!(
            res.unwrap().is_ok(),
            "expected old shutdown receiver to receive Ok(())"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────────
    // cert_dir_has_files positive and negative cases
    // ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn cert_dir_missing_returns_false() {
        let mut dir = std::env::temp_dir();
        dir.push("edge_test_missing_dir_this_should_not_exist_12345");

        // Ensure it doesn't exist
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }

        let has_files = cert_dir_has_files(&dir).expect("cert_dir_has_files should not error");
        assert!(
            !has_files,
            "missing directory should report has_files == false"
        );
    }

    #[test]
    fn cert_dir_empty_returns_false() {
        let dir = make_temp_dir("empty");

        let has_files = cert_dir_has_files(&dir).expect("cert_dir_has_files should not error");
        assert!(
            !has_files,
            "empty directory should report has_files == false"
        );

        // cleanup
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cert_dir_with_file_returns_true() {
        let dir = make_temp_dir("with_file");

        // Create one file inside the directory
        let mut file_path = dir.clone();
        file_path.push("dummy.cert");
        fs::write(&file_path, b"dummy").expect("failed to write test file");

        let has_files = cert_dir_has_files(&dir).expect("cert_dir_has_files should not error");
        assert!(
            has_files,
            "directory with a file should report has_files == true"
        );

        // cleanup
        fs::remove_file(&file_path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
