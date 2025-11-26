// crates/edge/src/router.rs

use adapt::http::middleware::PluginLayer;
use adapt::runtime::bootstrap::RuntimeHandles;
use adapt::runtime::theme_actor::ThemeRuntimeClient;
use domain::content::ResolvedContent;
use serve::{
    ctx::http::{RequestContext, ResponseBodySpec},
    resolver::{build_request_context, resolve},
};

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use http::header;
use std::collections::HashMap;
use std::convert::Infallible;
use tower::Layer;
use tracing::{debug, error};

use crate::fs::ext::ThemeBinding;

// ─────────────────────────────────────────────────────────────────────────────
// App state per mounted theme
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct ThemeAppState {
    theme_client: ThemeRuntimeClient,
    theme_id: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Example plugin logging / circuit breaker layers
// ─────────────────────────────────────────────────────────────────────────────

/// Example plugin logging layer (plugin-specific).
#[derive(Clone)]
pub struct PluginLoggingLayer {
    plugin_id: String,
}

impl PluginLoggingLayer {
    #[tracing::instrument(skip_all)]
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
        }
    }
}

#[derive(Clone)]
pub struct PluginLoggingService<S> {
    inner: S,
    _plugin_id: String,
}

impl<S> Layer<S> for PluginLoggingLayer {
    type Service = PluginLoggingService<S>;

    #[tracing::instrument(skip_all)]
    fn layer(&self, inner: S) -> Self::Service {
        PluginLoggingService {
            inner,
            _plugin_id: self.plugin_id.clone(),
        }
    }
}

impl<S, Req> tower::Service<Req> for PluginLoggingService<S>
where
    S: tower::Service<Req>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    #[tracing::instrument(skip_all)]
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[tracing::instrument(skip_all)]
    fn call(&mut self, req: Req) -> Self::Future {
        // tracing::debug!(plugin = %self._plugin_id, "plugin middleware invoked");
        self.inner.call(req)
    }
}

/// Stub circuit-breaker layer per plugin.
///
/// For now it's a pass-through; you can replace with tower::limit or
/// a proper CB implementation later.
#[derive(Clone)]
pub struct PluginCircuitBreakerLayer {
    _plugin_id: String,
}

impl PluginCircuitBreakerLayer {
    #[tracing::instrument(skip_all)]
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self {
            _plugin_id: plugin_id.into(),
        }
    }
}

impl<S> Layer<S> for PluginCircuitBreakerLayer {
    type Service = S;

    #[tracing::instrument(skip_all)]
    fn layer(&self, inner: S) -> Self::Service {
        inner
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Router construction
// ─────────────────────────────────────────────────────────────────────────────

/// Build the main Axum router given:
/// - a content root for resolving markdown/html/json/etc
/// - runtime handles (theme + plugin actors)
/// - a list of theme bindings (mount path → theme id)
#[tracing::instrument(skip_all)]
pub fn build_app_router(handles: RuntimeHandles, bindings: Vec<ThemeBinding>) -> Router {
    let plugin_client = handles.plugin_client.clone();
    let theme_client = handles.theme_client.clone();

    // Ordered list of configured plugin IDs (host-facing IDs)
    let plugin_ids: Vec<String> = handles
        .plugin_configs
        .iter()
        .map(|cfg| cfg.id.clone())
        .collect();

    let mut app = Router::new();

    for binding in bindings {
        let mount_path = binding.mount_path.clone();
        let theme_id = binding.theme_id.clone();

        // Each mounted router gets its own small state (incl. theme_id)
        let state = ThemeAppState {
            theme_client: theme_client.clone(),
            theme_id: theme_id.clone(),
        };

        let nested = Router::new()
            .route("/", any(theme_route_handler))
            .with_state(state)
            // JS-plugin actor client layer with per-plugin orchestration
            .layer(PluginLayer::new(plugin_client.clone(), plugin_ids.clone()))
            // Example per-theme logging / CB layers (still keyed by theme_id)
            .layer(PluginLoggingLayer::new(theme_id.clone()))
            .layer(PluginCircuitBreakerLayer::new(theme_id));

        app = match mount_path.as_str() {
            "/" => app.merge(nested),
            _ => app.nest(&mount_path, nested),
        };
    }

    app
}

// ─────────────────────────────────────────────────────────────────────────────
// Handler + helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Parse raw query string into HashMap<String, String>.
fn parse_query_params(raw_query: &str) -> HashMap<String, String> {
    if raw_query.is_empty() {
        return HashMap::new();
    }

    form_urlencoded::parse(raw_query.as_bytes())
        .into_owned()
        .collect()
}

/// Axum handler for all requests under a given theme mount.
///
/// State carries `theme_client`, `theme_id`, and the content resolver.
#[tracing::instrument(skip_all)]
async fn theme_route_handler(
    State(state): State<ThemeAppState>,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let ThemeAppState {
        theme_client,
        theme_id,
    } = state;

    // Prefer the RequestContext built by the plugin middleware.
    // Fall back to a direct resolver call if it isn't present
    // (e.g., if the route is hit without the middleware in front).
    let ctx: RequestContext = if let Some(existing) = req.extensions().get::<RequestContext>() {
        existing.clone()
    } else {
        let path = req.uri().path().to_string();
        let method = req.method().clone();
        let headers = req.headers().clone();
        let raw_query = req.uri().query().unwrap_or_default().to_string();
        let query_params = parse_query_params(&raw_query);

        let resolved = match resolve(&path, &method) {
            Ok(r) => r,
            Err(_e) => ResolvedContent::empty(),
        };

        build_request_context(path, method, headers, query_params, resolved)
    };

    debug!("theme_id: {}", theme_id);

    // Ask the theme actor to render a ResponseBodySpec.
    let result = theme_client.render(&theme_id, ctx).await;
    debug!("The ResponseBodySpec: {:?}", result);

    let resp = match result {
        Ok(ResponseBodySpec::HtmlString(html)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(html))
            .unwrap(),

        Ok(ResponseBodySpec::JsonValue(val)) => {
            let body = serde_json::to_vec(&val).unwrap_or_else(|_| b"{}".to_vec());
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap()
        }

        Ok(ResponseBodySpec::HtmlTemplate { .. }) => Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .body(Body::from(
                "HtmlTemplate rendering is not wired at the edge layer",
            ))
            .unwrap(),

        Ok(ResponseBodySpec::None | ResponseBodySpec::Unset) => Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap(),

        Err(e) => {
            error!("Theme runtime error: {}", e);
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Theme runtime error"))
                .unwrap()
        }
    };

    Ok(resp)
}
