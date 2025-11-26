// crates/adapt/src/http/theme.rs

use crate::http::app::AppState;
use axum::{
    body::Body,
    extract::{Path, State},
    http::Request,
    response::Response,
};
use http::StatusCode;
use serve::resolver::{build_request_context, resolve};
use serve::{
    ctx::http::{RequestContext, ResponseBodySpec},
    resolver::ResolverError,
};
use std::borrow::Cow;
use std::collections::HashMap;

/// Axum handler that resolves content and then delegates to the theme runtime.
#[tracing::instrument(skip_all)]
pub async fn theme_entrypoint(
    State(state): State<AppState>,
    Path(path): Path<String>,
    req: Request<Body>,
) -> Result<Response, Response> {
    // Very simple query string parsing into a HashMap<String, String>.
    let mut query_params = HashMap::new();
    if let Some(q) = req.uri().query() {
        for pair in q.split('&') {
            if pair.is_empty() {
                continue;
            }
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("").to_string();
            if k.is_empty() {
                continue;
            }
            let v = it.next().unwrap_or("").to_string();
            query_params.insert(k, v);
        }
    }

    // Resolve content by path + method using the injected resolver.
    let resolved = resolve(&path, req.method()).map_err(|e| {
        let status = match e {
            ResolverError::Backend(_) => 500,
            ResolverError::Io(_) => 500,
        };

        Response::builder()
            .status(StatusCode::from_u16(status).unwrap())
            .body(Body::from(format!("Resolve error: {e}")))
            .unwrap()
    })?;

    // Build RequestContext from HTTP request + resolved content.
    let ctx: RequestContext = build_request_context(
        path.clone(),
        req.method().clone(),
        req.headers().clone(),
        query_params,
        resolved,
    );

    // Decide which theme to use. For this phase, we always use "default".
    // This can be extended later to inspect front matter / config.
    let theme_id = Cow::Borrowed("default").into_owned();

    // Render via theme runtime actor.
    let body_spec = state.theme_rt.render(&theme_id, ctx).await.map_err(|e| {
        Response::builder()
            .status(500)
            .body(Body::from(format!("Theme runtime error: {e}")))
            .unwrap()
    })?;

    Ok(response_from_body_spec(body_spec))
}

/// Helper: map ResponseBodySpec -> hyper::Response<Body>.
#[tracing::instrument(skip_all)]
fn response_from_body_spec(spec: ResponseBodySpec) -> Response {
    match spec {
        ResponseBodySpec::HtmlString(s) => Response::builder()
            .status(200)
            .header("content-type", "text/html; charset=utf-8")
            .body(Body::from(s))
            .unwrap(),

        ResponseBodySpec::JsonValue(v) => Response::builder()
            .status(200)
            .header("content-type", "application/json; charset=utf-8")
            .body(Body::from(v.to_string()))
            .unwrap(),

        // For now, we just dump the template name and JSON model.
        // Later this can be hooked up to a real templating engine.
        ResponseBodySpec::HtmlTemplate { template, model } => {
            let rendered = format!("TEMPLATE: {template}\nMODEL: {}", model.to_string());
            Response::builder()
                .status(200)
                .header("content-type", "text/html; charset=utf-8")
                .body(Body::from(rendered))
                .unwrap()
        }

        ResponseBodySpec::None | ResponseBodySpec::Unset => {
            Response::builder().status(204).body(Body::empty()).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde_json::json;

    // ─────────────────────────────────────────────────────────────────────────
    // ResponseBodySpec → Response mapping tests
    // ─────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn html_string_maps_to_200_and_html_content_type() {
        let spec = ResponseBodySpec::HtmlString("<h1>Hello</h1>".to_string());

        let resp = response_from_body_spec(spec);

        assert_eq!(resp.status().as_u16(), 200);

        let content_type = resp.headers().get("content-type").unwrap();
        assert_eq!(
            content_type.to_str().unwrap(),
            "text/html; charset=utf-8",
            "HtmlString responses should have HTML content-type"
        );

        let body_bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(
            &body_bytes[..],
            b"<h1>Hello</h1>",
            "HtmlString body should be passed through as-is"
        );
    }

    #[tokio::test]
    async fn json_value_maps_to_200_and_json_content_type() {
        let value = json!({ "ok": true, "answer": 42 });
        let spec = ResponseBodySpec::JsonValue(value.clone());

        let resp = response_from_body_spec(spec);

        assert_eq!(resp.status().as_u16(), 200);

        let content_type = resp.headers().get("content-type").unwrap();
        assert_eq!(
            content_type.to_str().unwrap(),
            "application/json; charset=utf-8",
            "JsonValue responses should have JSON content-type"
        );

        let body_bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let decoded: serde_json::Value =
            serde_json::from_slice(&body_bytes).expect("body should be valid JSON");

        assert_eq!(
            decoded, value,
            "JSON content should round-trip correctly through the response body"
        );
    }

    #[tokio::test]
    async fn html_template_renders_template_and_model() {
        let template = "page.html".to_string();
        let model = json!({
            "title": "Hello",
            "count": 3,
            "nested": { "k": "v" }
        });

        let spec = ResponseBodySpec::HtmlTemplate {
            template: template.clone(),
            model: model.clone(),
        };

        let resp = response_from_body_spec(spec);

        assert_eq!(resp.status().as_u16(), 200);

        let content_type = resp.headers().get("content-type").unwrap();
        assert_eq!(
            content_type.to_str().unwrap(),
            "text/html; charset=utf-8",
            "HtmlTemplate responses should have HTML content-type"
        );

        let body_bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body = String::from_utf8(body_bytes.to_vec()).expect("body should be UTF-8");

        // Should follow the "TEMPLATE: {template}\nMODEL: {json}" pattern.
        assert!(
            body.starts_with("TEMPLATE: "),
            "body should start with TEMPLATE: prefix, got: {body}"
        );
        assert!(
            body.contains(&template),
            "body should contain template name {template}, got: {body}"
        );
        assert!(
            body.contains("MODEL: "),
            "body should contain MODEL: prefix, got: {body}"
        );

        // Extract the MODEL portion roughly and ensure the JSON parses/equates.
        let model_prefix = "MODEL: ";
        let model_pos = body
            .find(model_prefix)
            .expect("body should contain MODEL: segment");
        let json_str = &body[model_pos + model_prefix.len()..];

        let parsed_model: serde_json::Value =
            serde_json::from_str(json_str).expect("MODEL JSON should parse");
        assert_eq!(
            parsed_model, model,
            "MODEL JSON in rendered body should match the original model"
        );
    }

    #[tokio::test]
    async fn none_maps_to_204_with_empty_body_and_no_content_type() {
        let spec = ResponseBodySpec::None;

        let resp = response_from_body_spec(spec);

        assert_eq!(
            resp.status().as_u16(),
            204,
            "None body should map to 204 No Content"
        );

        // No content-type header should be set for an empty body.
        assert!(
            resp.headers().get("content-type").is_none(),
            "204 responses for None should not set content-type"
        );

        let body_bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert!(
            body_bytes.is_empty(),
            "204 responses should have an empty body for None"
        );
    }

    #[tokio::test]
    async fn unset_maps_to_204_with_empty_body_and_no_content_type() {
        let spec = ResponseBodySpec::Unset;

        let resp = response_from_body_spec(spec);

        assert_eq!(
            resp.status().as_u16(),
            204,
            "Unset body should map to 204 No Content"
        );

        assert!(
            resp.headers().get("content-type").is_none(),
            "204 responses for Unset should not set content-type"
        );

        let body_bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert!(
            body_bytes.is_empty(),
            "204 responses should have an empty body for Unset"
        );
    }

    #[tokio::test]
    async fn json_value_with_complex_nested_structure_still_serializes_correctly() {
        let value = json!({
            "arr": [1, 2, 3],
            "obj": {
                "a": true,
                "b": null,
                "c": [ {"x": 1}, {"y": 2} ]
            }
        });

        let spec = ResponseBodySpec::JsonValue(value.clone());

        let resp = response_from_body_spec(spec);
        assert_eq!(resp.status().as_u16(), 200);

        let body_bytes = to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let decoded: serde_json::Value = serde_json::from_slice(&body_bytes)
            .expect("body should be valid JSON even when nested");

        assert_eq!(
            decoded, value,
            "complex nested JSON should survive serialization unchanged"
        );
    }
}
