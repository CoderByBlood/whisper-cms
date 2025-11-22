// crates/adapt/src/http/resolver.rs

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
#[derive(Debug, Clone)]
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

fn canonicalize_header_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut upper_next = true;

    for ch in raw.chars() {
        if ch == '-' {
            out.push('-');
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.extend(ch.to_lowercase());
        }
    }

    out
}

/// Helper to build an initial RequestContext from HTTP parts + ResolvedContent.
pub fn build_request_context(
    path: String,
    method: http::Method,
    headers: http::HeaderMap,
    query_params: HashMap<String, String>,
    resolved: ResolvedContent,
) -> RequestContext {
    use serde_json::{Map as JsonMap, Value as Json};

    // headers -> JSON object
    let mut hdr_obj = JsonMap::new();
    for (name, value) in headers.iter() {
        let canonical = canonicalize_header_name(name.as_str());
        hdr_obj.insert(canonical, json!(value.to_str().unwrap_or("")));
    }

    // query_params -> JSON object
    let mut qp_obj = JsonMap::new();
    for (k, v) in query_params.iter() {
        qp_obj.insert(k.clone(), Json::String(v.clone()));
    }

    RequestContext::builder()
        // req_* fields
        .path(Json::String(path))
        .method(Json::String(method.to_string()))
        // let version default from the builder; resolver doesn't know the real version
        .headers(Json::Object(hdr_obj))
        .params(Json::Object(qp_obj))
        // front_matter becomes the initial content_meta shape
        .content_meta(resolved.front_matter)
        // start empty; can be filled later by higher layers
        .theme_config(Json::Object(JsonMap::new()))
        .plugin_configs(HashMap::new())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::content::ContentKind;
    use http::Method;
    use serde_json::json;
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
        use http::HeaderMap;
        use http::Method;
        use serde_json::{json, Value as Json};
        use std::collections::HashMap;
        use std::path::PathBuf;

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

        // req_path
        assert_eq!(
            ctx.req_path,
            Json::String(path),
            "req_path must reflect the original path"
        );

        // req_method
        assert_eq!(
            ctx.req_method,
            Json::String(method.to_string()),
            "req_method must reflect the original method"
        );

        // req_headers JSON object
        let hdrs = ctx
            .req_headers
            .as_object()
            .expect("req_headers must be a JSON object");
        assert_eq!(
            hdrs.get("X-Test-Header").unwrap(),
            "value",
            "header value should be serialized into JSON"
        );

        // req_params JSON object
        let params = ctx
            .req_params
            .as_object()
            .expect("req_params must be a JSON object");
        assert_eq!(params.get("q").unwrap(), "rust");

        // content_meta from front_matter
        assert_eq!(
            ctx.content_meta["title"],
            json!("My Post"),
            "front_matter.title should be in content_meta"
        );

        // theme_config should start as empty object
        assert!(ctx.theme_config.is_object());
        assert!(ctx.theme_config.as_object().unwrap().is_empty());

        // plugin_configs should start empty
        assert!(ctx.plugin_configs.is_empty());

        // req_id is an auto-generated UUID in JSON
        match &ctx.req_id {
            Json::String(s) => {
                assert!(
                    uuid::Uuid::parse_str(s).is_ok(),
                    "req_id must be a valid UUID string"
                )
            }
            other => panic!("req_id must be a JSON string, got {other:?}"),
        }

        // req_version is a JSON string
        assert!(
            ctx.req_version.is_string(),
            "req_version must be a JSON string"
        );

        // Streams start unset
        assert!(ctx.req_body.is_none());
        assert!(ctx.content_body.is_none());
    }

    #[test]
    fn build_request_context_allows_empty_headers_and_query_params() {
        use http::{HeaderMap, Method};
        use serde_json::{json, Value as Json};
        use std::collections::HashMap;
        use std::path::PathBuf;

        let path = "/no/headers/or/query".to_string();
        let method = Method::HEAD;

        let headers = HeaderMap::new();
        let query_params: HashMap<String, String> = HashMap::new();

        let resolved = ResolvedContent {
            content_kind: ContentKind::Asset,
            front_matter: json!({}),
            body_path: PathBuf::from("/assets/file.bin"),
        };

        let ctx = build_request_context(
            path.clone(),
            method.clone(),
            headers,
            query_params,
            resolved,
        );

        assert_eq!(ctx.req_path, Json::String(path));
        assert_eq!(ctx.req_method, Json::String(method.to_string()));

        let hdrs = ctx
            .req_headers
            .as_object()
            .expect("req_headers must be object");
        assert!(hdrs.is_empty(), "headers should be empty object");

        let params = ctx
            .req_params
            .as_object()
            .expect("req_params must be object");
        assert!(params.is_empty(), "params should be empty object");

        assert_eq!(
            ctx.content_meta,
            json!({}),
            "content_meta should match provided empty front_matter"
        );
    }
}
