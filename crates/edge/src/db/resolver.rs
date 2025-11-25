// crates/edge/src/db/resolver.rs

//! Edge-side resolver adapters.
//!
//! This module connects the storage-agnostic resolver logic in
//! `serve::resolver` to the concrete backends wired up in the edge
//! layer (IndexedJson for front matter, Tantivy for rendered content).

use crate::fs::index::{lookup_front_matter_by_path, lookup_front_matter_by_slug};
use domain::stream::StreamHandle;
use serde_json::Value as Json;
use serve::resolver::ResolverError;
use std::path::Path;

/// Lookup front matter by slug, using IndexedJson.
///
/// This is injected into `serve::resolver::set_resolver_deps`.
pub fn lookup_fm_by_slug(slug: &str) -> Result<Option<Json>, ResolverError> {
    lookup_front_matter_by_slug(slug).map_err(|e| ResolverError::Backend(e.to_string()))
}

/// Lookup front matter by **served path**, e.g. `/index.html`.
///
/// The resolver passes normalized HTTP paths; we treat them as-is.
pub fn lookup_fm_by_served(served: &str) -> Result<Option<Json>, ResolverError> {
    let path = Path::new(served);
    lookup_front_matter_by_path(path).map_err(|e| ResolverError::Backend(e.to_string()))
}

/// Lookup body stream handle by **served path**.
///
/// In Option A, we key the CAS/Tantivy index by the same served path
/// strings (`/index.html`, `/docs/search.html`, etc.), so we can
/// construct the handle directly without hitting the index here.
///
/// If there is no corresponding CAS entry, the eventual stream open
/// will fail with an I/O error, which is acceptable for now.
pub fn lookup_body_handle(served: &str) -> Result<Option<StreamHandle>, ResolverError> {
    // `serve::resolver` already normalizes paths to start with `/`,
    // and we index bodies with the same canonical ID.
    let key = served.to_owned();
    Ok(Some(StreamHandle::Cas { key }))
}
