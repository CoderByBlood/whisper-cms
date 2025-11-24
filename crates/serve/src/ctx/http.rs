// crates/serve/src/ctx/http.rs

use crate::render::recommendation::Recommendations;
use http::{HeaderMap, HeaderValue, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use thiserror::Error;
use tracing::debug;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ContextError {
    #[error("invalid header value: {0}")]
    InvalidHeaderValue(String),

    #[error("json patch error: {0}")]
    JsonPatch(#[from] json_patch::PatchError),

    #[error("other core error: {0}")]
    Other(String),
}

impl ContextError {
    pub fn to_status(&self) -> StatusCode {
        match self {
            ContextError::InvalidHeaderValue(_) => StatusCode::BAD_REQUEST,
            ContextError::JsonPatch(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ContextError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// StreamHandle API (opaque, fn-pointer based)
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStatus {
    Ok(usize),
    Eof,
    WouldBlock,
    Err(u32),
}

/// A stable, opaque ID for a registered stream.
///
/// This is trivially `Send + Sync` and `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamId(u64);

impl StreamId {
    pub fn new(raw: u64) -> Self {
        StreamId(raw)
    }
}

static STREAM_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn next_stream_id() -> StreamId {
    StreamId(STREAM_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub type StreamReadFn = fn(id: StreamId, dst: &mut [u8]) -> StreamStatus;
pub type StreamCloseFn = fn(id: StreamId);

#[derive(Debug, Clone, Copy)]
pub struct StreamVTable {
    pub read: StreamReadFn,
    pub close: Option<StreamCloseFn>,
}

/// Opaque handle that JS sees. No trait objects, no raw pointers.
#[derive(Debug, Clone, Copy)]
pub struct StreamHandle {
    pub id: StreamId,
    pub vtable: StreamVTable,
}

impl StreamHandle {
    #[inline]
    pub fn read(&self, dst: &mut [u8]) -> StreamStatus {
        (self.vtable.read)(self.id, dst)
    }

    #[inline]
    pub fn close(&self) {
        if let Some(close) = self.vtable.close {
            close(self.id);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Serde helpers for foreign types (HeaderMap, StatusCode)
// ─────────────────────────────────────────────────────────────────────────────

mod serde_headermap {
    use super::*;
    use http::header::HeaderName;
    use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S>(map: &HeaderMap<HeaderValue>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut tmp = HashMap::<String, String>::new();

        for (name, value) in map.iter() {
            if let Ok(v) = value.to_str() {
                tmp.insert(name.as_str().to_string(), v.to_string());
            }
        }

        tmp.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HeaderMap<HeaderValue>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let tmp = HashMap::<String, String>::deserialize(deserializer)?;
        let mut headers: HeaderMap<HeaderValue> = HeaderMap::new();

        for (k, v) in tmp {
            let name: HeaderName = k.parse().map_err(D::Error::custom)?;
            let value: HeaderValue = v.parse().map_err(D::Error::custom)?;
            headers.insert(name, value);
        }

        Ok(headers)
    }
}

mod serde_status {
    use super::*;
    use serde::{de::Error as DeError, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(code: &StatusCode, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u16(code.as_u16())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<StatusCode, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = u16::deserialize(deserializer)?;
        StatusCode::from_u16(v).map_err(D::Error::custom)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Response specification (what the theme wants to send back)
// ─────────────────────────────────────────────────────────────────────────────

/// Describes what the theme wants the host to send as an HTTP response.
///
/// The theme manipulates this indirectly via ctx.response.* in JS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSpec {
    #[serde(with = "serde_status")]
    pub status: StatusCode,

    #[serde(with = "serde_headermap")]
    pub headers: HeaderMap,

    pub body: ResponseBodySpec,
}

impl Default for ResponseSpec {
    fn default() -> Self {
        Self {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: ResponseBodySpec::Unset,
        }
    }
}

/// The body the theme wants to produce.
///
/// HTML and JSON variants are modeled explicitly; streaming
/// variants will be wired in the HTTP layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseBodySpec {
    /// Theme has not set any body yet (used by theme.rs).
    Unset,

    /// Theme explicitly requested "no body" (used by ctx_bridge).
    None,

    /// Render a template with a JSON model (HTML responses).
    HtmlTemplate { template: String, model: Json },

    /// Raw HTML string (not recommended for large bodies).
    HtmlString(String),

    /// JSON value (will be serialized to UTF-8).
    JsonValue(Json),
}

impl ResponseSpec {
    pub fn set_status(&mut self, status: StatusCode) {
        self.status = status;
    }

    pub fn set_header(&mut self, name: &str, value: &str) -> Result<(), ContextError> {
        use http::header::HeaderName;

        let header_name: HeaderName = name
            .parse()
            .map_err(|_| ContextError::InvalidHeaderValue(name.to_string()))?;
        let hv = value
            .parse()
            .map_err(|_| ContextError::InvalidHeaderValue(value.to_string()))?;

        self.headers.insert(header_name, hv);
        Ok(())
    }

    pub fn append_header(&mut self, name: &str, value: &str) -> Result<(), ContextError> {
        use http::header::HeaderName;

        let header_name: HeaderName = name
            .parse()
            .map_err(|_| ContextError::InvalidHeaderValue(name.to_string()))?;
        let hv = value
            .parse()
            .map_err(|_| ContextError::InvalidHeaderValue(value.to_string()))?;

        self.headers.append(header_name, hv);
        Ok(())
    }

    pub fn remove_header(&mut self, name: &str) {
        self.headers.remove(name);
    }

    pub fn set_html_template(&mut self, template: String, model: Json) {
        self.body = ResponseBodySpec::HtmlTemplate { template, model };
    }

    pub fn set_html_string<S: Into<String>>(&mut self, html: S) {
        self.body = ResponseBodySpec::HtmlString(html.into());
    }

    pub fn set_json_value(&mut self, value: Json) {
        self.body = ResponseBodySpec::JsonValue(value);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON-first RequestContext
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RequestContext {
    pub req_id: Json, // UUID as JSON string
    pub req_path: Json,
    pub req_method: Json,
    pub req_version: Json,
    pub req_headers: Json,
    pub req_params: Json,

    pub content_meta: Json, // includes frontmatter etc.
    pub theme_config: Json,
    pub plugin_configs: HashMap<String, Json>,

    #[serde(skip)]
    pub req_body: Option<StreamHandle>, // opaque HTTP request stream

    #[serde(skip)]
    pub content_body: Option<StreamHandle>, // opaque FS / CAS stream

    pub recommendations: Recommendations,
    pub response_spec: ResponseSpec,
}

impl RequestContext {
    /// Convenience constructor that wires through to the builder.
    ///
    /// Note: `req_id` is auto-generated as a UUID JSON string.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        req_path: Json,
        req_method: Json,
        req_version: Json,
        req_headers: Json,
        req_params: Json,
        content_meta: Json,
        theme_config: Json,
        plugin_configs: HashMap<String, Json>,
        req_body: Option<StreamHandle>,
        content_body: Option<StreamHandle>,
    ) -> Self {
        RequestContext::builder()
            .path(req_path)
            .method(req_method)
            .version(req_version)
            .headers(req_headers)
            .params(req_params)
            .content_meta(content_meta)
            .theme_config(theme_config)
            .plugin_configs(plugin_configs)
            .req_body_opt(req_body)
            .content_body_opt(content_body)
            .build()
    }

    pub fn builder() -> RequestContextBuilder {
        RequestContextBuilder::new()
    }

    /// Consume the context and extract just the `ResponseBodySpec`.
    /// Used by the theme actor / HTTP layer once JS processing is finished.
    pub fn into_response_body_spec(self) -> ResponseBodySpec {
        debug!("ResponseSpec: {:?}", self.response_spec);
        debug!("ResponseBodySpec: {:?}", self.response_spec.body);
        self.response_spec.body
    }

    /// Borrow the current response body spec.
    pub fn response_body_spec(&self) -> &ResponseBodySpec {
        &self.response_spec.body
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Builder pattern for RequestContext
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default, Debug, Clone)]
pub struct RequestContextBuilder {
    pub req_path: Json,
    pub req_method: Json,
    pub req_version: Json,
    pub req_headers: Json,
    pub req_params: Json,
    pub content_meta: Json,
    pub theme_config: Json,
    pub plugin_configs: HashMap<String, Json>,
    pub req_body: Option<StreamHandle>,
    pub content_body: Option<StreamHandle>,
}

impl RequestContextBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn path(mut self, v: impl Into<Json>) -> Self {
        self.req_path = v.into();
        self
    }

    pub fn method(mut self, v: impl Into<Json>) -> Self {
        self.req_method = v.into();
        self
    }

    pub fn version(mut self, v: impl Into<Json>) -> Self {
        self.req_version = v.into();
        self
    }

    pub fn headers(mut self, v: impl Into<Json>) -> Self {
        self.req_headers = v.into();
        self
    }

    pub fn params(mut self, v: impl Into<Json>) -> Self {
        self.req_params = v.into();
        self
    }

    pub fn content_meta(mut self, v: impl Into<Json>) -> Self {
        self.content_meta = v.into();
        self
    }

    pub fn theme_config(mut self, v: impl Into<Json>) -> Self {
        self.theme_config = v.into();
        self
    }

    pub fn plugin_config(mut self, id: impl Into<String>, cfg: impl Into<Json>) -> Self {
        self.plugin_configs.insert(id.into(), cfg.into());
        self
    }

    pub fn plugin_configs(mut self, map: HashMap<String, Json>) -> Self {
        self.plugin_configs = map;
        self
    }

    pub fn req_body(mut self, s: StreamHandle) -> Self {
        self.req_body = Some(s);
        self
    }

    pub fn req_body_opt(mut self, s: Option<StreamHandle>) -> Self {
        self.req_body = s;
        self
    }

    pub fn content_body(mut self, s: StreamHandle) -> Self {
        self.content_body = Some(s);
        self
    }

    pub fn content_body_opt(mut self, s: Option<StreamHandle>) -> Self {
        self.content_body = s;
        self
    }

    pub fn build(self) -> RequestContext {
        RequestContext {
            req_id: Json::String(Uuid::new_v4().to_string()),
            req_path: self.req_path,
            req_method: self.req_method,
            req_version: match self.req_version.is_null() {
                true => json!("HTTP/1.1"),
                false => self.req_version,
            },
            req_headers: self.req_headers,
            req_params: self.req_params,
            content_meta: self.content_meta,
            theme_config: self.theme_config,
            plugin_configs: self.plugin_configs,
            req_body: self.req_body,
            content_body: self.content_body,
            recommendations: Recommendations::default(),
            response_spec: ResponseSpec::default(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Helper to construct a fake StreamHandle
    fn fake_stream_handle() -> StreamHandle {
        fn read_fn(_id: StreamId, dst: &mut [u8]) -> StreamStatus {
            // pretend we always read 1 byte of 'A'
            if !dst.is_empty() {
                dst[0] = b'A';
                StreamStatus::Ok(1)
            } else {
                StreamStatus::Ok(0)
            }
        }

        fn close_fn(_id: StreamId) {
            // no-op for tests
        }

        let id = next_stream_id();

        StreamHandle {
            id,
            vtable: StreamVTable {
                read: read_fn,
                close: Some(close_fn),
            },
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // serde_headermap tests
    // ─────────────────────────────────────────────────────────────────────────

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct HeaderMapHolder {
        #[serde(with = "super::serde_headermap")]
        headers: HeaderMap,
    }

    #[test]
    fn serde_headermap_roundtrip() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("text/html"));
        headers.insert("x-custom", HeaderValue::from_static("value"));

        let original = HeaderMapHolder { headers };

        let json = serde_json::to_string(&original).expect("serialize HeaderMapHolder");
        let value: Json = serde_json::from_str(&json).expect("parse serialized HeaderMapHolder");

        assert_eq!(value["headers"]["content-type"], json!("text/html"));
        assert_eq!(value["headers"]["x-custom"], json!("value"));

        let decoded: HeaderMapHolder =
            serde_json::from_str(&json).expect("deserialize HeaderMapHolder");
        let ct = decoded
            .headers
            .get("content-type")
            .expect("content-type should exist");
        assert_eq!(ct.to_str().unwrap(), "text/html");

        let xc = decoded
            .headers
            .get("x-custom")
            .expect("x-custom should exist");
        assert_eq!(xc.to_str().unwrap(), "value");
    }

    #[test]
    fn serde_headermap_invalid_name_fails_deserialize() {
        // Space in header name is invalid.
        let json = r#"{ "headers": { "bad name": "value" } }"#;
        let decoded: Result<HeaderMapHolder, _> = serde_json::from_str(json);
        assert!(
            decoded.is_err(),
            "invalid header name should fail to deserialize"
        );
    }

    #[test]
    fn serde_headermap_invalid_value_fails_deserialize() {
        // Newlines in header values are invalid.
        let json = r#"{ "headers": { "x-bad": "line1\nline2" } }"#;
        let decoded: Result<HeaderMapHolder, _> = serde_json::from_str(json);
        assert!(
            decoded.is_err(),
            "invalid header value should fail to deserialize"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // serde_status tests
    // ─────────────────────────────────────────────────────────────────────────

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct StatusHolder {
        #[serde(with = "super::serde_status")]
        status: StatusCode,
    }

    #[test]
    fn serde_status_roundtrip() {
        let original = StatusHolder {
            status: StatusCode::CREATED,
        };

        let json = serde_json::to_string(&original).expect("serialize StatusHolder");
        let value: Json = serde_json::from_str(&json).expect("parse serialized StatusHolder");

        assert_eq!(value["status"], json!(201));

        let decoded: StatusHolder = serde_json::from_str(&json).expect("deserialize StatusHolder");
        assert_eq!(decoded, original);
    }

    #[test]
    fn serde_status_invalid_code_fails_deserialize() {
        // 99 is not a valid HTTP status (must be 100..=599)
        let json = r#"{ "status": 99 }"#;
        let decoded: Result<StatusHolder, _> = serde_json::from_str(json);
        assert!(
            decoded.is_err(),
            "invalid status code should fail to deserialize"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ResponseSpec / ResponseBodySpec tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn response_spec_default_values() {
        let spec = ResponseSpec::default();

        assert_eq!(spec.status, StatusCode::OK);
        assert!(spec.headers.is_empty());
        assert!(
            matches!(spec.body, ResponseBodySpec::Unset),
            "default body should be Unset"
        );
    }

    #[test]
    fn response_spec_set_status() {
        let mut spec = ResponseSpec::default();
        spec.set_status(StatusCode::NOT_FOUND);
        assert_eq!(spec.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn response_spec_set_header_success() {
        let mut spec = ResponseSpec::default();

        spec.set_header("x-custom", "value")
            .expect("header should be valid");

        let hv = spec
            .headers
            .get("x-custom")
            .expect("header should exist after set");
        assert_eq!(hv.to_str().unwrap(), "value");
    }

    #[test]
    fn response_spec_set_header_invalid_name() {
        let mut spec = ResponseSpec::default();

        let res = spec.set_header("bad name", "value");
        assert!(res.is_err(), "expected invalid header name to error");
    }

    #[test]
    fn response_spec_set_header_invalid_value() {
        let mut spec = ResponseSpec::default();

        let res = spec.set_header("x-bad", "line1\nline2");
        assert!(res.is_err(), "expected invalid header value to error");
    }

    #[test]
    fn response_spec_append_header_success() {
        let mut spec = ResponseSpec::default();

        spec.append_header("x-many", "one").expect("append first");
        spec.append_header("x-many", "two").expect("append second");

        let values: Vec<_> = spec
            .headers
            .get_all("x-many")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();

        assert_eq!(values, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn response_spec_remove_header() {
        let mut spec = ResponseSpec::default();
        spec.set_header("x-remove", "value").unwrap();

        assert!(spec.headers.contains_key("x-remove"));
        spec.remove_header("x-remove");
        assert!(!spec.headers.contains_key("x-remove"));
    }

    #[test]
    fn response_body_spec_setters() {
        let mut spec = ResponseSpec::default();

        // HtmlTemplate
        let model = json!({ "foo": "bar" });
        spec.set_html_template("tpl".to_string(), model.clone());
        match &spec.body {
            ResponseBodySpec::HtmlTemplate { template, model: m } => {
                assert_eq!(template, "tpl");
                assert_eq!(m, &model);
            }
            other => panic!("expected HtmlTemplate, got {other:?}"),
        }

        // HtmlString
        spec.set_html_string("<h1>Hi</h1>");
        match &spec.body {
            ResponseBodySpec::HtmlString(s) => {
                assert_eq!(s, "<h1>Hi</h1>");
            }
            other => panic!("expected HtmlString, got {other:?}"),
        }

        // JsonValue
        let value = json!({ "ok": true });
        spec.set_json_value(value.clone());
        match &spec.body {
            ResponseBodySpec::JsonValue(v) => {
                assert_eq!(v, &value);
            }
            other => panic!("expected JsonValue, got {other:?}"),
        }
    }

    #[test]
    fn response_spec_serde_roundtrip() {
        let mut spec = ResponseSpec::default();
        spec.set_status(StatusCode::CREATED);
        spec.set_header("x-serde", "yes").unwrap();
        spec.set_json_value(json!({ "answer": 42 }));

        let value = serde_json::to_value(&spec).expect("serialize ResponseSpec");

        assert_eq!(value["status"], json!(201));
        assert_eq!(value["headers"]["x-serde"], json!("yes"));

        let decoded: ResponseSpec =
            serde_json::from_value(value).expect("deserialize ResponseSpec");
        assert_eq!(decoded.status, StatusCode::CREATED);
        assert_eq!(
            decoded.headers.get("x-serde").unwrap().to_str().unwrap(),
            "yes"
        );
        assert!(
            matches!(decoded.body, ResponseBodySpec::JsonValue(_)),
            "body should deserialize as JsonValue"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // RequestContext / Builder / StreamHandle tests
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn builder_minimal_builds_valid_ctx_with_auto_uuid() {
        let ctx = RequestContext::builder()
            .path("/foo")
            .method("GET")
            .version("HTTP/1.1")
            .headers(json!({"host": "example"}))
            .params(json!({}))
            .content_meta(json!({}))
            .theme_config(json!({}))
            .build();

        // req_id should be a JSON string containing a UUID
        let id_str = ctx.req_id.as_str().expect("req_id must be a JSON string");
        assert!(!id_str.is_empty(), "req_id string should not be empty");

        assert_eq!(ctx.req_path, json!("/foo"));
        assert_eq!(ctx.req_method, json!("GET"));
        assert_eq!(ctx.req_version, json!("HTTP/1.1"));
        assert_eq!(ctx.req_headers, json!({"host": "example"}));
        assert_eq!(ctx.req_params, json!({}));
        assert!(ctx.plugin_configs.is_empty());
        assert!(ctx.req_body.is_none());
        assert!(ctx.content_body.is_none());
    }

    #[test]
    fn builder_allows_overriding_plugin_configs() {
        let mut map = HashMap::new();
        map.insert("p1".to_string(), json!({"enabled": true}));

        let ctx = RequestContext::builder()
            .path("/plugins")
            .method("POST")
            .version("HTTP/2")
            .headers(json!({}))
            .params(json!({}))
            .content_meta(json!({}))
            .theme_config(json!({}))
            .plugin_configs(map)
            .plugin_config("p2", json!({"flag": 1}))
            .build();

        assert_eq!(ctx.plugin_configs.len(), 2);
        assert_eq!(ctx.plugin_configs["p1"], json!({"enabled": true}));
        assert_eq!(ctx.plugin_configs["p2"], json!({"flag": 1}));
    }

    #[test]
    fn builder_with_streams_sets_handles() {
        let h = fake_stream_handle();

        let ctx = RequestContext::builder()
            .path("/stream")
            .method("GET")
            .version("HTTP/3")
            .headers(json!({}))
            .params(json!({}))
            .content_meta(json!({}))
            .theme_config(json!({}))
            .req_body(h)
            .content_body(h)
            .build();

        assert!(ctx.req_body.is_some());
        assert!(ctx.content_body.is_some());
    }

    #[test]
    fn stream_handle_read_and_close_are_callable() {
        let h = fake_stream_handle();

        let mut buf = [0u8; 4];
        let status = h.read(&mut buf);
        assert_eq!(status, StreamStatus::Ok(1));
        assert_eq!(buf[0], b'A');

        // Should not panic
        h.close();
    }

    #[test]
    fn request_context_serialization_skips_stream_fields() {
        let h = fake_stream_handle();

        let ctx = RequestContext::builder()
            .path("/skip_streams")
            .method("GET")
            .version("HTTP/1.1")
            .headers(json!({}))
            .params(json!({}))
            .content_meta(json!({}))
            .theme_config(json!({}))
            .req_body(h)
            .content_body(h)
            .build();

        let value = serde_json::to_value(&ctx).expect("serialize RequestContext");
        let obj = value.as_object().expect("expected JSON object");

        assert!(
            !obj.contains_key("req_body"),
            "req_body must be skipped during serialization"
        );
        assert!(
            !obj.contains_key("content_body"),
            "content_body must be skipped during serialization"
        );
    }

    #[test]
    fn request_context_new_uses_builder_and_generates_uuid() {
        let ctx = RequestContext::new(
            json!("/from_new"),
            json!("PUT"),
            json!("HTTP/1.0"),
            json!({"h": "v"}),
            json!({"k": "v"}),
            json!({"meta": true}),
            json!({"theme": "t"}),
            HashMap::new(),
            None,
            None,
        );

        assert_eq!(ctx.req_path, json!("/from_new"));
        assert_eq!(ctx.req_method, json!("PUT"));
        assert_eq!(ctx.req_version, json!("HTTP/1.0"));
        assert!(ctx.req_id.is_string(), "req_id should be auto-generated");
    }

    #[test]
    fn into_response_body_spec_moves_body_out() {
        let mut ctx = RequestContext::builder()
            .path("/")
            .method("GET")
            .version("HTTP/1.1")
            .headers(json!({}))
            .params(json!({}))
            .content_meta(json!({}))
            .theme_config(json!({}))
            .build();

        ctx.response_spec.set_html_string("<p>hi</p>");

        let body = ctx.into_response_body_spec();
        match body {
            ResponseBodySpec::HtmlString(s) => assert_eq!(s, "<p>hi</p>"),
            other => panic!("expected HtmlString, got {other:?}"),
        }
    }

    #[test]
    fn response_body_spec_reference_is_borrowed() {
        let mut ctx = RequestContext::builder()
            .path("/")
            .method("GET")
            .version("HTTP/1.1")
            .headers(json!({}))
            .params(json!({}))
            .content_meta(json!({}))
            .theme_config(json!({}))
            .build();

        assert!(
            matches!(ctx.response_body_spec(), ResponseBodySpec::Unset),
            "default body should be Unset"
        );

        ctx.response_spec.set_json_value(json!({"ok": true}));

        match ctx.response_body_spec() {
            ResponseBodySpec::JsonValue(v) => assert_eq!(v["ok"], true),
            other => panic!("expected JsonValue, got {other:?}"),
        }
    }
}
