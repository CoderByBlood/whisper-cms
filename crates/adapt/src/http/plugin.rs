// crates/adapt/src/http/plugin.rs

//! PluginMiddleware
//!
//! Actix-web middleware that:
//!   - Reads `RequestContext` from request extensions (if present).
//!   - Calls `before_plugin` for each plugin in forward order,
//!     threading the updated `RequestContext` through each call.
//!   - Inserts the final `RequestContext` back into the request extensions.
//!   - Delegates to the inner service.
//!   - After the response, calls `after_plugin` in *reverse* order,
//!     again threading the updated `RequestContext`.
//!
//! No `unsafe` is used. The inner service is wrapped in `Rc<RefCell<_>>`
//! so it can be moved into the async future while still satisfying
//! Actix's `Service` trait requirements.

use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

use actix_web::{
    dev::{Service, ServiceRequest, ServiceResponse, Transform},
    Error, HttpMessage,
};

use serve::render::http::RequestContext;

use crate::runtime::PluginRuntimeClient;

/// Actix middleware factory: holds shared plugin runtime + ordered plugin IDs.
#[derive(Clone)]
pub struct PluginMiddleware {
    plugin_client: PluginRuntimeClient,
    plugin_ids: Vec<String>,
}

impl PluginMiddleware {
    /// Create a new PluginMiddleware with a plugin runtime client and an
    /// ordered list of plugin IDs.
    pub fn new(plugin_client: PluginRuntimeClient, plugin_ids: Vec<String>) -> Self {
        Self {
            plugin_client,
            plugin_ids,
        }
    }
}

impl<S, B> Transform<S, ServiceRequest> for PluginMiddleware
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = PluginMiddlewareService<S>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Transform, Self::InitError>>>>;

    fn new_transform(&self, service: S) -> Self::Future {
        let plugin_client = self.plugin_client.clone();
        let plugin_ids = self.plugin_ids.clone();

        Box::pin(async move {
            Ok(PluginMiddlewareService {
                inner: Rc::new(RefCell::new(service)),
                plugin_client,
                plugin_ids,
            })
        })
    }
}

/// Middleware service: actually processes requests/responses.
pub struct PluginMiddlewareService<S> {
    inner: Rc<RefCell<S>>,
    plugin_client: PluginRuntimeClient,
    plugin_ids: Vec<String>,
}

impl<S, B> Service<ServiceRequest> for PluginMiddlewareService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.borrow_mut().poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let inner = Rc::clone(&self.inner);
        let plugin_client = self.plugin_client.clone();
        let plugin_ids = self.plugin_ids.clone();

        // Grab any existing RequestContext from extensions.
        let ctx_opt = {
            let exts = req.extensions();
            exts.get::<RequestContext>().cloned()
        };

        Box::pin(async move {
            // If we don't have a RequestContext, just pass through.
            let Some(mut ctx) = ctx_opt else {
                return inner.borrow_mut().call(req).await;
            };

            // ─────────────────────────────────────────────────────────────
            // BEFORE: run plugins in forward order, threading ctx.
            // NOTE: we clone() ctx to avoid moving it; the actor owns its
            //       parameter and returns an updated RequestContext.
            // ─────────────────────────────────────────────────────────────
            for plugin_id in &plugin_ids {
                match plugin_client
                    .before_plugin(plugin_id.clone(), ctx.clone())
                    .await
                {
                    Ok(new_ctx) => {
                        ctx = new_ctx;
                    }
                    Err(err) => {
                        tracing::error!(
                            "before_plugin(\"{plugin_id}\") failed: {err}; continuing request"
                        );
                        // Keep previous ctx and continue; you can short-circuit here instead.
                    }
                }
            }

            // Store the updated ctx back into the request extensions so
            // handlers and later middleware can see it.
            {
                let mut exts = req.extensions_mut();
                exts.insert::<RequestContext>(ctx.clone());
            }

            // Call the inner service (theme handler, etc).
            let resp = inner.borrow_mut().call(req).await?;

            // ─────────────────────────────────────────────────────────────
            // AFTER: run plugins in reverse order.
            // We start from whatever RequestContext is now in the request
            // extensions (so any handler updates are respected).
            // ─────────────────────────────────────────────────────────────
            let ctx_after_opt = {
                let exts = resp.request().extensions();
                exts.get::<RequestContext>().cloned()
            };

            let Some(mut ctx_after) = ctx_after_opt else {
                return Ok(resp);
            };

            for plugin_id in plugin_ids.iter().rev() {
                match plugin_client
                    .after_plugin(plugin_id.clone(), ctx_after.clone())
                    .await
                {
                    Ok(new_ctx) => {
                        ctx_after = new_ctx;
                    }
                    Err(err) => {
                        tracing::error!(
                            "after_plugin(\"{plugin_id}\") failed: {err}; continuing response"
                        );
                    }
                }
            }

            // If you want to do something with ctx_after (e.g., logging),
            // you can do it here. We don't try to mutate extensions again.

            Ok(resp)
        })
    }
}
