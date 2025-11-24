//! Bridge between the internal Rust `ReqCtx` / `Recommendations`
//! and the JavaScript world (plugins & themes).

use crate::js::value::JsValue;
use crate::runtime::error::RuntimeError;
use http::{header, HeaderMap, HeaderValue, StatusCode};
use serde_json::{json, Map as JsonMap, Value as Json};
use serve::context::{RequestContext, ResponseBodySpec, ResponseSpec};
use serve::render::recommendation::{
    BodyPatch, BodyPatchKind, DomOp, HeaderPatch, HeaderPatchKind, ModelPatch, Recommendations,
};
use tracing::debug;

pub const CTX_SHIM_SRC: &str = r#"
(function (global) {
    function normalizeHeaderName(name) {
        return String(name).toLowerCase();
    }

    function wrapHeaders(rawHeaders) {
        // rawHeaders is a plain object with canonical keys from Rust,
        // but we build a lowercased index for lookups.
        const canonical = {};
        const lowerIndex = {};

        for (const [k, v] of Object.entries(rawHeaders || {})) {
            const canonKey = k;
            const lowerKey = normalizeHeaderName(k);
            canonical[canonKey] = v;
            lowerIndex[lowerKey] = v;
        }

        return {
            get(name) {
                const key = normalizeHeaderName(name);
                return lowerIndex[key];
            },
            has(name) {
                const key = normalizeHeaderName(name);
                return Object.prototype.hasOwnProperty.call(lowerIndex, key);
            },
            entries() {
                return Object.entries(canonical);
            },
            keys() {
                return Object.keys(canonical);
            },
            values() {
                return Object.values(canonical);
            },
            toJSON() {
                return { ...canonical };
            },
        };
    }

    function wrapCtx(ctx) {
        if (ctx && ctx.request && ctx.request.headers && !ctx.request.headers.__wrapped) {
            const raw = ctx.request.headers;
            ctx.request.headers = wrapHeaders(raw);
            // mark as wrapped to avoid double-wrapping if plugins chain ctx
            ctx.request.headers.__wrapped = true;
        }
        return ctx;
    }

    global.__wrapCtx = wrapCtx;
})(typeof globalThis !== 'undefined' ? globalThis : this);
"#;

/// Build the JS context object for plugins.
///
/// `plugin_id` is used to pick the correct per-plugin config from
/// `ReqCtx.plugin_configs`. The selected config is exposed to JS
/// as `ctx.config`.
#[tracing::instrument(skip_all)]
pub fn ctx_to_js_for_plugins(ctx: &RequestContext, plugin_id: &str) -> JsValue {
    let cfg = ctx.plugin_configs.get(plugin_id);
    ctx_to_js(ctx, cfg)
}

/// Build the JS context object for themes.
///
/// `theme_id` is not currently used to look up config (we only have a
/// single `theme_config` in `ReqCtx`), but is accepted for
/// symmetry with plugins. The theme config is exposed as `ctx.config`.
#[tracing::instrument(skip_all)]
pub fn ctx_to_js_for_theme(ctx: &RequestContext, theme_id: &str) -> JsValue {
    debug!("RequestContext for theme {}: {:?}", theme_id, ctx);
    let cfg = Some(&ctx.theme_config);
    ctx_to_js(ctx, cfg)
}

/// Merge JS result back into Rust context for plugins.
#[tracing::instrument(skip_all)]
pub fn merge_recommendations_from_js(
    ret: &JsValue,
    ctx: &mut RequestContext,
) -> Result<(), RuntimeError> {
    merge_from_js(ret, ctx)
}

/// Merge JS result back into Rust context for themes.
#[tracing::instrument(skip_all)]
pub fn merge_theme_ctx_from_js(
    ret: &JsValue,
    ctx: &mut RequestContext,
) -> Result<(), RuntimeError> {
    merge_from_js(ret, ctx)
}

// ─────────────────────────────────────────────────────────────────────────────
// Rust -> JS
// ─────────────────────────────────────────────────────────────────────────────

#[tracing::instrument(skip_all)]
fn ctx_to_js(ctx: &RequestContext, config: Option<&serde_json::Value>) -> JsValue {
    debug!("Config: {:?}", config);
    let mut root = JsonMap::new();

    // ---------------------------------------------------------------------
    // request
    // ---------------------------------------------------------------------
    let mut req_obj = JsonMap::new();

    // These are already Json values on ReqCtx; we just clone them
    // into the JS-facing shape.
    req_obj.insert("requestId".to_string(), ctx.req_id.clone());
    req_obj.insert("path".to_string(), ctx.req_path.clone());
    req_obj.insert("method".to_string(), ctx.req_method.clone());
    req_obj.insert("version".to_string(), ctx.req_version.clone());
    req_obj.insert("headers".to_string(), ctx.req_headers.clone());
    req_obj.insert("queryParams".to_string(), ctx.req_params.clone());

    root.insert("request".to_string(), Json::Object(req_obj));

    // ---------------------------------------------------------------------
    // response spec
    // ---------------------------------------------------------------------
    root.insert(
        "response".to_string(),
        response_spec_to_js(&ctx.response_spec),
    );

    // ---------------------------------------------------------------------
    // content: model + recommendations
    // ---------------------------------------------------------------------
    let mut content_obj = JsonMap::new();

    // For now we still expose an empty "model" placeholder here. When you
    // decide how to project `content_meta` into JS, wire it in.
    content_obj.insert("model".to_string(), Json::Object(JsonMap::new()));

    let recs = &ctx.recommendations;
    let mut recs_obj = JsonMap::new();

    let header_vals: Vec<Json> = recs.header_patches.iter().map(header_patch_to_js).collect();
    recs_obj.insert("headerPatches".to_string(), Json::Array(header_vals));

    let model_vals: Vec<Json> = recs.model_patches.iter().map(model_patch_to_js).collect();
    recs_obj.insert("modelPatches".to_string(), Json::Array(model_vals));

    let body_vals: Vec<Json> = recs.body_patches.iter().map(body_patch_to_js).collect();
    recs_obj.insert("bodyPatches".to_string(), Json::Array(body_vals));

    content_obj.insert("recommendations".to_string(), Json::Object(recs_obj));

    root.insert("content".to_string(), Json::Object(content_obj));

    // ---------------------------------------------------------------------
    // config for theme / plugin
    // ---------------------------------------------------------------------
    if let Some(cfg) = config {
        root.insert("config".to_string(), cfg.clone());
    } else {
        root.insert("config".to_string(), Json::Object(JsonMap::new()));
    }

    JsValue::from_json(&Json::Object(root))
}

#[tracing::instrument(skip_all)]
fn response_spec_to_js(spec: &ResponseSpec) -> Json {
    debug!("Response spec: {:?}", spec);
    let mut obj = JsonMap::new();

    obj.insert(
        "status".to_string(),
        Json::Number(spec.status.as_u16().into()),
    );

    // headers: string -> string
    let mut hdrs = JsonMap::new();
    for (name, value) in spec.headers.iter() {
        hdrs.insert(
            name.to_string(),
            Json::String(value.to_str().unwrap_or("").to_string()),
        );
    }
    obj.insert("headers".to_string(), Json::Object(hdrs));

    // body
    let body_json = match &spec.body {
        ResponseBodySpec::HtmlTemplate { template, model } => json!({
            "kind": "htmlTemplate",
            "template": template,
            "model": model,
        }),
        ResponseBodySpec::HtmlString(html) => json!({
            "kind": "htmlString",
            "html": html,
        }),
        ResponseBodySpec::JsonValue(value) => json!({
            "kind": "json",
            "value": value,
        }),
        ResponseBodySpec::None => json!({ "kind": "none" }),
        ResponseBodySpec::Unset => json!({ "kind": "unset" }),
    };

    obj.insert("body".to_string(), body_json);
    Json::Object(obj)
}

#[tracing::instrument(skip_all)]
fn header_patch_to_js(hp: &HeaderPatch) -> Json {
    let (kind_str, has_value) = match hp.kind {
        HeaderPatchKind::Set => ("set", true),
        HeaderPatchKind::Append => ("append", true),
        HeaderPatchKind::Remove => ("remove", false),
    };

    let mut obj = JsonMap::new();
    obj.insert("kind".to_string(), Json::String(kind_str.to_string()));
    obj.insert("name".to_string(), Json::String(hp.name.clone()));

    if has_value {
        if let Some(v) = &hp.value {
            obj.insert("value".to_string(), Json::String(v.clone()));
        }
    }

    obj.insert(
        "sourcePlugin".to_string(),
        Json::String(hp.source_plugin.clone()),
    );

    Json::Object(obj)
}

#[tracing::instrument(skip_all)]
fn model_patch_to_js(mp: &ModelPatch) -> Json {
    let mut obj = JsonMap::new();
    obj.insert("patch".to_string(), mp.patch.clone());
    obj.insert(
        "sourcePlugin".to_string(),
        Json::String(mp.source_plugin.clone()),
    );
    Json::Object(obj)
}

#[tracing::instrument(skip_all)]
fn dom_op_to_js(op: &DomOp) -> Json {
    match op {
        DomOp::SetInnerHtml(html) => json!({
            "kind": "setInnerHtml",
            "html": html,
        }),
        DomOp::PrependHtml(html) => json!({
            "kind": "prependHtml",
            "html": html,
        }),
        // All other variants: we still serialise a generic representation so
        // JS can at least inspect the kind.
        _ => json!({
            "kind": format!("{:?}", op),
        }),
    }
}

#[tracing::instrument(skip_all)]
fn body_patch_to_js(bp: &BodyPatch) -> Json {
    let mut obj = JsonMap::new();

    match &bp.kind {
        BodyPatchKind::Regex {
            pattern,
            replacement,
        } => {
            obj.insert("kind".to_string(), Json::String("regex".to_string()));
            obj.insert("pattern".to_string(), Json::String(pattern.clone()));
            obj.insert("replacement".to_string(), Json::String(replacement.clone()));
        }
        BodyPatchKind::HtmlDom { selector, ops } => {
            obj.insert("kind".to_string(), Json::String("htmlDom".to_string()));
            obj.insert("selector".to_string(), Json::String(selector.clone()));
            obj.insert(
                "ops".to_string(),
                Json::Array(ops.iter().map(dom_op_to_js).collect()),
            );
        }
        BodyPatchKind::JsonPatch { patch } => {
            obj.insert("kind".to_string(), Json::String("jsonPatch".to_string()));
            obj.insert("patch".to_string(), patch.clone());
        }
    }

    obj.insert(
        "sourcePlugin".to_string(),
        Json::String(bp.source_plugin.clone()),
    );

    Json::Object(obj)
}

// ─────────────────────────────────────────────────────────────────────────────
// JS -> Rust
// ─────────────────────────────────────────────────────────────────────────────

#[tracing::instrument(skip_all)]
fn parse_header_patch(v: &Json) -> Option<HeaderPatch> {
    let obj = v.as_object()?;
    let kind = obj.get("kind")?.as_str()?;
    let name = obj.get("name")?.as_str()?.to_string();
    let value = obj
        .get("value")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let source_plugin = obj
        .get("sourcePlugin")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let kind = match kind {
        "set" => HeaderPatchKind::Set,
        "append" => HeaderPatchKind::Append,
        "remove" => HeaderPatchKind::Remove,
        _ => return None,
    };

    if let Some(source_plugin) = source_plugin {
        Some(HeaderPatch {
            name,
            value,
            kind,
            source_plugin,
        })
    } else {
        None
    }
}

#[tracing::instrument(skip_all)]
fn parse_model_patch(v: &Json) -> Option<ModelPatch> {
    let obj = v.as_object()?;
    let patch = obj.get("patch")?.clone();
    let source_plugin = obj
        .get("sourcePlugin")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(source_plugin) = source_plugin {
        Some(ModelPatch {
            patch,
            source_plugin,
        })
    } else {
        None
    }
}

#[tracing::instrument(skip_all)]
fn parse_dom_op(v: &Json) -> Option<DomOp> {
    let obj = v.as_object()?;
    let kind = obj.get("kind")?.as_str()?;

    match kind {
        "setInnerHtml" => Some(DomOp::SetInnerHtml(obj.get("html")?.as_str()?.to_string())),
        "prependHtml" => Some(DomOp::PrependHtml(obj.get("html")?.as_str()?.to_string())),
        // For all other variants, we currently do not support round-tripping
        // from JS; they can be introduced later as needed.
        _ => None,
    }
}

#[tracing::instrument(skip_all)]
fn parse_body_patch(v: &Json) -> Option<BodyPatch> {
    let obj = v.as_object()?;
    let kind = obj.get("kind")?.as_str()?;
    let source_plugin = obj
        .get("sourcePlugin")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let kind = match kind {
        "regex" => BodyPatchKind::Regex {
            pattern: obj.get("pattern")?.as_str()?.to_string(),
            replacement: obj.get("replacement")?.as_str()?.to_string(),
        },
        "htmlDom" => {
            let selector = obj.get("selector")?.as_str()?.to_string();
            let ops_arr = obj.get("ops")?.as_array()?;
            let mut ops = Vec::new();
            for op_v in ops_arr {
                if let Some(op) = parse_dom_op(op_v) {
                    ops.push(op);
                }
            }
            BodyPatchKind::HtmlDom { selector, ops }
        }
        "jsonPatch" => BodyPatchKind::JsonPatch {
            patch: obj.get("patch")?.clone(),
        },
        _ => return None,
    };

    if let Some(source_plugin) = source_plugin {
        Some(BodyPatch {
            kind,
            source_plugin,
        })
    } else {
        None
    }
}

#[tracing::instrument(skip_all)]
fn parse_response_spec(v: &Json) -> Option<ResponseSpec> {
    let obj = v.as_object()?;

    let status = obj
        .get("status")
        .and_then(|s| s.as_u64())
        .and_then(|n| StatusCode::from_u16(n as u16).ok())
        .unwrap_or(StatusCode::OK);

    let mut headers = HeaderMap::new();
    if let Some(hdrs) = obj.get("headers").and_then(|h| h.as_object()) {
        for (name, val) in hdrs.iter() {
            if let Ok(header_name) = header::HeaderName::from_bytes(name.as_bytes()) {
                match val {
                    Json::String(s) => {
                        if let Ok(hv) = HeaderValue::from_str(s) {
                            headers.append(header_name.clone(), hv);
                        }
                    }
                    Json::Array(arr) => {
                        for v in arr {
                            if let Some(s) = v.as_str() {
                                if let Ok(hv) = HeaderValue::from_str(s) {
                                    headers.append(header_name.clone(), hv);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let body = if let Some(body_obj) = obj.get("body").and_then(|b| b.as_object()) {
        match body_obj.get("kind").and_then(|s| s.as_str()) {
            Some("none") | None => ResponseBodySpec::None,

            Some("json") => {
                let value = body_obj.get("value").cloned().unwrap_or(Json::Null);
                ResponseBodySpec::JsonValue(value)
            }

            Some("htmlTemplate") => {
                let template = body_obj
                    .get("template")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string();
                let model = body_obj.get("model").cloned().unwrap_or(Json::Null);
                ResponseBodySpec::HtmlTemplate { template, model }
            }

            // 🔧 NEW: handle htmlString from JS
            Some("htmlString") => {
                let html = body_obj
                    .get("html")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string();
                ResponseBodySpec::HtmlString(html)
            }

            _ => ResponseBodySpec::None,
        }
    } else {
        ResponseBodySpec::None
    };

    Some(ResponseSpec {
        status,
        headers,
        body,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Merge from JS into Rust
// ─────────────────────────────────────────────────────────────────────────────

#[tracing::instrument(skip_all)]
fn merge_from_js(ret: &JsValue, ctx: &mut RequestContext) -> Result<(), RuntimeError> {
    let json = ret.to_json();
    let obj = match json.as_object() {
        Some(o) => o,
        None => return Ok(()),
    };

    // recommendations
    if let Some(recs_val) = obj.get("recommendations") {
        if let Some(recs_obj) = recs_val.as_object() {
            let mut new_recs = Recommendations::default();

            if let Some(hp_arr) = recs_obj.get("headerPatches").and_then(|v| v.as_array()) {
                for hp_v in hp_arr {
                    if let Some(hp) = parse_header_patch(hp_v) {
                        new_recs.header_patches.push(hp);
                    }
                }
            }

            if let Some(mp_arr) = recs_obj.get("modelPatches").and_then(|v| v.as_array()) {
                for mp_v in mp_arr {
                    if let Some(mp) = parse_model_patch(mp_v) {
                        new_recs.model_patches.push(mp);
                    }
                }
            }

            if let Some(bp_arr) = recs_obj.get("bodyPatches").and_then(|v| v.as_array()) {
                for bp_v in bp_arr {
                    if let Some(bp) = parse_body_patch(bp_v) {
                        new_recs.body_patches.push(bp);
                    }
                }
            }

            ctx.recommendations
                .header_patches
                .extend(new_recs.header_patches.into_iter());
            ctx.recommendations
                .model_patches
                .extend(new_recs.model_patches.into_iter());
            ctx.recommendations
                .body_patches
                .extend(new_recs.body_patches.into_iter());
        }
    }

    // response override
    if let Some(resp_val) = obj.get("response") {
        if let Some(spec) = parse_response_spec(resp_val) {
            ctx.response_spec = spec;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serve::context::RequestContext;
    use std::collections::HashMap;

    fn make_base_ctx() -> RequestContext {
        RequestContext::builder()
            .path("/test")
            .method("GET")
            .version("HTTP/1.1")
            .headers(json!({}))
            .params(json!({}))
            .content_meta(json!({ "title": "Test" }))
            .theme_config(json!({ "color": "blue" }))
            .plugin_configs(HashMap::new())
            // No req_body / content_body streams in this base test context.
            .build()
    }

    #[test]
    fn ctx_to_js_for_plugins_uses_plugin_specific_config() {
        let mut ctx = make_base_ctx();
        ctx.plugin_configs.insert(
            "plugin-a".to_string(),
            json!({"enabled": true, "threshold": 5}),
        );
        ctx.plugin_configs
            .insert("plugin-b".to_string(), json!({"enabled": false}));

        let js = ctx_to_js_for_plugins(&ctx, "plugin-a");
        let json = js.to_json();

        let config = json
            .get("config")
            .expect("config missing")
            .as_object()
            .expect("config not object");

        assert_eq!(config.get("enabled").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(config.get("threshold").and_then(|v| v.as_i64()), Some(5));
    }

    #[test]
    fn ctx_to_js_for_plugins_missing_config_yields_empty_object() {
        let ctx = make_base_ctx();

        let js = ctx_to_js_for_plugins(&ctx, "missing-plugin");
        let json = js.to_json();

        let config = json
            .get("config")
            .expect("config missing")
            .as_object()
            .expect("config not object");
        assert!(config.is_empty(), "expected empty config object");
    }

    #[test]
    fn ctx_to_js_for_theme_uses_theme_config() {
        let mut ctx = make_base_ctx();

        ctx.theme_config = json!({"themeName": "MyTheme", "darkMode": true});

        let js = ctx_to_js_for_theme(&ctx, "any-theme-id");
        let json = js.to_json();

        let config = json
            .get("config")
            .expect("config missing")
            .as_object()
            .expect("config not object");

        assert_eq!(
            config.get("themeName").and_then(|v| v.as_str()),
            Some("MyTheme")
        );
        assert_eq!(config.get("darkMode").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn merge_from_js_appends_recommendations_and_overrides_response() {
        let mut ctx = make_base_ctx();

        // Seed existing recommendations to ensure we append.
        ctx.recommendations.header_patches.push(HeaderPatch {
            kind: HeaderPatchKind::Set,
            name: "x-existing".into(),
            value: Some("1".into()),
            source_plugin: "existing-plugin".into(),
        });

        let js_json = json!({
            "recommendations": {
                "headerPatches": [
                    {
                        "kind": "set",
                        "name": "x-test",
                        "value": "ok",
                        "sourcePlugin": "p1"
                    }
                ],
                "modelPatches": [
                    {
                        "patch": { "op": "add", "path": "/foo", "value": 42 },
                        "sourcePlugin": "p1"
                    }
                ],
                "bodyPatches": [
                    {
                        "kind": "regex",
                        "pattern": "foo",
                        "replacement": "bar",
                        "sourcePlugin": "p1"
                    }
                ]
            },
            "response": {
                "status": 201u16,
                "headers": {
                    "content-type": "text/html"
                },
                "body": {
                    "kind": "json",
                    "value": { "ok": true }
                }
            }
        });

        let js = JsValue::from_json(&js_json);

        merge_from_js(&js, &mut ctx).expect("merge_from_js failed");

        // Header patches appended
        assert_eq!(ctx.recommendations.header_patches.len(), 2);
        assert!(ctx
            .recommendations
            .header_patches
            .iter()
            .any(|h| h.name == "x-test" && h.value.as_deref() == Some("ok")));

        // Model patches present
        assert_eq!(ctx.recommendations.model_patches.len(), 1);
        assert_eq!(
            ctx.recommendations.model_patches[0].source_plugin,
            "p1".to_string()
        );

        // Body patches present
        assert_eq!(ctx.recommendations.body_patches.len(), 1);
        matches!(
            ctx.recommendations.body_patches[0].kind,
            BodyPatchKind::Regex { .. }
        );

        // Response overridden
        assert_eq!(ctx.response_spec.status, StatusCode::CREATED);
        let ct = ctx
            .response_spec
            .headers
            .get("content-type")
            .and_then(|h| h.to_str().ok());
        assert_eq!(ct, Some("text/html"));
        match &ctx.response_spec.body {
            ResponseBodySpec::JsonValue(v) => {
                assert_eq!(v.get("ok").and_then(|v| v.as_bool()), Some(true));
            }
            other => panic!("expected JsonValue body, got {:?}", other),
        }
    }

    #[test]
    fn merge_from_js_ignores_non_object_root() {
        let mut ctx = make_base_ctx();
        let js = JsValue::from_json(&json!(42));

        merge_from_js(&js, &mut ctx).expect("merge_from_js should not fail");

        // No recommendations or response changes
        assert!(ctx.recommendations.header_patches.is_empty());
        assert!(ctx.recommendations.model_patches.is_empty());
        assert!(ctx.recommendations.body_patches.is_empty());
    }

    #[test]
    fn parse_response_spec_handles_invalid_status_and_headers_gracefully() {
        let js = json!({
            "status": "not-a-number",
            "headers": {
                // invalid header name, invalid value types
                "Bad Header": 123,
                "x-ok": ["a", 42, "b"]
            },
            "body": {
                "kind": "json",
                "value": { "x": 1 }
            }
        });

        let spec = parse_response_spec(&js).expect("parse_response_spec must return Some");
        // Falls back to 200 OK
        assert_eq!(spec.status, StatusCode::OK);

        // Only valid string values are included
        let hv: Vec<_> = spec
            .headers
            .get_all("x-ok")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        assert_eq!(hv, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn parse_body_patch_rejects_missing_source_plugin() {
        let v = json!({
            "kind": "regex",
            "pattern": "a",
            "replacement": "b"
            // no sourcePlugin
        });

        assert!(parse_body_patch(&v).is_none());
    }

    #[test]
    fn parse_header_patch_rejects_unknown_kind() {
        let v = json!({
            "kind": "unknown",
            "name": "x",
            "value": "y",
            "sourcePlugin": "p"
        });

        assert!(parse_header_patch(&v).is_none());
    }

    #[test]
    fn ctx_to_js_includes_request_and_response_shapes() {
        let mut ctx = make_base_ctx();

        ctx.response_spec.status = StatusCode::ACCEPTED;
        ctx.response_spec
            .headers
            .insert("x-test", HeaderValue::from_static("ok"));

        let js = ctx_to_js_for_theme(&ctx, "theme-id");
        let json = js.to_json();

        let req = json
            .get("request")
            .and_then(|v| v.as_object())
            .expect("request missing");
        assert_eq!(req.get("path").and_then(|v| v.as_str()), Some("/test"));
        assert_eq!(req.get("method").and_then(|v| v.as_str()), Some("GET"));

        let resp = json
            .get("response")
            .and_then(|v| v.as_object())
            .expect("response missing");
        assert_eq!(
            resp.get("status").and_then(|v| v.as_u64()),
            Some(StatusCode::ACCEPTED.as_u16() as u64)
        );
        let hdrs = resp
            .get("headers")
            .and_then(|v| v.as_object())
            .expect("headers missing");
        assert_eq!(hdrs.get("x-test").and_then(|v| v.as_str()), Some("ok"));
    }
}
