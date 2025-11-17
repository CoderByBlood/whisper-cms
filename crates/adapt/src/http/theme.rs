use super::error::HttpError;
use crate::core::context::{RequestContext, ResponseBodySpec};
use crate::core::recommendation::{BodyPatch, HeaderPatchKind};
use crate::render::{render_html_template_to, render_json_to, TemplateEngine};
use axum::body::Body;
use axum::response::Response;
use http::Request;
use serde_json::Value as Json;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::Service;

/// Trait implemented by a theme in Phase 3 (Rust-side).
///
/// Later phases will replace the implementation with JS `handle(ctx)`.
pub trait ThemeHandler: Send + Sync + 'static {
    fn handle(&self, ctx: &mut RequestContext) -> Result<(), HttpError>;
}

/// A simple example theme handler used for Phase 3.
///
/// It looks at the content kind and:
/// - For HtmlContent: uses a hard-coded template name "default" and model = front_matter.
/// - For JsonContent: uses JsonValue = { "ok": true }.
/// This is just to validate the pipeline; real themes will come later.
pub struct SimpleThemeHandler;

impl ThemeHandler for SimpleThemeHandler {
    fn handle(&self, ctx: &mut RequestContext) -> Result<(), HttpError> {
        // For now, we let the outer layers set up the ResponseBodySpec.
        // This simple handler is mostly a placeholder. You could imagine:
        //
        // - Choosing template based on front matter.
        // - Populating ctx.response_spec.model from front matter + content.
        //
        // In this phase, assume ctx.response_spec was already set up.
        if let ResponseBodySpec::Unset = ctx.response_spec.body {
            // Fallback: basic JSON response to prove the path works.
            ctx.response_spec.body = ResponseBodySpec::JsonValue(Json::from(serde_json::json!({
                "ok": true,
                "path": ctx.path,
            })));
        }

        Ok(())
    }
}

/// ThemeService drives the theme + render pipeline for a single request.
///
/// It expects a RequestContext to have been inserted into the request
/// extensions by an outer layer (e.g. PluginMiddleware).
pub struct ThemeService<H, T>
where
    H: ThemeHandler,
    T: TemplateEngine,
{
    theme: Arc<H>,
    template_engine: T,
}

impl<H, T> ThemeService<H, T>
where
    H: ThemeHandler,
    T: TemplateEngine,
{
    pub fn new(theme: H, template_engine: T) -> Self {
        Self {
            theme: Arc::new(theme),
            template_engine,
        }
    }
}

impl<H, T> Service<Request<Body>> for ThemeService<H, T>
where
    H: ThemeHandler,
    T: TemplateEngine + Clone + Send + 'static,
{
    type Response = Response<Body>;
    type Error = HttpError;
    type Future = futures::future::BoxFuture<'static, Result<Response<Body>, HttpError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // ThemeService has no internal backpressure.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        let theme = Arc::clone(&self.theme);
        let template_engine = self.template_engine.clone();

        // Extract RequestContext from extensions.
        let mut ctx = match req.extensions_mut().remove::<RequestContext>() {
            Some(c) => c,
            None => return Box::pin(async { Err(HttpError::MissingContext) }),
        };

        Box::pin(async move {
            // Let the theme populate ResponseSpec.
            theme.handle(&mut ctx)?;

            // 1. Apply header patches.
            for hp in &ctx.recommendations.header_patches {
                match hp.kind {
                    HeaderPatchKind::Set | HeaderPatchKind::Append | HeaderPatchKind::Remove => {
                        hp.apply_to_headers(&mut ctx.response_spec.headers);
                    }
                }
            }

            // 2. Apply model patches if this is an HtmlTemplate.
            if let ResponseBodySpec::HtmlTemplate { ref mut model, .. } = ctx.response_spec.body {
                for mp in &ctx.recommendations.model_patches {
                    if let Err(e) = mp.apply_to_model(model) {
                        // In this pipeline, a failed model patch should not crash the request;
                        // we log and continue instead of turning it into an HttpError.
                        eprintln!("model patch failed: {}", e);
                    }
                }
            }

            // 3. Render body with body patches (BodyRegex + HtmlDom/JsonPatch).
            let mut body_bytes: Vec<u8> = Vec::new();
            let body_patches: &[BodyPatch] = &ctx.recommendations.body_patches;

            match &ctx.response_spec.body {
                ResponseBodySpec::HtmlTemplate { template, model } => {
                    render_html_template_to(
                        &template_engine,
                        template,
                        model,
                        body_patches,
                        &mut body_bytes,
                    )?;
                }
                ResponseBodySpec::HtmlString(html) => {
                    // Run body regex + HtmlDom over a single string.
                    render_html_template_to(
                        &template_engine,
                        "__inline_html__",
                        &serde_json::json!({ "body": html }),
                        body_patches,
                        &mut body_bytes,
                    )?;
                    // Note: in a real system you'd have a dedicated path for raw HTML.
                }
                ResponseBodySpec::JsonValue(value) => {
                    render_json_to(value, body_patches, &mut body_bytes)?;
                }
                ResponseBodySpec::None => {
                    // Explicit "no body" – leave body_bytes empty.
                    // The builder below will turn this into an empty response body.
                }
                ResponseBodySpec::Unset => {
                    // If still unset, this is an internal error in the theme.
                    return Err(HttpError::Other("missing body spec in theme".to_string()));
                }
            }

            // 4. Build the final http::Response using the status + headers from ctx.response_spec.
            let mut builder = http::Response::builder().status(ctx.response_spec.status);

            for (name, value) in ctx.response_spec.headers.iter() {
                builder = builder.header(name, value);
            }

            let body = if body_bytes.is_empty() {
                Body::empty()
            } else {
                Body::from(body_bytes)
            };

            let response = builder
                .body(body)
                .map_err(|e| HttpError::Other(e.to_string()))?;

            Ok(response)
        })
    }
}
