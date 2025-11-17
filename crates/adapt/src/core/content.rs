use serde::{Deserialize, Serialize};

/// Content classification for a given request path.
///
/// This is determined before plugins/themes run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContentKind {
    /// Static asset: bypasses plugins/themes and all transformation pipeline.
    StaticAsset,

    /// HTML content: theme generates HTML response.
    HtmlContent,

    /// JSON content: theme generates JSON response.
    JsonContent,
}
