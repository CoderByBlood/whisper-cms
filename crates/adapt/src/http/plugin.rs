// crates/adapt/src/http/plugin.rs

use super::error::HttpError;
use super::resolver::{self, build_request_context, ContentResolver};
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
    // Kept for compatibility / type-level wiring; not used at runtime
    _resolver: R,
}

impl<S, R> PluginMiddleware<S, R>
where
    S: Service<Request<Body>>,
    R: ContentResolver,
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
    R: ContentResolver + Clone,
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
    R: ContentResolver + Clone + Send + Sync + 'static,
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
        let ctx: RequestContext = build_request_context(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::CoreError;
    use crate::http::resolver::ResolvedContent;
    use axum::response::Response as AxumResponse;
    use domain::content::ContentKind;
    use futures::future::{ready, Ready};
    use futures::task::noop_waker;
    use http::Method;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    // ─────────────────────────────────────────────────────────────────────
    // Test helpers
    // ─────────────────────────────────────────────────────────────────────

    /// Minimal ResolvedContent for tests.
    fn dummy_resolved() -> ResolvedContent {
        ResolvedContent {
            content_kind: ContentKind::Html,
            front_matter: json!({ "title": "test" }),
            body_path: PathBuf::from("/tmp/body.html"),
        }
    }

    /// Inner service that always succeeds and inspects the RequestContext.
    #[derive(Clone, Default)]
    struct InspectingService {
        pub seen_ctx: Arc<Mutex<Option<RequestContext>>>,
    }

    impl Service<Request<Body>> for InspectingService {
        type Response = AxumResponse;
        type Error = HttpError;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: Request<Body>) -> Self::Future {
            let ctx = req.extensions().get::<RequestContext>().cloned();
            *self.seen_ctx.lock().unwrap() = ctx;
            ready(Ok(AxumResponse::new(Body::empty())))
        }
    }

    /// Inner service whose poll_ready returns an error.
    #[derive(Clone, Default)]
    struct ErrorReadyService;

    impl Service<Request<Body>> for ErrorReadyService {
        type Response = AxumResponse;
        type Error = HttpError;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Err(HttpError::Other("poll_ready failed".into())))
        }

        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            // Should never be called in these tests.
            ready(Err(HttpError::Other("call should not be used".into())))
        }
    }

    /// Resolver that always succeeds with a known ResolvedContent.
    #[derive(Clone, Default)]
    struct FakeResolver {
        pub last_path: Arc<Mutex<Option<String>>>,
        pub last_method: Arc<Mutex<Option<Method>>>,
    }

    impl ContentResolver for FakeResolver {
        fn resolve(&self, path: &str, method: &Method) -> Result<ResolvedContent, CoreError> {
            *self.last_path.lock().unwrap() = Some(path.to_string());
            *self.last_method.lock().unwrap() = Some(method.clone());
            Ok(dummy_resolved())
        }
    }

    /// Resolver that always fails, causing PluginMiddleware::call to panic
    /// due to `.expect("content resolver failed")`.
    #[derive(Clone, Default)]
    struct FailingResolver;

    impl ContentResolver for FailingResolver {
        fn resolve(&self, _path: &str, _method: &Method) -> Result<ResolvedContent, CoreError> {
            Err(CoreError::InvalidHeaderValue("resolver failure".into()))
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn plugin_middleware_inserts_request_context() {
        use serde_json::{json, Value as Json};

        let inner = InspectingService::default();
        let seen_ctx = inner.seen_ctx.clone();
        let resolver = FakeResolver::default();

        let mut plugin = PluginMiddleware::new(inner, resolver);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/foo/bar?x=1&y=2")
            .body(Body::empty())
            .unwrap();

        let resp = futures::executor::block_on(plugin.call(req)).unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);

        let ctx_opt = seen_ctx.lock().unwrap().clone();
        let ctx = ctx_opt.expect("RequestContext should be present in extensions");

        // req_path
        assert_eq!(
            ctx.req_path,
            Json::String("/foo/bar".to_string()),
            "req_path should be JSON string '/foo/bar'"
        );

        // req_method
        assert_eq!(
            ctx.req_method,
            Json::String("GET".to_string()),
            "req_method should be JSON string 'GET'"
        );

        // content_meta must contain title from FakeResolver
        assert_eq!(
            ctx.content_meta["title"],
            json!("test"),
            "FakeResolver front matter"
        );

        // query params → req_params JSON object
        let params = ctx
            .req_params
            .as_object()
            .expect("req_params should be a JSON object");

        assert_eq!(params.get("x").unwrap(), "1");
        assert_eq!(params.get("y").unwrap(), "2");
    }

    #[test]
    fn plugin_middleware_parses_query_params_correctly() {
        let inner = InspectingService::default();
        let seen_ctx = inner.seen_ctx.clone();
        let resolver = FakeResolver::default();

        let mut plugin = PluginMiddleware::new(inner, resolver);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/search?q=rust&tags=web&tags=async")
            .body(Body::empty())
            .unwrap();

        futures::executor::block_on(plugin.call(req)).unwrap();

        let ctx = seen_ctx
            .lock()
            .unwrap()
            .clone()
            .expect("RequestContext should be present");

        let params = ctx
            .req_params
            .as_object()
            .expect("req_params should be a JSON object");

        // form_urlencoded::parse → collect into HashMap<String, String>
        // For duplicate keys, the last wins.
        assert_eq!(params.get("q").unwrap(), "rust");
        assert_eq!(params.get("tags").unwrap(), "async");
    }

    #[test]
    fn plugin_middleware_calls_resolver_with_correct_path_and_method() {
        let inner = InspectingService::default();
        let resolver = FakeResolver::default();
        let last_path = resolver.last_path.clone();
        let last_method = resolver.last_method.clone();

        let mut plugin = PluginMiddleware::new(inner, resolver);

        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/items?id=42")
            .body(Body::empty())
            .unwrap();

        futures::executor::block_on(plugin.call(req)).unwrap();

        assert_eq!(last_path.lock().unwrap().as_deref(), Some("/api/items"));
        assert_eq!(last_method.lock().unwrap().as_ref(), Some(&Method::POST));
    }

    #[test]
    fn plugin_middleware_poll_ready_propagates_ok() {
        let inner = InspectingService::default();
        let resolver = FakeResolver::default();
        let mut plugin = PluginMiddleware::new(inner, resolver);

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let res = plugin.poll_ready(&mut cx);

        match res {
            Poll::Ready(Ok(())) => {} // expected
            other => panic!("expected Poll::Ready(Ok(())), got {:?}", other),
        }
    }

    #[test]
    fn plugin_middleware_poll_ready_propagates_error() {
        let inner = ErrorReadyService::default();
        let resolver = FakeResolver::default();
        let mut plugin = PluginMiddleware::new(inner, resolver);

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let res = plugin.poll_ready(&mut cx);

        match res {
            Poll::Ready(Err(_)) => {} // expected
            other => panic!("expected Poll::Ready(Err(_)), got {:?}", other),
        }
    }

    #[test]
    #[should_panic(expected = "content resolver failed")]
    fn plugin_middleware_panics_when_resolver_fails() {
        let inner = InspectingService::default();
        let resolver = FailingResolver::default();
        let mut plugin = PluginMiddleware::new(inner, resolver);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/will/panic")
            .body(Body::empty())
            .unwrap();

        // Because PluginMiddleware::call uses `.expect("content resolver failed")`,
        // a resolver error should panic with that message.
        let _ = futures::executor::block_on(plugin.call(req));
    }
}
