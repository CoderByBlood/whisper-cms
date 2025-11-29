// crates/domain/src/content.rs

use crate::stream::{BytesStream, StreamHandle, Utf8Stream};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as Json};

/// Content classification for a given request path.
///
/// This is determined before plugins/themes run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContentKind {
    /// Static asset: bypasses plugins/themes and all transformation pipeline.
    Asset,

    /// HTML content: theme generates HTML response.
    Html,

    /// JSON content: theme generates JSON response.
    Json,
}

/// Information resolved from the request path.
///
/// Produced by the edge-layer resolver and used to populate
/// `RequestContext.content_meta` and body streams.
///
/// IMPORTANT:
/// - There is **no** filesystem `PathBuf` here.
/// - Body access is via a single `StreamHandle`, which can point at:
///     * filesystem content,
///     * CAS/Tantivy-backed HTML,
///     * or any other backing store.
/// - Materialization (`StreamHandle -> BoxStream`) is done via the DI in
///   `crate::stream`.
#[derive(Debug, Clone)]
pub struct ResolvedContent {
    /// Classified content kind (HTML / JSON / Asset).
    pub content_kind: ContentKind,

    /// Front matter / metadata for this path.
    pub front_matter: Json,

    /// Optional body stream handle (FS or CAS).
    pub body: Option<StreamHandle>,
}

impl ResolvedContent {
    /// Convenience constructor for "no content" / empty cases.
    ///
    /// - `content_kind`: Asset
    /// - `front_matter`: {}
    /// - No body stream.
    pub fn empty() -> Self {
        Self {
            content_kind: ContentKind::Asset,
            front_matter: Json::Object(JsonMap::new()),
            body: None,
        }
    }

    /// Builder-style helper to attach a body stream handle.
    pub fn with_body(mut self, body: Option<StreamHandle>) -> Self {
        self.body = body;
        self
    }

    /// Materialize the body as a byte stream.
    ///
    /// Returns `None` if no body handle is present.
    pub fn bytes_stream(&self) -> Option<BytesStream> {
        self.body.as_ref().map(|h| h.open_bytes())
    }

    /// Materialize the body as a UTF-8 stream.
    ///
    /// Returns `None` if no body handle is present.
    pub fn utf8_stream(&self) -> Option<Utf8Stream> {
        self.body.as_ref().map(|h| h.open_utf8())
    }
}
