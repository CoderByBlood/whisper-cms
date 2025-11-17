// crates/adapt/src/core/context.rs

use super::content::ContentKind;
use super::error::CoreError;
use super::recommendation::Recommendations;
use http::{HeaderMap, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

// ─────────────────────────────────────────────────────────────────────────────
// Serde helpers for foreign types (http::Method, HeaderMap, StatusCode)
// ─────────────────────────────────────────────────────────────────────────────

mod serde_method {
    use http::Method;
    use serde::{de::Error as DeError, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(m: &Method, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(m.as_str())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Method, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Method::from_bytes(s.as_bytes()).map_err(D::Error::custom)
    }
}

mod serde_headermap {
    use http::{header::HeaderName, HeaderMap, HeaderValue};
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
    use http::StatusCode;
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

/// Per-request context backing the JS `ctx` object.
///
/// This is the central state passed through plugins, theme, and the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    pub request_id: Uuid,
    pub path: String,

    #[serde(with = "serde_method")]
    pub method: Method,

    #[serde(with = "serde_headermap")]
    pub headers: HeaderMap,

    pub query_params: HashMap<String, String>,

    pub content_kind: ContentKind,
    pub front_matter: Json,
    pub body_path: PathBuf,

    pub theme_config: Json,
    pub plugin_configs: HashMap<String, Json>,

    pub recommendations: Recommendations,
    pub response_spec: ResponseSpec,
}

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
            // Start as Unset: theme hasn't decided on a body yet.
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

impl RequestContext {
    pub fn new(
        path: String,
        method: Method,
        headers: HeaderMap,
        query_params: HashMap<String, String>,
        content_kind: ContentKind,
        front_matter: Json,
        body_path: PathBuf,
        theme_config: Json,
        plugin_configs: HashMap<String, Json>,
    ) -> Self {
        Self {
            request_id: Uuid::new_v4(),
            path,
            method,
            headers,
            query_params,
            content_kind,
            front_matter,
            body_path,
            theme_config,
            plugin_configs,
            recommendations: Recommendations::default(),
            response_spec: ResponseSpec::default(),
        }
    }
}

impl ResponseSpec {
    pub fn set_status(&mut self, status: StatusCode) {
        self.status = status;
    }

    pub fn set_header(&mut self, name: &str, value: &str) -> Result<(), CoreError> {
        use http::header::HeaderName;

        let header_name: HeaderName = name
            .parse()
            .map_err(|_| CoreError::InvalidHeaderValue(name.to_string()))?;
        let hv = value
            .parse()
            .map_err(|_| CoreError::InvalidHeaderValue(value.to_string()))?;

        self.headers.insert(header_name, hv);
        Ok(())
    }

    pub fn append_header(&mut self, name: &str, value: &str) -> Result<(), CoreError> {
        use http::header::HeaderName;

        let header_name: HeaderName = name
            .parse()
            .map_err(|_| CoreError::InvalidHeaderValue(name.to_string()))?;
        let hv = value
            .parse()
            .map_err(|_| CoreError::InvalidHeaderValue(value.to_string()))?;

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
