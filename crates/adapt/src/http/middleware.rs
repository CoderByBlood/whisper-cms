// crates/adapt/src/http/middleware.rs

//! Tower middleware that executes JS plugins before and after the theme
//! handler using the single-threaded `PluginRuntimeClient`.
//!
//! - Each plugin is treated as an independent "service" with its own
//!   circuit breaker state (timeouts + failure counters).
//! - Plugin failures never bubble up as tower errors; instead we log
//!   and "open" the circuit for that plugin, and continue the request
//!   pipeline so a bad plugin can't take the site down.

use crate::runtime::plugin_actor::PluginRuntimeClient;

use axum::{body::Body, http::Request, response::Response};
use domain::content::ResolvedContent;
use futures::future::BoxFuture;
use http::Uri;
use serve::{
    ctx::http::RequestContext,
    resolver::{self, build_request_context},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tower::{Layer, Service};

// ─────────────────────────────────────────────────────────────────────────────
// Circuit-breaker configuration
// ─────────────────────────────────────────────────────────────────────────────

const PLUGIN_TIMEOUT: Duration = Duration::from_millis(100); // per-call timeout
const FAILURE_WINDOW: Duration = Duration::from_secs(30); // look back this far
const MAX_FAILURES: u32 = 5; // within FAILURE_WINDOW
const OPEN_DURATION: Duration = Duration::from_secs(30); // circuit open time

#[derive(Debug, Clone)]
struct FailureState {
    failures: u32,
    last_failure: Option<Instant>,
    open_until: Option<Instant>,
}

impl FailureState {
    fn new() -> Self {
        Self {
            failures: 0,
            last_failure: None,
            open_until: None,
        }
    }

    fn record_failure(&mut self, now: Instant) {
        // If last failure is "old", reset the window.
        if let Some(last) = self.last_failure {
            if now.duration_since(last) > FAILURE_WINDOW {
                self.failures = 0;
            }
        }

        self.failures += 1;
        self.last_failure = Some(now);

        if self.failures >= MAX_FAILURES {
            self.open_until = Some(now + OPEN_DURATION);
        }
    }

    fn record_success(&mut self) {
        self.failures = 0;
        self.last_failure = None;
        self.open_until = None;
    }

    fn is_open(&self, now: Instant) -> bool {
        if let Some(until) = self.open_until {
            if now < until {
                return true;
            }
        }
        false
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PluginLayer
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct PluginLayer {
    plugin_rt: PluginRuntimeClient,
    plugin_ids: Arc<Vec<String>>,
    state: Arc<Mutex<HashMap<String, FailureState>>>,
}

impl PluginLayer {
    /// Create a new layer given the JS plugin runtime client and the list
    /// of configured plugin IDs (in execution order).
    #[tracing::instrument(skip_all)]
    pub fn new(plugin_rt: PluginRuntimeClient, plugin_ids: Vec<String>) -> Self {
        Self {
            plugin_rt,
            plugin_ids: Arc::new(plugin_ids),
            state: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl<S> Layer<S> for PluginLayer {
    type Service = PluginMiddleware<S>;

    #[tracing::instrument(skip_all)]
    fn layer(&self, inner: S) -> Self::Service {
        PluginMiddleware {
            inner,
            plugin_rt: self.plugin_rt.clone(),
            plugin_ids: self.plugin_ids.clone(),
            state: self.state.clone(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PluginMiddleware
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct PluginMiddleware<S> {
    inner: S,
    plugin_rt: PluginRuntimeClient,
    plugin_ids: Arc<Vec<String>>,
    state: Arc<Mutex<HashMap<String, FailureState>>>,
}

impl<S> Service<Request<Body>> for PluginMiddleware<S>
where
    S: Service<Request<Body>, Response = Response> + Clone + Send + 'static,
    S::Error: Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    #[tracing::instrument(skip_all)]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[tracing::instrument(skip_all)]
    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        let plugin_rt = self.plugin_rt.clone();
        let plugin_ids = self.plugin_ids.clone();
        let state = self.state.clone();

        // Extract HTTP pieces we need for RequestContext construction.
        let uri: Uri = req.uri().clone();
        let path = uri.path().to_string();
        let method = req.method().clone();
        let headers = req.headers().clone();
        let query_params: HashMap<String, String> = uri
            .query()
            .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
            .unwrap_or_else(HashMap::new);

        Box::pin(async move {
            // Resolve content + build RequestContext once per request.
            let resolved =
                resolver::resolve(&path, &method).unwrap_or_else(|_e| ResolvedContent::empty());

            let mut ctx: RequestContext = build_request_context(
                path.clone(),
                method.clone(),
                headers.clone(),
                query_params,
                resolved,
            );

            // BEFORE hooks: in configured order, with per-plugin circuit breaker.
            for plugin_id in plugin_ids.iter() {
                let now = Instant::now();

                // Check / update circuit state
                {
                    let mut map = state.lock().unwrap();
                    let entry = map
                        .entry(plugin_id.clone())
                        .or_insert_with(FailureState::new);

                    if entry.is_open(now) {
                        // Circuit open: skip this plugin.
                        tracing::warn!(
                            plugin = %plugin_id,
                            "plugin circuit is open; skipping before_plugin"
                        );
                        continue;
                    }
                }

                // Call plugin with timeout. We pass a CLONE of ctx so that the
                // original ctx remains valid for later plugins / insertion.
                let call_fut = plugin_rt.before_plugin(plugin_id.clone(), ctx.clone());
                let result = tokio::time::timeout(PLUGIN_TIMEOUT, call_fut).await;

                match result {
                    Ok(Ok(new_ctx)) => {
                        // Success: update ctx and reset failure state.
                        ctx = new_ctx;
                        let mut map = state.lock().unwrap();
                        if let Some(st) = map.get_mut(plugin_id) {
                            st.record_success();
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::error!(
                            plugin = %plugin_id,
                            "before_plugin error: {err}"
                        );
                        let mut map = state.lock().unwrap();
                        if let Some(st) = map.get_mut(plugin_id) {
                            st.record_failure(now);
                        }
                        // On failure, we keep the previous ctx and continue.
                    }
                    Err(_elapsed) => {
                        tracing::error!(
                            plugin = %plugin_id,
                            "before_plugin timed out after {:?}",
                            PLUGIN_TIMEOUT
                        );
                        let mut map = state.lock().unwrap();
                        if let Some(st) = map.get_mut(plugin_id) {
                            st.record_failure(now);
                        }
                        // Keep ctx, continue.
                    }
                }
            }

            // Put the final RequestContext into request extensions so the
            // theme handler can see plugin recommendations.
            let mut req = req;
            req.extensions_mut().insert::<RequestContext>(ctx.clone());

            // Call the inner service (theme handler, etc.).
            let resp = inner.call(req).await?;

            // AFTER hooks: reverse order, using the same ctx snapshot.
            // Note: for now we do not attempt to mutate the already-built
            // HTTP response from plugin recommendations; AFTER is mainly
            // for side effects / telemetry.
            let mut ctx_after = ctx;
            for plugin_id in plugin_ids.iter().rev() {
                let now = Instant::now();

                {
                    let mut map = state.lock().unwrap();
                    let entry = map
                        .entry(plugin_id.clone())
                        .or_insert_with(FailureState::new);

                    if entry.is_open(now) {
                        tracing::warn!(
                            plugin = %plugin_id,
                            "plugin circuit is open; skipping after_plugin"
                        );
                        continue;
                    }
                }

                // Again, pass a CLONE so ctx_after stays valid if the plugin
                // times out or errors.
                let call_fut = plugin_rt.after_plugin(plugin_id.clone(), ctx_after.clone());
                let result = tokio::time::timeout(PLUGIN_TIMEOUT, call_fut).await;

                match result {
                    Ok(Ok(new_ctx)) => {
                        ctx_after = new_ctx;
                        let mut map = state.lock().unwrap();
                        if let Some(st) = map.get_mut(plugin_id) {
                            st.record_success();
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::error!(
                            plugin = %plugin_id,
                            "after_plugin error: {err}"
                        );
                        let mut map = state.lock().unwrap();
                        if let Some(st) = map.get_mut(plugin_id) {
                            st.record_failure(now);
                        }
                    }
                    Err(_elapsed) => {
                        tracing::error!(
                            plugin = %plugin_id,
                            "after_plugin timed out after {:?}",
                            PLUGIN_TIMEOUT
                        );
                        let mut map = state.lock().unwrap();
                        if let Some(st) = map.get_mut(plugin_id) {
                            st.record_failure(now);
                        }
                    }
                }
            }

            Ok(resp)
        })
    }
}
