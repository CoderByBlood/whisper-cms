// crates/serve/src/ctx/http.rs

use crate::render::recommendation::Recommendations;
use domain::stream::StreamHandle;
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
        debug!("ResponseSpec Headers: {:?}", self.response_spec.headers);
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
