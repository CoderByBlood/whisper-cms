// crates/edge/src/router.rs
//
use adapt::http::plugin_middleware::PluginLayer;
use adapt::runtime::bootstrap::RuntimeHandles;
use adapt::runtime::theme_actor::ThemeRuntimeClient;
use domain::content::ResolvedContent;
use serve::resolver::ContentResolver;
use serve::{ctx::http::ResponseBodySpec, resolver::build_request_context};

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use domain::content::ContentKind;
use http::header;
use serde_json::json;
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use tower::Layer;
use tracing::{debug, error};

use crate::db::resolver::IndexedContentResolver;
use crate::fs::ext::ThemeBinding;

// ─────────────────────────────────────────────────────────────────────────────
// App state per mounted theme
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct ThemeAppState {
    theme_client: ThemeRuntimeClient,
    theme_id: String,
    resolver: Arc<dyn ContentResolver>,
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
pub fn build_app_router(
    content_root: PathBuf,
    handles: RuntimeHandles,
    bindings: Vec<ThemeBinding>,
) -> Router {
    let plugin_client = handles.plugin_client.clone();
    let theme_client = handles.theme_client.clone();

    // Single shared filesystem resolver for all themes.
    let resolver: Arc<dyn ContentResolver> =
        Arc::new(IndexedContentResolver::new(content_root.clone()));

    let mut app = Router::new();

    for binding in bindings {
        let mount_path = binding.mount_path.clone();
        let theme_id = binding.theme_id.clone();

        // Each mounted router gets its own small state (incl. theme_id)
        let state = ThemeAppState {
            theme_client: theme_client.clone(),
            theme_id: theme_id.clone(),
            resolver: resolver.clone(),
        };

        let nested = Router::new()
            .route("/", any(theme_route_handler))
            .with_state(state)
            // JS-plugin actor client layer (currently pass-through)
            .layer(PluginLayer::new(plugin_client.clone()))
            // Example per-theme logging / CB layers
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
        resolver,
    } = state;

    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let headers = req.headers().clone();
    let raw_query = req.uri().query().unwrap_or_default().to_string();
    let query_params = parse_query_params(&raw_query);

    // Ask the resolver for content info (kind + front matter + body path).
    let resolved = match resolver.resolve(&path, &method) {
        Ok(r) => r,
        Err(_e) => ResolvedContent {
            content_kind: ContentKind::Asset,
            front_matter: json!({}),
            body_path: PathBuf::new(),
        },
    };

    // Build RequestContext using the adapt-side helper.
    let ctx = build_request_context(path.clone(), method, headers, query_params, resolved);

    debug!("theme_id: {}", theme_id);

    // Ask the theme actor to render a ResponseBodySpec.
    let result = theme_client.render(&theme_id, ctx).await;
    debug!("!!!! HERE WE GO !!!! The ResponseBodySpec: {:?}", result);

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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use domain::doc::BodyKind;
    use std::{collections::HashMap, path::Path};

    /// Parse a query string into a `HashMap<String, String>`, URL-decoding keys/values.
    ///
    /// E.g. `a=1&b=hello+world` → { "a": "1", "b": "hello world" }
    fn parse_query_params(query: &str) -> HashMap<String, String> {
        if query.is_empty() {
            return HashMap::new();
        }

        form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect()
    }

    /// Infer `ContentKind` and `body_path` from the request path and content root.
    ///
    /// Rules:
    /// - `/` or `/foo/` → treat as `/foo/index` and assume `.html`
    /// - Known extensions:
    ///   - `.md`, `.markdown` → Markdown
    ///   - `.html`, `.htm`    → Html
    ///   - `.adoc`, `.asciidoc` → AsciiDoc
    ///   - `.txt`              → Plain
    /// - Unknown / no extension → default to Html + `.html`
    ///
    /// The returned `body_path` is `content_root` joined with the normalized path.
    fn infer_body_kind_and_body_path(root: &Path, path: &str) -> (BodyKind, PathBuf) {
        // Normalize path: ensure leading slash, and handle directories as `/.../index`
        let mut normalized = if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        };

        if !normalized.starts_with('/') {
            normalized.insert(0, '/');
        }

        if normalized.ends_with('/') {
            normalized.push_str("index");
        }

        // Split off extension (if any)
        let (stem, ext_opt) = match normalized.rsplit_once('.') {
            Some((stem, ext)) => (stem.to_string(), Some(ext.to_lowercase())),
            None => (normalized.clone(), None),
        };

        let (kind, effective_path) = match ext_opt.as_deref() {
            Some("md") | Some("markdown") => (BodyKind::Markdown, normalized),
            Some("html") | Some("htm") => (BodyKind::Html, normalized),
            Some("adoc") | Some("asciidoc") => (BodyKind::AsciiDoc, normalized),
            Some("txt") => (BodyKind::Plain, normalized),
            // No extension or unknown: assume Html + ".html"
            _ => {
                let mut p = stem;
                p.push_str(".html");
                (BodyKind::Html, p)
            }
        };

        // Strip leading slash when joining with a root filesystem path.
        let rel = effective_path.trim_start_matches('/');
        let body_path = root.join(rel);

        (kind, body_path)
    }

    #[test]
    fn parse_query_params_empty() {
        let map = parse_query_params("");
        assert!(map.is_empty());
    }

    #[test]
    fn parse_query_params_simple_pairs() {
        let map = parse_query_params("a=1&b=two");
        assert_eq!(map.get("a").unwrap(), "1");
        assert_eq!(map.get("b").unwrap(), "two");
    }

    #[test]
    fn parse_query_params_url_decodes() {
        let map = parse_query_params("q=hello+world&x=%2Ffoo%2Fbar");
        assert_eq!(map.get("q").unwrap(), "hello world");
        assert_eq!(map.get("x").unwrap(), "/foo/bar");
    }

    #[test]
    fn infer_content_kind_and_body_path_root_html_default() {
        let root = PathBuf::from("/content");
        let (kind, body_path) = infer_body_kind_and_body_path(&root, "/");

        assert_eq!(kind, BodyKind::Html);
        assert_eq!(body_path, PathBuf::from("/content/index.html"));
    }

    #[test]
    fn infer_content_kind_and_body_path_md() {
        let root = PathBuf::from("/content");
        let (kind, body_path) = infer_body_kind_and_body_path(&root, "/posts/hello.md");

        assert_eq!(kind, BodyKind::Markdown);
        assert_eq!(body_path, PathBuf::from("/content/posts/hello.md"));
    }

    #[test]
    fn infer_content_kind_and_body_path_no_ext_defaults_to_html() {
        let root = PathBuf::from("/content");
        let (kind, body_path) = infer_body_kind_and_body_path(&root, "/about");

        assert_eq!(kind, BodyKind::Html);
        assert_eq!(body_path, PathBuf::from("/content/about.html"));
    }

    #[test]
    fn infer_content_kind_and_body_path_trailing_slash() {
        let root = PathBuf::from("/content");
        let (kind, body_path) = infer_body_kind_and_body_path(&root, "/docs/");

        assert_eq!(kind, BodyKind::Html);
        assert_eq!(body_path, PathBuf::from("/content/docs/index.html"));
    }
}
