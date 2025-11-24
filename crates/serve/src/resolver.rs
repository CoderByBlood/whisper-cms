// crates/serve/src/resolver.rs

use crate::ctx::http::{ContextError, RequestContext};
use domain::content::{ContentKind, ResolvedContent};
use http::Method;
use serde_json::{json, Map as JsonMap, Value as Json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::warn;

/// Trait for resolving HTTP requests to content metadata.
///
/// Implementations can live in the edge crate (e.g. filesystem / index
/// backed resolvers). The adapt crate only defines the contract.
pub trait ContentResolver: Send + Sync {
    fn resolve(&self, path: &str, method: &Method) -> Result<ResolvedContent, ContextError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Type-erased resolver + context builder (injected from edge)
// ─────────────────────────────────────────────────────────────────────────────

/// Type of the injected resolver function.
pub type ResolveFn = fn(path: &str, method: &Method) -> Result<ResolvedContent, ContextError>;

/// Type of the injected RequestContext builder.
pub type BuildRequestContextFn = fn(
    path: String,
    method: http::Method,
    headers: http::HeaderMap,
    query_params: HashMap<String, String>,
    resolved: ResolvedContent,
) -> RequestContext;

/// Global slots for injected functions (set from edge).
static RESOLVE_FN: OnceLock<ResolveFn> = OnceLock::new();
static BUILD_CTX_FN: OnceLock<BuildRequestContextFn> = OnceLock::new();

/// Inject the resolver function (edge calls this once at startup).
#[tracing::instrument(skip_all)]
pub fn set_resolver_fn(f: ResolveFn) -> Result<(), &'static str> {
    RESOLVE_FN
        .set(f)
        .map_err(|_| "resolver function already set")
}

/// Inject the RequestContext builder function (edge calls this once).
#[tracing::instrument(skip_all)]
pub fn set_build_request_context_fn(f: BuildRequestContextFn) -> Result<(), &'static str> {
    BUILD_CTX_FN
        .set(f)
        .map_err(|_| "build_request_context function already set")
}

/// Public, type-erased resolver entrypoint used everywhere in adapt.
///
/// If no resolver has been injected yet, falls back to a trivial
/// implementation that treats the path as an asset and returns empty
/// front matter.
#[tracing::instrument(skip_all)]
pub fn resolve(path: &str, method: &Method) -> Result<ResolvedContent, ContextError> {
    if let Some(f) = RESOLVE_FN.get() {
        return f(path, method);
    }

    warn!("resolve is defaulting because edge did not inject dependency");

    // Fallback: extremely simple, no indexing.
    let trimmed = path.trim_start_matches('/');
    Ok(ResolvedContent {
        content_kind: if trimmed.ends_with(".html") {
            ContentKind::Html
        } else if trimmed.ends_with(".json") {
            ContentKind::Json
        } else {
            ContentKind::Asset
        },
        front_matter: json!({}),
        body_path: PathBuf::from(trimmed),
    })
}

/// Public, type-erased RequestContext builder used by theme + plugin paths.
///
/// If no builder has been injected yet, uses a default implementation
/// that normalizes header names and plugs in `resolved.front_matter` as
/// `content_meta`.
#[tracing::instrument(skip_all)]
pub fn build_request_context(
    path: String,
    method: http::Method,
    headers: http::HeaderMap,
    query_params: HashMap<String, String>,
    resolved: ResolvedContent,
) -> RequestContext {
    if let Some(f) = BUILD_CTX_FN.get() {
        return f(path, method, headers, query_params, resolved);
    }

    warn!("build_request_context() is default because edge did not inject dependency");
    // Fallback to the default implementation.
    build_request_context_default(path, method, headers, query_params, resolved)
}

// ─────────────────────────────────────────────────────────────────────────────
// Default (non-injected) RequestContext builder
// ─────────────────────────────────────────────────────────────────────────────

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

/// Default implementation used when edge has not injected a builder.
///
/// - Normalizes header names to `Accept-Language` style.
/// - Converts query params map into a JSON object.
/// - Uses `resolved.front_matter` as `content_meta`.
fn build_request_context_default(
    path: String,
    method: http::Method,
    headers: http::HeaderMap,
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
