// crates/adapt/src/http/plugin.rs

use super::error::HttpError;
use axum::body::Body;
use domain::content::ResolvedContent;
use http::{self, Request, Uri};
use serde_json::{json, Map as JsonMap, Value as Json};
use serve::ctx::http::RequestContext;
use serve::resolver;
use std::collections::HashMap;
use std::task::{Context, Poll};
use tower::Service;

/// Canonicalize header names to `Accept-Language` style.
fn canonicalize_header_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper_next = true;

    for ch in raw.chars() {
        if ch == '-' {
            out.push('-');
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.extend(ch.to_lowercase());
        }
    }

    out
}

/// Build a `RequestContext` from the HTTP request pieces plus `ResolvedContent`.
///
/// This mirrors the old default implementation in `serve::resolver`, but lives
/// in adapt now so we don't depend on a specific serve helper API.
fn build_request_context_from_http(
    path: String,
    method: http::Method,
    headers: http::HeaderMap,
    query_params: HashMap<String, String>,
    resolved: ResolvedContent,
) -> RequestContext {
    // headers -> JSON object
    let mut hdr_obj = JsonMap::new();
    for (name, value) in headers.iter() {
        let canonical = canonicalize_header_name(name.as_str());
        hdr_obj.insert(canonical, json!(value.to_str().unwrap_or("")));
    }

    // query_params -> JSON object
    let mut qp_obj = JsonMap::new();
    for (k, v) in query_params.iter() {
        qp_obj.insert(k.clone(), Json::String(v.clone()));
    }

    RequestContext::builder()
        // req_* fields
        .path(Json::String(path))
        .method(Json::String(method.to_string()))
        // let version default from the builder; resolver doesn't know the real version
        .headers(Json::Object(hdr_obj))
        .params(Json::Object(qp_obj))
        // front_matter becomes the initial content_meta shape
        .content_meta(resolved.front_matter)
        // start empty; can be filled later by higher layers
        .theme_config(Json::Object(JsonMap::new()))
        .plugin_configs(HashMap::new())
        .build()
}

/// PluginMiddleware is responsible for:
/// - Resolving content for the incoming request (path → ContentKind, body handle, front matter)
/// - Building a RequestContext
/// - Inserting it into request.extensions()
///
/// In later phases, it will also:
/// - Invoke JS plugin before/after hooks to populate recommendations.
/// For this phase, it only does the context setup.
///
/// Note: we keep the `R` type parameter for compatibility with higher-level
/// wiring (e.g., PluginLayer), but the resolver itself is now the global
/// `serve::resolver::resolve` function; `R` is not used at runtime.
pub struct PluginMiddleware<S, R>
where
    S: Service<Request<Body>>,
{
    inner: S,
    // Kept for compatibility / type-level wiring; not used at runtime
    _resolver: R,
}

impl<S, R> PluginMiddleware<S, R>
where
    S: Service<Request<Body>>,
{
    pub fn new(inner: S, resolver: R) -> Self {
        Self {
            inner,
            _resolver: resolver,
        }
    }
}

impl<S, R> Clone for PluginMiddleware<S, R>
where
    S: Service<Request<Body>> + Clone,
    R: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _resolver: self._resolver.clone(),
        }
    }
}

impl<S, R> Service<Request<Body>> for PluginMiddleware<S, R>
where
    S: Service<Request<Body>, Response = axum::response::Response, Error = HttpError>
        + Send
        + 'static,
    S::Future: Send + 'static,
    R: Clone + Send + Sync + 'static,
{
    type Response = axum::response::Response;
    type Error = HttpError;
    type Future = S::Future;

    #[tracing::instrument(skip_all)]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|e| e)
    }

    #[tracing::instrument(skip_all)]
    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        // Extract path & query params for initial RequestContext.
        let uri: &Uri = req.uri();
        let path = uri.path().to_string();

        let query_params: HashMap<String, String> = uri
            .query()
            .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
            .unwrap_or_else(HashMap::new);

        // Resolve content using the injected, type-erased resolver.
        let resolved = resolver::resolve(&path, req.method())
            .map_err(HttpError::from)
            .expect("content resolver failed"); // In a real system, handle gracefully.

        // Build RequestContext and insert into extensions.
        let ctx: RequestContext = build_request_context_from_http(
            path,
            req.method().clone(),
            req.headers().clone(),
            query_params,
            resolved,
        );

        req.extensions_mut().insert::<RequestContext>(ctx);

        // For this phase, do not invoke any plugin hooks yet.
        self.inner.call(req)
    }
}
