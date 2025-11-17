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
            ContentKind::Html
        } else if path.ends_with(".json") {
            ContentKind::Json
        } else {
            ContentKind::Asset
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::content::ContentKind;
    use http::{HeaderMap, Method};
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;

    // ─────────────────────────────────────────────────────────────────────
    // SimpleContentResolver::resolve
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn resolve_html_path_maps_to_html_kind_and_correct_body_path() {
        let root = PathBuf::from("/var/www");
        let resolver = SimpleContentResolver::new(&root);

        let path = "/index.html";
        let method = Method::GET;

        let resolved = resolver
            .resolve(path, &method)
            .expect("resolver should succeed");

        assert_eq!(resolved.content_kind, ContentKind::Html);

        // Should trim leading slash and join with root
        let expected_body_path = root.join("index.html");
        assert_eq!(resolved.body_path, expected_body_path);

        // Front matter is currently an empty JSON object
        assert_eq!(resolved.front_matter, json!({}));
    }

    #[test]
    fn resolve_json_path_maps_to_json_kind() {
        let root = PathBuf::from("/content");
        let resolver = SimpleContentResolver::new(&root);

        let path = "/api/data.json";
        let method = Method::GET;

        let resolved = resolver
            .resolve(path, &method)
            .expect("resolver should succeed");

        assert_eq!(resolved.content_kind, ContentKind::Json);
        let expected_body_path = root.join("api/data.json");
        assert_eq!(resolved.body_path, expected_body_path);
    }

    #[test]
    fn resolve_other_extensions_map_to_asset_kind() {
        let root = PathBuf::from("/static");
        let resolver = SimpleContentResolver::new(&root);

        let cases = vec!["/style.css", "/image.png", "/no_extension"];

        for path in cases {
            let resolved = resolver
                .resolve(path, &Method::GET)
                .expect("resolver should succeed");

            assert_eq!(
                resolved.content_kind,
                ContentKind::Asset,
                "path {:?} should be Asset",
                path
            );

            let trimmed = path.trim_start_matches('/');
            let expected_body_path = root.join(trimmed);
            assert_eq!(resolved.body_path, expected_body_path);
        }
    }

    #[test]
    fn resolve_handles_multiple_leading_slashes_and_nested_paths() {
        let root = PathBuf::from("/root");
        let resolver = SimpleContentResolver::new(&root);

        let path = "///nested/dir/page.html";
        let resolved = resolver
            .resolve(path, &Method::GET)
            .expect("resolver should succeed");

        assert_eq!(resolved.content_kind, ContentKind::Html);

        // trim_start_matches('/') should leave "nested/dir/page.html"
        let expected_body_path = root.join("nested/dir/page.html");
        assert_eq!(resolved.body_path, expected_body_path);
    }

    #[test]
    fn resolve_does_not_depend_on_http_method_for_now() {
        let root = PathBuf::from("/root");
        let resolver = SimpleContentResolver::new(&root);

        let path = "/resource.json";

        let get_resolved = resolver
            .resolve(path, &Method::GET)
            .expect("GET resolve should succeed");
        let post_resolved = resolver
            .resolve(path, &Method::POST)
            .expect("POST resolve should succeed");

        // Implementation currently ignores method, so both should be identical.
        assert_eq!(get_resolved.content_kind, post_resolved.content_kind);
        assert_eq!(get_resolved.body_path, post_resolved.body_path);
        assert_eq!(get_resolved.front_matter, post_resolved.front_matter);
    }

    // ─────────────────────────────────────────────────────────────────────
    // build_request_context
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn build_request_context_populates_all_fields_correctly() {
        let path = "/blog/post.html".to_string();
        let method = Method::GET;

        let mut headers = HeaderMap::new();
        headers.insert("X-Test-Header", "value".parse().unwrap());

        let mut query_params = HashMap::new();
        query_params.insert("q".to_string(), "rust".to_string());

        let resolved = ResolvedContent {
            content_kind: ContentKind::Html,
            front_matter: json!({ "title": "My Post" }),
            body_path: PathBuf::from("/var/www/blog/post.html"),
        };

        let ctx = build_request_context(
            path.clone(),
            method.clone(),
            headers.clone(),
            query_params.clone(),
            resolved,
        );

        assert_eq!(ctx.path, path);
        assert_eq!(ctx.method, method);
        assert_eq!(ctx.headers, headers);
        assert_eq!(ctx.query_params, query_params);
        assert_eq!(ctx.content_kind, ContentKind::Html);
        assert_eq!(ctx.front_matter["title"], "My Post");
        assert_eq!(ctx.body_path, PathBuf::from("/var/www/blog/post.html"));

        // Theme config should start as empty object
        assert!(ctx.theme_config.is_object());
        assert!(ctx.theme_config.as_object().unwrap().is_empty());

        // Plugin configs should start empty
        assert!(ctx.plugin_configs.is_empty());

        // Recommendations should start empty
        assert!(ctx.recommendations.is_empty());

        // Response spec should be default (200 OK, empty headers, Unset body)
        assert_eq!(ctx.response_spec.status, http::StatusCode::OK);
        assert!(ctx.response_spec.headers.is_empty());
        match ctx.response_spec.body {
            crate::core::context::ResponseBodySpec::Unset => {}
            other => panic!("expected ResponseBodySpec::Unset, got {:?}", other),
        }
    }

    #[test]
    fn build_request_context_allows_empty_headers_and_query_params() {
        let path = "/no/headers/or/query".to_string();
        let method = Method::HEAD;

        let headers = HeaderMap::new();
        let query_params = HashMap::new();

        let resolved = ResolvedContent {
            content_kind: ContentKind::Asset,
            front_matter: json!({}),
            body_path: PathBuf::from("/assets/file.bin"),
        };

        let ctx = build_request_context(
            path.clone(),
            method.clone(),
            headers.clone(),
            query_params.clone(),
            resolved,
        );

        assert_eq!(ctx.path, path);
        assert_eq!(ctx.method, method);
        assert!(ctx.headers.is_empty());
        assert!(ctx.query_params.is_empty());
        assert_eq!(ctx.content_kind, ContentKind::Asset);
        assert_eq!(ctx.front_matter, json!({}));
        assert_eq!(ctx.body_path, PathBuf::from("/assets/file.bin"));
    }
}
