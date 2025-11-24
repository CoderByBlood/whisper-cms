use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::path::PathBuf;

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
/// This is produced by the edge-layer resolver and used to
/// populate `RequestContext.content_meta` and any body path
/// that might later be turned into a StreamHandle.
#[derive(Debug, Clone)]
pub struct ResolvedContent {
    pub content_kind: ContentKind,
    pub front_matter: Json,
    pub body_path: PathBuf,
}
