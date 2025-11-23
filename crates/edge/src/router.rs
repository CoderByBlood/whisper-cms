// crates/edge/src/router.rs

use crate::fs::ext::ThemeBinding;
use adapt::http::plugin_middleware::PluginLayer;
use adapt::runtime::bootstrap::RuntimeHandles;
use adapt::runtime::theme_actor::ThemeRuntimeClient;

use axum::Router;
use std::path::PathBuf;
use tower::Layer;

// ─────────────────────────────────────────────────────────────────────────────
// App state per mounted theme
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct ThemeAppState {
    _content_root: PathBuf,
    _theme_client: ThemeRuntimeClient,
    _theme_id: String,
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

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        // You could log here with tracing, including request_id, etc.
        // tracing::debug!(plugin = %self.plugin_id, "plugin middleware invoked");
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
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self {
            _plugin_id: plugin_id.into(),
        }
    }
}

impl<S> Layer<S> for PluginCircuitBreakerLayer {
    type Service = S;

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
pub fn build_app_router(
    content_root: PathBuf,
    handles: RuntimeHandles,
    bindings: Vec<ThemeBinding>,
) -> Router {
    let plugin_client = handles.plugin_client.clone();
    let theme_client = handles.theme_client.clone();

    let mut app = Router::new();

    for binding in bindings {
        let mount_path = binding.mount_path.clone();
        let theme_id = binding.theme_name.clone();

        // Each mounted router gets its own small state (incl. theme_id)
        let state = ThemeAppState {
            _content_root: content_root.clone(),
            _theme_client: theme_client.clone(),
            _theme_id: theme_id.clone(),
        };

        let nested = Router::new()
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
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use adapt::core::{RequestContext, ResponseBodySpec};
    use axum::{
        body::Body,
        extract::{Request, State},
        http::Uri,
        response::Response,
    };
    use domain::doc::BodyKind;
    use http::{header, HeaderMap, Method, Request as HttpRequest, StatusCode, Version};
    use serde_json::{json, Map as JsonMap, Value as Json};
    use std::{collections::HashMap, convert::Infallible, path::Path, str::FromStr};

    // ─────────────────────────────────────────────────────────────────────────────
    // Handler + helpers
    // ─────────────────────────────────────────────────────────────────────────────

    /// Axum handler for all requests under a given theme mount.
    ///
    /// State carries `content_root`, `theme_client`, and the bound `theme_id`.
    async fn _theme_route_handler(
        State(state): State<ThemeAppState>,
        req: Request<Body>,
    ) -> Result<Response<Body>, Infallible> {
        let ThemeAppState {
            _content_root: content_root,
            _theme_client: theme_client,
            _theme_id: theme_id,
        } = state;

        // Build a RequestContext from the HTTP request + content root.
        let ctx = build_request_context(&req, &content_root);

        // Ask the theme actor to render a ResponseBodySpec.
        let result = theme_client.render(&theme_id, ctx).await;

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

            Err(_e) => Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Theme runtime error"))
                .unwrap(),
        };

        Ok(resp)
    }

    fn http_version_to_json(version: Version) -> Json {
        let s = match version {
            Version::HTTP_09 => "HTTP/0.9",
            Version::HTTP_10 => "HTTP/1.0",
            Version::HTTP_11 => "HTTP/1.1",
            Version::HTTP_2 => "HTTP/2.0",
            Version::HTTP_3 => "HTTP/3.0",
            _ => "HTTP/1.1",
        };
        Json::String(s.to_string())
    }

    fn headers_to_json(headers: &HeaderMap) -> Json {
        let mut obj = JsonMap::new();
        for (name, value) in headers.iter() {
            if let Ok(s) = value.to_str() {
                obj.insert(name.to_string(), Json::String(s.to_string()));
            }
        }
        Json::Object(obj)
    }

    fn query_to_params_json(raw_query: &str) -> Json {
        if raw_query.is_empty() {
            return Json::Object(JsonMap::new());
        }

        let map: HashMap<String, String> = form_urlencoded::parse(raw_query.as_bytes())
            .into_owned()
            .collect();
        serde_json::to_value(map).unwrap_or_else(|_| Json::Object(JsonMap::new()))
    }

    /// Build a `RequestContext` from an Axum `Request<Body>`.
    ///
    /// - Parses query params into a `HashMap<String, String>`
    /// - Infers `content_kind` + `body_path` from the request path & extensions.
    fn build_request_context(req: &Request<Body>, _content_root: &Path) -> RequestContext {
        let path_str = req.uri().path().to_string();
        let raw_query = req.uri().query().unwrap_or_default();

        let req_path = Json::String(path_str);
        let req_method = Json::String(req.method().to_string());
        let req_version = http_version_to_json(req.version());
        let req_headers = headers_to_json(req.headers());
        let req_params = query_to_params_json(raw_query);

        // For now, content_meta/theme_config/plugin_configs are empty; they’re
        // filled in by other parts of the pipeline (resolver, settings, etc.).
        RequestContext::builder()
            .path(req_path)
            .method(req_method)
            .version(req_version)
            .headers(req_headers)
            .params(req_params)
            .content_meta(json!({}))
            .theme_config(json!({}))
            .plugin_configs(HashMap::new())
            .build()
    }

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

    #[test]
    fn build_request_context_populates_fields() {
        let uri = Uri::from_str("/posts/hello.md?tag=rust&tag2=web").unwrap();

        let req: HttpRequest<Body> = HttpRequest::builder()
            .method(Method::GET)
            .uri(uri)
            .header("x-test", "yes")
            .body(Body::empty())
            .unwrap();

        let root = PathBuf::from("/content");
        let ctx = build_request_context(&req, &root);

        //
        // req_path
        //
        assert_eq!(
            ctx.req_path,
            Json::String("/posts/hello.md".to_string()),
            "req_path must be JSON string"
        );

        //
        // req_method
        //
        assert_eq!(
            ctx.req_method,
            Json::String("GET".to_string()),
            "req_method must be JSON string"
        );

        //
        // req_headers
        //
        let hdrs = ctx
            .req_headers
            .as_object()
            .expect("req_headers must be object");
        assert_eq!(
            hdrs.get("x-test").unwrap(),
            "yes",
            "header x-test should be captured"
        );

        //
        // req_params
        //
        let params = ctx
            .req_params
            .as_object()
            .expect("req_params must be object");
        assert_eq!(params.get("tag").unwrap(), "rust");
        assert_eq!(params.get("tag2").unwrap(), "web");

        //
        // content_meta
        //
        assert!(
            ctx.content_meta.is_object(),
            "content_meta starts as empty object"
        );
        assert!(
            ctx.content_meta.as_object().unwrap().is_empty(),
            "content_meta must be empty"
        );

        //
        // theme_config
        //
        assert!(
            ctx.theme_config.is_object(),
            "theme_config starts as empty object"
        );

        //
        // plugin_configs
        //
        assert!(ctx.plugin_configs.is_empty(), "plugin configs start empty");

        //
        // req_id must be auto-generated UUID string
        //
        if let Json::String(s) = &ctx.req_id {
            assert!(
                uuid::Uuid::parse_str(s).is_ok(),
                "req_id must be a valid UUID string"
            );
        } else {
            panic!("req_id must be JSON string");
        }

        //
        // req_version must be JSON string
        //
        assert!(
            ctx.req_version.is_string(),
            "req_version must be a JSON string"
        );

        //
        // Streams must start unset
        //
        assert!(ctx.req_body.is_none());
        assert!(ctx.content_body.is_none());
    }
}
