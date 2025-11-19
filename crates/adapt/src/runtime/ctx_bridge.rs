//! Bridge between the internal Rust `RequestContext` / `Recommendations`
//! and the JavaScript world (plugins & themes).

use crate::core::context::{RequestContext, ResponseBodySpec, ResponseSpec};
use crate::core::recommendation::{
    BodyPatch, BodyPatchKind, DomOp, HeaderPatch, HeaderPatchKind, ModelPatch, Recommendations,
};
use crate::js::value::JsValue;
use crate::runtime::error::RuntimeError;
use http::{header, HeaderMap, HeaderValue, StatusCode};
use serde_json::{json, Map as JsonMap, Value as Json};

/// Build the JS context object for plugins.
///
/// `plugin_id` is used to pick the correct per-plugin config from
/// `RequestContext.plugin_configs`. The selected config is exposed to JS
/// as `ctx.config`.
pub fn ctx_to_js_for_plugins(ctx: &RequestContext, plugin_id: &str) -> JsValue {
    let cfg = ctx.plugin_configs.get(plugin_id);
    ctx_to_js(ctx, cfg)
}

/// Build the JS context object for themes.
///
/// `theme_id` is not currently used to look up config (we only have a
/// single `theme_config` in `RequestContext`), but is accepted for
/// symmetry with plugins. The theme config is exposed as `ctx.config`.
pub fn ctx_to_js_for_theme(ctx: &RequestContext, _theme_id: &str) -> JsValue {
    let cfg = Some(&ctx.theme_config);
    ctx_to_js(ctx, cfg)
}

/// Merge JS result back into Rust context for plugins.
pub fn merge_recommendations_from_js(
    ret: &JsValue,
    ctx: &mut RequestContext,
) -> Result<(), RuntimeError> {
    merge_from_js(ret, ctx)
}

/// Merge JS result back into Rust context for themes.
pub fn merge_theme_ctx_from_js(
    ret: &JsValue,
    ctx: &mut RequestContext,
) -> Result<(), RuntimeError> {
    merge_from_js(ret, ctx)
}

// ─────────────────────────────────────────────────────────────────────────────
// Rust -> JS
// ─────────────────────────────────────────────────────────────────────────────

fn ctx_to_js(ctx: &RequestContext, config: Option<&serde_json::Value>) -> JsValue {
    let mut root = JsonMap::new();

    // request
    let mut req_obj = JsonMap::new();
    req_obj.insert(
        "requestId".to_string(),
        Json::String(ctx.request_id.to_string()),
    );
    req_obj.insert("path".to_string(), Json::String(ctx.path.clone()));
    req_obj.insert("method".to_string(), Json::String(ctx.method.to_string()));

    // request headers: string -> string
    let mut headers_obj = JsonMap::new();
    for (name, value) in ctx.headers.iter() {
        headers_obj.insert(
            name.to_string(),
            Json::String(value.to_str().unwrap_or("").to_string()),
        );
    }
    req_obj.insert("headers".to_string(), Json::Object(headers_obj));

    // query params: string -> string
    let mut qp_obj = JsonMap::new();
    for (k, v) in ctx.query_params.iter() {
        qp_obj.insert(k.clone(), Json::String(v.clone()));
    }
    req_obj.insert("queryParams".to_string(), Json::Object(qp_obj));

    root.insert("request".to_string(), Json::Object(req_obj));

    // response spec
    root.insert(
        "response".to_string(),
        response_spec_to_js(&ctx.response_spec),
    );

    // content: (model + recommendations)
    let mut content_obj = JsonMap::new();

    // NOTE: if/when RequestContext has a real `model`, wire it here.
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

    // config for theme / plugin
    if let Some(cfg) = config {
        root.insert("config".to_string(), cfg.clone());
    } else {
        root.insert("config".to_string(), Json::Object(JsonMap::new()));
    }

    JsValue::from_json(&Json::Object(root))
}

fn response_spec_to_js(spec: &ResponseSpec) -> Json {
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

fn model_patch_to_js(mp: &ModelPatch) -> Json {
    let mut obj = JsonMap::new();
    obj.insert("patch".to_string(), mp.patch.clone());
    obj.insert(
        "sourcePlugin".to_string(),
        Json::String(mp.source_plugin.clone()),
    );
    Json::Object(obj)
}

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

fn parse_response_spec(v: &Json) -> Option<ResponseSpec> {
    let obj = v.as_object()?;

    let status = obj
        .get("status")
        .and_then(|s| s.as_u64())
        .and_then(|n| StatusCode::from_u16(n as u16).ok())
        .unwrap_or(StatusCode::OK);

    // headers: string -> string
    let mut headers = HeaderMap::new();
    if let Some(hdrs) = obj.get("headers").and_then(|h| h.as_object()) {
        for (name, val) in hdrs.iter() {
            if let Ok(header_name) = header::HeaderName::from_bytes(name.as_bytes()) {
                // accept either a string or an array of strings
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

    // body
    let body = if let Some(body_obj) = obj.get("body").and_then(|b| b.as_object()) {
        match body_obj.get("kind").and_then(|s| s.as_str()) {
            Some("none") | None => ResponseBodySpec::None,
            Some("json") => {
                let value = body_obj.get("value").cloned().unwrap_or_else(|| Json::Null);
                ResponseBodySpec::JsonValue(value)
            }
            Some("htmlTemplate") => {
                let template = body_obj
                    .get("template")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string();
                let model = body_obj.get("model").cloned().unwrap_or_else(|| Json::Null);
                ResponseBodySpec::HtmlTemplate { template, model }
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
    use crate::core::content::ContentKind;
    use crate::core::context::ResponseBodySpec;
    use crate::core::recommendation::{
        BodyPatchKind, DomOp, HeaderPatchKind, ModelPatch, Recommendations,
    };
    use http::{HeaderMap, Method};
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;

    // ─────────────────────────────────────────────────────────────
    // Helpers
    // ─────────────────────────────────────────────────────────────

    fn base_ctx() -> RequestContext {
        RequestContext::new(
            "/test".to_string(),
            Method::GET,
            HeaderMap::new(),
            HashMap::new(),
            ContentKind::Html,
            json!({"title": "doc"}),
            PathBuf::from("content/test.html"),
            json!({}),
            HashMap::new(),
        )
    }

    fn _ctx_with_response(body: ResponseBodySpec) -> RequestContext {
        let mut ctx = base_ctx();
        ctx.response_spec.body = body;
        ctx
    }

    fn ctx_with_recommendations() -> RequestContext {
        use crate::core::recommendation::{BodyPatch, HeaderPatch};

        let mut ctx = base_ctx();

        ctx.recommendations = Recommendations {
            header_patches: vec![HeaderPatch::set(
                "x-test".into(),
                "1".into(),
                "plugin-a".into(),
            )],
            model_patches: vec![ModelPatch {
                patch: json!([{ "op": "add", "path": "/foo", "value": 42 }]),
                source_plugin: "plugin-a".into(),
            }],
            body_patches: vec![BodyPatch {
                kind: BodyPatchKind::Regex {
                    pattern: "foo".into(),
                    replacement: "bar".into(),
                },
                source_plugin: "plugin-a".into(),
            }],
        };

        ctx
    }

    // ─────────────────────────────────────────────────────────────
    // ctx_to_js_* (Rust -> JS)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn ctx_to_js_for_plugins_basic_shape() {
        let ctx = base_ctx();
        let js = ctx_to_js_for_plugins(&ctx, "test-plugin");
        let json = js.to_json();

        let obj = json.as_object().expect("root should be object");

        // request basics
        let req = obj.get("request").and_then(|v| v.as_object()).unwrap();
        assert_eq!(req.get("path").unwrap().as_str().unwrap(), "/test");
        assert_eq!(req.get("method").unwrap().as_str().unwrap(), "GET");
        assert!(req.get("requestId").is_some());

        // headers and query params objects exist
        assert!(req.get("headers").unwrap().is_object());
        assert!(req.get("queryParams").unwrap().is_object());

        // response present
        let resp = obj.get("response").unwrap().as_object().unwrap();
        assert_eq!(resp.get("status").unwrap().as_u64().unwrap(), 200);

        // body kind = "unset" by default
        let body = resp.get("body").unwrap().as_object().unwrap();
        assert_eq!(body.get("kind").unwrap().as_str().unwrap(), "unset");

        // content / model / recommendations present
        let content = obj.get("content").unwrap().as_object().unwrap();
        let model = content.get("model").unwrap().as_object().unwrap();
        assert!(model.is_empty(), "default model should be empty object");
        let recs = content.get("recommendations").unwrap().as_object().unwrap();
        assert!(recs
            .get("headerPatches")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        assert!(recs
            .get("modelPatches")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
        assert!(recs
            .get("bodyPatches")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());

        // config default
        assert!(obj.get("config").unwrap().as_object().unwrap().is_empty());
    }

    #[test]
    fn ctx_to_js_includes_headers_and_query_params() {
        let mut headers = HeaderMap::new();
        headers.insert("x-test", HeaderValue::from_static("abc"));

        let mut query = HashMap::new();
        query.insert("q".to_string(), "rust".to_string());

        let ctx = RequestContext::new(
            "/search".to_string(),
            Method::GET,
            headers,
            query,
            ContentKind::Html,
            json!({}),
            PathBuf::from("content/search.html"),
            json!({}),
            HashMap::new(),
        );

        let js = ctx_to_js_for_plugins(&ctx, "test-plugin");
        let json = js.to_json();
        let obj = json.as_object().unwrap();
        let req = obj.get("request").unwrap().as_object().unwrap();

        let hdrs = req.get("headers").unwrap().as_object().unwrap();
        assert_eq!(hdrs.get("x-test").unwrap().as_str().unwrap(), "abc");

        let qp = req.get("queryParams").unwrap().as_object().unwrap();
        assert_eq!(qp.get("q").unwrap().as_str().unwrap(), "rust");
    }

    #[test]
    fn response_spec_to_js_handles_all_body_variants() {
        // HtmlTemplate
        let mut spec = ResponseSpec {
            status: StatusCode::CREATED,
            headers: HeaderMap::new(),
            body: ResponseBodySpec::HtmlTemplate {
                template: "t1".into(),
                model: json!({"x": 1}),
            },
        };
        let j = response_spec_to_js(&spec);
        let o = j.as_object().unwrap();
        assert_eq!(o.get("status").unwrap().as_u64().unwrap(), 201);
        let body = o.get("body").unwrap().as_object().unwrap();
        assert_eq!(body.get("kind").unwrap().as_str().unwrap(), "htmlTemplate");
        assert_eq!(body.get("template").unwrap().as_str().unwrap(), "t1");

        // HtmlString
        spec.body = ResponseBodySpec::HtmlString("<p>hi</p>".into());
        let j = response_spec_to_js(&spec);
        let body = j
            .as_object()
            .unwrap()
            .get("body")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(body.get("kind").unwrap().as_str().unwrap(), "htmlString");
        assert_eq!(body.get("html").unwrap().as_str().unwrap(), "<p>hi</p>");

        // JsonValue
        spec.body = ResponseBodySpec::JsonValue(json!({"ok": true}));
        let j = response_spec_to_js(&spec);
        let body = j
            .as_object()
            .unwrap()
            .get("body")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(body.get("kind").unwrap().as_str().unwrap(), "json");
        assert_eq!(body.get("value").unwrap()["ok"], json!(true));

        // None
        spec.body = ResponseBodySpec::None;
        let j = response_spec_to_js(&spec);
        let body = j
            .as_object()
            .unwrap()
            .get("body")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(body.get("kind").unwrap().as_str().unwrap(), "none");

        // Unset
        spec.body = ResponseBodySpec::Unset;
        let j = response_spec_to_js(&spec);
        let body = j
            .as_object()
            .unwrap()
            .get("body")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(body.get("kind").unwrap().as_str().unwrap(), "unset");
    }

    // ─────────────────────────────────────────────────────────────
    // HeaderPatch / ModelPatch / BodyPatch (Rust -> JS -> Rust)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn header_patch_roundtrip_to_js_and_back() {
        let hp = HeaderPatch {
            kind: HeaderPatchKind::Append,
            name: "x-test".into(),
            value: Some("v".into()),
            source_plugin: "plugin-a".into(),
        };

        let j = header_patch_to_js(&hp);
        let parsed = parse_header_patch(&j).expect("should parse back");

        assert_eq!(parsed.name, "x-test");
        assert_eq!(parsed.value.as_deref(), Some("v"));
        assert_eq!(parsed.source_plugin, "plugin-a");
        assert_eq!(parsed.kind, HeaderPatchKind::Append);
    }

    #[test]
    fn parse_header_patch_rejects_missing_source_plugin() {
        let j = json!({
            "kind": "set",
            "name": "x-test",
            "value": "v"
        });

        assert!(parse_header_patch(&j).is_none());
    }

    #[test]
    fn model_patch_roundtrip_to_js_and_back() {
        let mp = ModelPatch {
            patch: json!([{ "op": "add", "path": "/x", "value": 1 }]),
            source_plugin: "plugin-a".into(),
        };

        let j = model_patch_to_js(&mp);
        let parsed = parse_model_patch(&j).expect("should parse back");

        assert_eq!(parsed.patch, mp.patch);
        assert_eq!(parsed.source_plugin, "plugin-a");
    }

    #[test]
    fn parse_model_patch_rejects_missing_source_plugin() {
        let j = json!({
            "patch": [{ "op": "add", "path": "/x", "value": 1 }]
        });

        assert!(parse_model_patch(&j).is_none());
    }

    #[test]
    fn body_patch_regex_roundtrip() {
        use crate::core::recommendation::BodyPatch;

        let bp = BodyPatch {
            kind: BodyPatchKind::Regex {
                pattern: "foo".into(),
                replacement: "bar".into(),
            },
            source_plugin: "plugin-a".into(),
        };

        let j = body_patch_to_js(&bp);
        let parsed = parse_body_patch(&j).expect("should parse");

        match parsed.kind {
            BodyPatchKind::Regex {
                pattern,
                replacement,
            } => {
                assert_eq!(pattern, "foo");
                assert_eq!(replacement, "bar");
            }
            _ => panic!("expected regex body patch"),
        }
        assert_eq!(parsed.source_plugin, "plugin-a");
    }

    #[test]
    fn body_patch_html_dom_roundtrip_for_supported_ops() {
        use crate::core::recommendation::BodyPatch;

        let bp = BodyPatch {
            kind: BodyPatchKind::HtmlDom {
                selector: "p".into(),
                ops: vec![
                    DomOp::SetInnerHtml("<b>hi</b>".into()),
                    DomOp::PrependHtml("<span>prefix</span>".into()),
                ],
            },
            source_plugin: "plugin-b".into(),
        };

        let j = body_patch_to_js(&bp);
        let parsed = parse_body_patch(&j).expect("should parse");

        match parsed.kind {
            BodyPatchKind::HtmlDom { selector, ops } => {
                assert_eq!(selector, "p");
                // Only the supported ops should roundtrip
                assert_eq!(ops.len(), 2);
                match &ops[0] {
                    DomOp::SetInnerHtml(html) => assert_eq!(html, "<b>hi</b>"),
                    _ => panic!("expected SetInnerHtml"),
                }
                match &ops[1] {
                    DomOp::PrependHtml(html) => assert_eq!(html, "<span>prefix</span>"),
                    _ => panic!("expected PrependHtml"),
                }
            }
            _ => panic!("expected HtmlDom body patch"),
        }
        assert_eq!(parsed.source_plugin, "plugin-b");
    }

    #[test]
    fn body_patch_html_dom_drops_unsupported_ops() {
        use crate::core::recommendation::BodyPatch;

        let bp = BodyPatch {
            kind: BodyPatchKind::HtmlDom {
                selector: "div".into(),
                ops: vec![DomOp::Remove], // not supported by parse_dom_op
            },
            source_plugin: "plugin-c".into(),
        };

        let j = body_patch_to_js(&bp);
        let parsed = parse_body_patch(&j).expect("should still parse HtmlDom");

        match parsed.kind {
            BodyPatchKind::HtmlDom { selector, ops } => {
                assert_eq!(selector, "div");
                assert!(
                    ops.is_empty(),
                    "unsupported ops should be dropped during parse_dom_op"
                );
            }
            _ => panic!("expected HtmlDom body patch"),
        }
    }

    #[test]
    fn body_patch_json_patch_roundtrip() {
        use crate::core::recommendation::BodyPatch;

        let patch_json = json!([{ "op": "replace", "path": "/x", "value": 2 }]);

        let bp = BodyPatch {
            kind: BodyPatchKind::JsonPatch {
                patch: patch_json.clone(),
            },
            source_plugin: "plugin-d".into(),
        };

        let j = body_patch_to_js(&bp);
        let parsed = parse_body_patch(&j).expect("should parse jsonPatch");

        match parsed.kind {
            BodyPatchKind::JsonPatch { patch } => {
                assert_eq!(patch, patch_json);
            }
            _ => panic!("expected JsonPatch body patch"),
        }
        assert_eq!(parsed.source_plugin, "plugin-d");
    }

    // ─────────────────────────────────────────────────────────────
    // parse_response_spec
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_response_spec_defaults_for_missing_fields() {
        let j = json!({});
        let spec = parse_response_spec(&j).expect("should construct default spec");

        assert_eq!(spec.status, StatusCode::OK);
        assert!(spec.headers.is_empty());
        match spec.body {
            ResponseBodySpec::None => {} // default in parser
            _ => panic!("expected ResponseBodySpec::None"),
        }
    }

    #[test]
    fn parse_response_spec_handles_status_headers_and_json_body() {
        let j = json!({
            "status": 201,
            "headers": {
                "x-a": "one",
                "x-b": ["two", "three"]
            },
            "body": {
                "kind": "json",
                "value": {"ok": true}
            }
        });

        let spec = parse_response_spec(&j).expect("parse_response_spec should succeed");

        assert_eq!(spec.status, StatusCode::CREATED);

        let mut vals = Vec::new();
        for v in spec.headers.get_all("x-b").iter() {
            vals.push(v.to_str().unwrap().to_string());
        }
        assert_eq!(spec.headers.get("x-a").unwrap(), "one");
        assert_eq!(vals, vec!["two".to_string(), "three".to_string()]);

        match spec.body {
            ResponseBodySpec::JsonValue(v) => {
                assert_eq!(v["ok"], json!(true));
            }
            _ => panic!("expected JsonValue"),
        }
    }

    #[test]
    fn parse_response_spec_treats_unknown_kind_as_none() {
        let j = json!({
            "body": { "kind": "something-else" }
        });

        let spec = parse_response_spec(&j).expect("should succeed");
        match spec.body {
            ResponseBodySpec::None => {}
            _ => panic!("unknown body kind should map to None"),
        }
    }

    // ─────────────────────────────────────────────────────────────
    // merge_from_js via public helpers
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn merge_recommendations_from_js_merges_new_recommendations_and_response() {
        let mut ctx = ctx_with_recommendations();

        // Start with something non-default so we can see overrides.
        ctx.response_spec.status = StatusCode::OK;

        let js_ret = json!({
            "recommendations": {
                "headerPatches": [
                    {
                        "kind": "set",
                        "name": "x-merged",
                        "value": "v",
                        "sourcePlugin": "plugin-x"
                    }
                ],
                "modelPatches": [
                    {
                        "patch": [{ "op": "add", "path": "/y", "value": 99 }],
                        "sourcePlugin": "plugin-y"
                    }
                ],
                "bodyPatches": [
                    {
                        "kind": "regex",
                        "pattern": "foo",
                        "replacement": "baz",
                        "sourcePlugin": "plugin-z"
                    }
                ]
            },
            "response": {
                "status": 404,
                "headers": {
                    "x-from-js": "yes"
                },
                "body": {
                    "kind": "json",
                    "value": { "fromJs": true }
                }
            }
        });

        let js_val = JsValue::from_json(&js_ret);

        let result = merge_recommendations_from_js(&js_val, &mut ctx);
        assert!(result.is_ok());

        // All existing recs plus the new ones
        assert!(
            ctx.recommendations.header_patches.len() >= 2,
            "should append header patches"
        );
        assert!(
            ctx.recommendations.model_patches.len() >= 2,
            "should append model patches"
        );
        assert!(
            ctx.recommendations.body_patches.len() >= 2,
            "should append body patches"
        );

        // Response overridden
        assert_eq!(ctx.response_spec.status, StatusCode::NOT_FOUND);
        assert_eq!(
            ctx.response_spec
                .headers
                .get("x-from-js")
                .unwrap()
                .to_str()
                .unwrap(),
            "yes"
        );
        match &ctx.response_spec.body {
            ResponseBodySpec::JsonValue(v) => {
                assert_eq!(v["fromJs"], json!(true));
            }
            _ => panic!("expected JsonValue from JS override"),
        }
    }

    #[test]
    fn merge_recommendations_from_js_is_noop_for_non_object() {
        let mut ctx = ctx_with_recommendations();
        let before = ctx.recommendations.header_patches.len();

        let js_val = JsValue::Null;
        let result = merge_recommendations_from_js(&js_val, &mut ctx);
        assert!(result.is_ok());
        assert_eq!(
            ctx.recommendations.header_patches.len(),
            before,
            "no change expected when ret is not object"
        );
    }

    #[test]
    fn merge_recommendations_skips_malformed_entries() {
        let mut ctx = base_ctx();

        let js_ret = json!({
            "recommendations": {
                "headerPatches": [
                    { "kind": "set", "name": "x-ok", "value": "1", "sourcePlugin": "p" },
                    { "kind": "set", "name": "x-bad", "value": "2" } // missing sourcePlugin
                ],
                "modelPatches": [
                    { "patch": [], "sourcePlugin": "p" },
                    { "patch": [] } // missing sourcePlugin
                ],
                "bodyPatches": [
                    {
                        "kind": "regex",
                        "pattern": "a",
                        "replacement": "b",
                        "sourcePlugin": "p"
                    },
                    {
                        "kind": "regex",
                        "pattern": "a",
                        "replacement": "b"
                        // missing sourcePlugin
                    }
                ]
            }
        });

        let js_val = JsValue::from_json(&js_ret);
        merge_recommendations_from_js(&js_val, &mut ctx).expect("merge should succeed");

        assert_eq!(ctx.recommendations.header_patches.len(), 1);
        assert_eq!(ctx.recommendations.model_patches.len(), 1);
        assert_eq!(ctx.recommendations.body_patches.len(), 1);
    }

    #[test]
    fn merge_theme_ctx_from_js_behaves_same_as_merge_recommendations_from_js() {
        let mut ctx_a = base_ctx();
        let mut ctx_b = base_ctx();

        let js_ret = json!({
            "response": { "status": 500 }
        });
        let js_val = JsValue::from_json(&js_ret);

        merge_recommendations_from_js(&js_val, &mut ctx_a).unwrap();
        merge_theme_ctx_from_js(&js_val, &mut ctx_b).unwrap();

        assert_eq!(ctx_a.response_spec.status, ctx_b.response_spec.status);
    }

    #[test]
    fn ctx_to_js_for_theme_matches_plugins_view_of_request() {
        let ctx = base_ctx();

        let js_theme = ctx_to_js_for_theme(&ctx, "test-theme").to_json();
        let js_plugins = ctx_to_js_for_plugins(&ctx, "test-plugin").to_json();

        assert_eq!(js_theme["request"]["path"], js_plugins["request"]["path"]);
        assert_eq!(
            js_theme["request"]["method"],
            js_plugins["request"]["method"]
        );
    }
}
