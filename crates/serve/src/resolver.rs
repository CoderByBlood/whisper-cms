// crates/serve/src/resolver.rs

//! Storage-agnostic content resolution.
//!
//! The *business logic* for request → content mapping lives here, but all actual storage
//! access (filesystem, indexed_json, Tantivy/CAS) is injected from the edge crate at
//! startup via `set_resolver_deps()`.
//!
//! This crate knows only about:
//!   - request path
//!   - normalized slug/path logic
//!   - ResolvedContent { content_kind, front_matter, body }
//!
//! All the data retrieval — FM lookup, content lookup, slug lookup, CAS stream creation —
//! is performed via injected closures.

use domain::content::{ContentKind, ResolvedContent};
use http::{HeaderMap, Method};
use serde_json::{json, Map as JsonMap, Value as Json};
use std::{collections::HashMap, string::FromUtf8Error};
use thiserror::Error;

use crate::{indexer::ContentManager, render::http::RequestContext};

// -----------------------------------------------------------------------------
// Error Type
// -----------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ResolverError {
    #[error("Io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("FromUTF8 error: {0}")]
    FromUTF8(#[from] FromUtf8Error),

    #[error("Backend error: {0}")]
    Backend(String),
}

// -----------------------------------------------------------------------------
// Utility
// -----------------------------------------------------------------------------

fn normalize(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{}", path)
    }
}

/// For extension inference on normalized paths.
fn infer_kind_from_ext(path: &str) -> ContentKind {
    match path.rsplit('.').next() {
        Some("html") => ContentKind::Html,
        Some("json") => ContentKind::Json,
        _ => ContentKind::Asset,
    }
}

// -----------------------------------------------------------------------------
// 0–5 RESOLUTION LOGIC
// -----------------------------------------------------------------------------

#[tracing::instrument(skip_all)]
pub async fn resolve(
    resolver: &impl ContentManager,
    path: &str,
    _method: &Method,
) -> Result<ResolvedContent, ResolverError> {
    let path = normalize(path);

    // -------------------------------------------
    // Step 1: Does the path match a slug exactly?
    // -------------------------------------------
    let slug = path.strip_prefix('/').unwrap_or(path.as_str());
    if let Some(fm) = resolver.lookup_slug(slug).await? {
        // Try to find a served-path for the body.

        // 1) Prefer an explicit served_path in front matter, if present.
        let handle: Option<&str> = fm.get("id").and_then(|v| v.as_str());
        if let Some(h) = handle {
            if let Some(body) = resolver.lookup_body(h).await? {
                // Only short-circuit here if we got a body as well as front matter.
                return Ok(ResolvedContent {
                    content_kind: infer_kind_from_ext(&path),
                    front_matter: fm,
                    body: Some(body),
                });
            }
        }

        // If we found FM but not body, we *deliberately* fall through to the served-path
        // resolution steps below, which might still find a body by path.
    }

    // ------------------------------------------------------------
    // Step 2: Does the path match served-path exactly?
    // ------------------------------------------------------------
    if let Some(fm) = resolver.lookup_served(&path).await? {
        if let Some(h) = resolver.lookup_body(&path).await? {
            return Ok(ResolvedContent {
                content_kind: infer_kind_from_ext(&path),
                front_matter: fm,
                body: Some(h),
            });
        }
    }

    // ------------------------------------------------------------
    // Step 3: Try adding `.html`
    // ------------------------------------------------------------
    if !path.ends_with(".html") {
        let html = format!("{}.html", path.trim_end_matches('/'));
        if let Some(fm) = resolver.lookup_served(&html).await? {
            if let Some(h) = resolver.lookup_body(&html).await? {
                return Ok(ResolvedContent {
                    content_kind: ContentKind::Html,
                    front_matter: fm,
                    body: Some(h),
                });
            }
        }
    }

    // ------------------------------------------------------------
    // Step 4: If trailing slash, try `<path>/index.html`
    // ------------------------------------------------------------
    if path.ends_with('/') {
        let index = format!("{}index.html", path);
        if let Some(fm) = resolver.lookup_served(&index).await? {
            if let Some(h) = resolver.lookup_body(&index).await? {
                return Ok(ResolvedContent {
                    content_kind: ContentKind::Html,
                    front_matter: fm,
                    body: Some(h),
                });
            }
        }
    }

    // ------------------------------------------------------------
    // Step 5: No matches — return empty
    // ------------------------------------------------------------
    Ok(ResolvedContent::empty())
}

/// Default implementation used by both adapt and edge.
///
/// - Normalizes header names to `Accept-Language` style.
/// - Converts query params map into a JSON object.
/// - Uses `resolved.front_matter` as `content_meta`.
#[tracing::instrument(skip_all)]
pub fn build_request_context(
    path: String,
    method: Method,
    headers: HeaderMap,
    query_params: HashMap<String, String>,
    resolved: ResolvedContent,
) -> RequestContext {
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
        .path(Json::String(path))
        .method(Json::String(method.to_string()))
        .headers(Json::Object(hdr_obj))
        .params(Json::Object(qp_obj))
        .content_meta(resolved.front_matter)
        .theme_config(Json::Object(JsonMap::new()))
        .plugin_configs(HashMap::new())
        .content_body_opt(resolved.body)
        .build()
}

#[tracing::instrument(skip_all)]
pub fn canonicalize_header_name(raw: &str) -> String {
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
