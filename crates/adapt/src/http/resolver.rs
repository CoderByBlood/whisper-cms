use crate::core::content::ContentKind;
use crate::core::context::RequestContext;
use crate::core::error::CoreError;
use http::Method;
use serde_json::json;
use serde_json::Value as Json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Information resolved from the request path.
#[derive(Debug, Clone)]
pub struct ResolvedContent {
    pub content_kind: ContentKind,
    pub front_matter: Json,
    pub body_path: PathBuf,
}

/// Trait for resolving HTTP requests to content metadata.
pub trait ContentResolver: Send + Sync {
    fn resolve(&self, path: &str, method: &Method) -> Result<ResolvedContent, CoreError>;
}

/// A simple resolver that maps URI paths to files under a root directory,
/// with naive content-kind detection based on file extension.
///
/// This is a placeholder for Phase 3; a more complete resolver can be
/// provided in later phases.
pub struct SimpleContentResolver {
    pub root: PathBuf,
}

impl SimpleContentResolver {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }
}

impl ContentResolver for SimpleContentResolver {
    fn resolve(&self, path: &str, _method: &Method) -> Result<ResolvedContent, CoreError> {
        // Normalize path, avoid leading slash issues
        let trimmed = path.trim_start_matches('/');
        let body_path = self.root.join(trimmed);

        // Very naive content-kind detection:
        let content_kind = if path.ends_with(".html") {
            ContentKind::HtmlContent
        } else if path.ends_with(".json") {
            ContentKind::JsonContent
        } else {
            ContentKind::StaticAsset
        };

        // Phase 3: no real front-matter yet; use empty object.
        let front_matter = json!({});

        Ok(ResolvedContent {
            content_kind,
            front_matter,
            body_path,
        })
    }
}

/// Helper to build an initial RequestContext from HTTP parts + ResolvedContent.
pub fn build_request_context(
    path: String,
    method: http::Method,
    headers: http::HeaderMap,
    query_params: HashMap<String, String>,
    resolved: ResolvedContent,
) -> RequestContext {
    let theme_config = Json::Object(Default::default());
    let plugin_configs = HashMap::new();

    RequestContext::new(
        path,
        method,
        headers,
        query_params,
        resolved.content_kind,
        resolved.front_matter,
        resolved.body_path,
        theme_config,
        plugin_configs,
    )
}
