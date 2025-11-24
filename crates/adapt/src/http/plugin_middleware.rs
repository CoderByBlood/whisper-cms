// crates/adapt/src/http/plugin_middleware.rs

//! Tower middleware that *could* call plugin `before`/`after` hooks around
//! requests, using the single-threaded `PluginRuntimeClient`.
//!
//! For Phase 3 this middleware is a pass-through; the plugin runtime
//! is wired at the actor level and can be integrated in the theme
//! entrypoint instead.

use crate::runtime::plugin_actor::PluginRuntimeClient;

use axum::{body::Body, http::Request, response::Response};
use futures::future::BoxFuture;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// Layer that injects plugin middleware into the stack.
#[derive(Clone)]
pub struct PluginLayer {
    plugin_rt: PluginRuntimeClient,
}

impl PluginLayer {
    #[tracing::instrument(skip_all)]
    pub fn new(plugin_rt: PluginRuntimeClient) -> Self {
        Self { plugin_rt }
    }
}

impl<S> Layer<S> for PluginLayer {
    type Service = PluginMiddleware<S>;

    #[tracing::instrument(skip_all)]
    fn layer(&self, inner: S) -> Self::Service {
        PluginMiddleware {
            inner,
            // Keep the client around; we can actually use it later.
            _plugin_rt: self.plugin_rt.clone(),
        }
    }
}

/// Middleware that currently just forwards requests.
#[derive(Clone)]
pub struct PluginMiddleware<S> {
    inner: S,
    // Leading underscore so unused-field warnings are silenced for now.
    _plugin_rt: PluginRuntimeClient,
}

impl<S> Service<Request<Body>> for PluginMiddleware<S>
where
    S: Service<Request<Body>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    #[tracing::instrument(skip_all)]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[tracing::instrument(skip_all)]
    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut inner = self.inner.clone();

        Box::pin(async move {
            // In a future phase, you can:
            //   - Build a RequestContext from `req`
            //   - Call plugin_rt.before_all / after_all around `inner.call(req)`
            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use futures::future::{ready, Ready};
    use futures::task::noop_waker_ref;
    use std::{convert::Infallible, task::Context};
    use tower::Service;

    use crate::js::engine::BoaEngine;
    use crate::runtime::plugin::PluginRuntime;
    use tokio::task::LocalSet;

    // --- Helper: build a valid PluginRuntimeClient for tests ---
    fn test_plugin_client() -> PluginRuntimeClient {
        let runtime = PluginRuntime::new(BoaEngine::new()).expect("No Plugin Runtime");
        PluginRuntimeClient::spawn(runtime)
    }

    // Simple error type
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestError(&'static str);

    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }
    impl std::error::Error for TestError {}

    // --- Inner services for tests ---

    #[derive(Clone)]
    struct OkService;

    impl Service<Request<Body>> for OkService {
        type Response = Response<Body>;
        type Error = Infallible;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            ready(Ok(Response::builder()
                .status(StatusCode::ACCEPTED)
                .body(Body::from("ok"))
                .unwrap()))
        }
    }

    #[derive(Clone)]
    struct ErrService;

    impl Service<Request<Body>> for ErrService {
        type Response = Response<Body>;
        type Error = TestError;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            ready(Err(TestError("call failed")))
        }
    }

    #[derive(Clone)]
    struct ReadyErrorService;

    impl Service<Request<Body>> for ReadyErrorService {
        type Response = Response<Body>;
        type Error = TestError;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Err(TestError("poll_ready failed")))
        }

        fn call(&mut self, _req: Request<Body>) -> Self::Future {
            panic!("call() should not be invoked after poll_ready error");
        }
    }

    // --- Tests ---

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_layer_passes_through_success_response() {
        let local = LocalSet::new();

        local
            .run_until(async {
                let plugin_rt = test_plugin_client();
                let layer = PluginLayer::new(plugin_rt);

                let mut svc = layer.layer(OkService);

                let req = Request::builder().uri("/test").body(Body::empty()).unwrap();

                let resp = svc.call(req).await.unwrap();
                assert_eq!(resp.status(), StatusCode::ACCEPTED);

                let bytes = to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .expect("body read");
                assert_eq!(&bytes[..], b"ok");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_layer_propagates_inner_error() {
        let local = LocalSet::new();

        local
            .run_until(async {
                let plugin_rt = test_plugin_client();
                let layer = PluginLayer::new(plugin_rt);

                let mut svc = layer.layer(ErrService);

                let req = Request::builder().uri("/err").body(Body::empty()).unwrap();

                let result = svc.call(req).await;
                assert!(result.is_err());
                assert_eq!(result.err().unwrap(), TestError("call failed"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_middleware_poll_ready_forwards_error() {
        let local = LocalSet::new();

        local
            .run_until(async {
                let plugin_rt = test_plugin_client();
                let mut svc = PluginMiddleware {
                    inner: ReadyErrorService,
                    _plugin_rt: plugin_rt,
                };

                let waker = noop_waker_ref();
                let mut cx = Context::from_waker(waker);

                match svc.poll_ready(&mut cx) {
                    Poll::Ready(Err(e)) => assert_eq!(e, TestError("poll_ready failed")),
                    other => panic!("expected Poll::Ready(Err), got {other:?}"),
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_middleware_clone_and_multiple_calls_work() {
        let local = LocalSet::new();

        local
            .run_until(async {
                let plugin_rt = test_plugin_client();

                let mut mw = PluginMiddleware {
                    inner: OkService,
                    _plugin_rt: plugin_rt.clone(),
                };

                let mut mw2 = mw.clone();

                let resp1 = mw
                    .call(Request::builder().uri("/1").body(Body::empty()).unwrap())
                    .await
                    .unwrap();

                let resp2 = mw2
                    .call(Request::builder().uri("/2").body(Body::empty()).unwrap())
                    .await
                    .unwrap();

                let body1 = to_bytes(resp1.into_body(), usize::MAX).await.unwrap();
                let body2 = to_bytes(resp2.into_body(), usize::MAX).await.unwrap();

                assert_eq!(&body1[..], b"ok");
                assert_eq!(&body2[..], b"ok");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_layer_and_middleware_are_cloneable() {
        let local = LocalSet::new();

        local
            .run_until(async {
                let plugin_rt = test_plugin_client();

                let layer = PluginLayer::new(plugin_rt.clone());
                let _layer2 = layer.clone();

                let mw = layer.layer(OkService);
                let _mw2 = mw.clone();
            })
            .await;
    }
}
