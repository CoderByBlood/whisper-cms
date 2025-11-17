use super::error::HttpError;
use super::resolver::{build_request_context, ContentResolver};
use crate::core::context::RequestContext;
use axum::body::Body;
use http::{Request, Uri};
use std::collections::HashMap;
use std::task::{Context, Poll};
use tower::Service;

/// PluginMiddleware is responsible for:
/// - Resolving content for the incoming request (path → ContentKind, body path, front matter)
/// - Building a RequestContext
/// - Inserting it into request.extensions()
///
/// In later phases, it will also:
/// - Invoke JS plugin before/after hooks to populate recommendations.
/// For Phase 3, it only does the context setup.
pub struct PluginMiddleware<S, R>
where
    S: Service<Request<Body>>,
    R: ContentResolver,
{
    inner: S,
    resolver: R,
}

impl<S, R> PluginMiddleware<S, R>
where
    S: Service<Request<Body>>,
    R: ContentResolver,
{
    pub fn new(inner: S, resolver: R) -> Self {
        Self { inner, resolver }
    }
}

impl<S, R> Clone for PluginMiddleware<S, R>
where
    S: Service<Request<Body>> + Clone,
    R: ContentResolver + Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            resolver: self.resolver.clone(),
        }
    }
}

impl<S, R> Service<Request<Body>> for PluginMiddleware<S, R>
where
    S: Service<Request<Body>, Response = axum::response::Response, Error = HttpError>
        + Send
        + 'static,
    S::Future: Send + 'static,
    R: ContentResolver + Clone + Send + Sync + 'static,
{
    type Response = axum::response::Response;
    type Error = HttpError;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|e| e)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        // Extract path & query params for initial RequestContext.
        let uri: &Uri = req.uri();
        let path = uri.path().to_string();

        let query_params: HashMap<String, String> = uri
            .query()
            .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
            .unwrap_or_else(HashMap::new);

        // Resolve content.
        let resolved = self
            .resolver
            .resolve(&path, req.method())
            .map_err(HttpError::from)
            .expect("content resolver failed"); // In a real system, handle gracefully.

        // Build RequestContext and insert into extensions.
        let ctx = build_request_context(
            path,
            req.method().clone(),
            req.headers().clone(),
            query_params,
            resolved,
        );

        req.extensions_mut().insert::<RequestContext>(ctx);

        // For Phase 3, do not invoke any plugin hooks yet.
        self.inner.call(req)
    }
}
