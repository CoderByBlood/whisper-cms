// crates/adapt/src/http/theme.rs

use crate::http::app::AppState;
use actix_web::{web, HttpMessage, HttpRequest, HttpResponse, Result};
use http as http1;
use http1::StatusCode;
use serve::resolver::{build_request_context, resolve};
use serve::{
    render::http::{RequestContext, ResponseBodySpec},
    resolver::ResolverError,
};
use std::borrow::Cow;
use std::collections::HashMap;

/// Simple query string parsing into HashMap<String, String>.
fn parse_query_params(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();

    if raw.is_empty() {
        return out;
    }

    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("").to_string();
        if k.is_empty() {
            continue;
        }
        let v = it.next().unwrap_or("").to_string();
        out.insert(k, v);
    }

    out
}

/// Actix handler that resolves content (or uses a pre-built RequestContext)
/// and then delegates to the theme runtime.
#[tracing::instrument(skip_all)]
pub async fn theme_entrypoint(
    state: web::Data<AppState>,
    req: HttpRequest,
) -> Result<HttpResponse> {
    // Prefer a RequestContext built by middleware (plugins).
    let ctx: RequestContext = if let Some(existing) = req.extensions().get::<RequestContext>() {
        existing.clone()
    } else {
        // Fallback: resolve synchronously here if no middleware has done it.
        let path = req.path().to_string();

        // Convert Actix method to http 1.x Method via string.
        let method_str = req.method().as_str();
        let method = http1::Method::from_bytes(method_str.as_bytes()).unwrap_or(http1::Method::GET);

        // Convert Actix headers to http 1.x HeaderMap.
        let mut headers = http1::HeaderMap::new();
        for (name, value) in req.headers().iter() {
            if let Ok(v_str) = value.to_str() {
                if let Ok(hname) = http1::header::HeaderName::from_bytes(name.as_str().as_bytes()) {
                    if let Ok(hval) = http1::HeaderValue::from_str(v_str) {
                        headers.insert(hname, hval);
                    }
                }
            }
        }

        let raw_query = req.uri().query().unwrap_or_default();
        let query_params = parse_query_params(raw_query);

        let resolved = match resolve(&path, &method) {
            Ok(r) => r,
            Err(e) => {
                let status = match e {
                    ResolverError::Backend(_) | ResolverError::Io(_) => {
                        StatusCode::INTERNAL_SERVER_ERROR
                    }
                };

                let resp = HttpResponse::build(
                    actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap(),
                )
                .body(format!("Resolve error: {e}"));
                return Ok(resp);
            }
        };

        build_request_context(path, method, headers, query_params, resolved)
    };

    // For now we always use the "default" theme id.
    let theme_id: String = Cow::Borrowed("default").into_owned();

    // Render via theme runtime actor.
    let body_spec = match state.theme_rt.render(&theme_id, ctx).await {
        Ok(spec) => spec,
        Err(e) => {
            let resp =
                HttpResponse::InternalServerError().body(format!("Theme runtime error: {e}"));
            return Ok(resp);
        }
    };

    Ok(response_from_body_spec(body_spec))
}

/// Helper: map ResponseBodySpec -> actix_web::HttpResponse.
#[tracing::instrument(skip_all)]
fn response_from_body_spec(spec: ResponseBodySpec) -> HttpResponse {
    match spec {
        ResponseBodySpec::HtmlString(s) => HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .body(s),

        ResponseBodySpec::JsonValue(v) => HttpResponse::Ok()
            .content_type("application/json; charset=utf-8")
            .body(v.to_string()),

        // For now, we just dump the template name and JSON model.
        // Later this can be hooked up to a real templating engine.
        ResponseBodySpec::HtmlTemplate { template, model } => {
            let rendered = format!("TEMPLATE: {template}\nMODEL: {}", model.to_string());
            HttpResponse::Ok()
                .content_type("text/html; charset=utf-8")
                .body(rendered)
        }

        ResponseBodySpec::None | ResponseBodySpec::Unset => HttpResponse::NoContent().finish(),
    }
}
