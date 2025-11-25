// crates/edge/src/db/resolver.rs

use crate::fs::index::fm_index_dir;
use adapt::mql::index::{IndexRecord, StringField};
use domain::content::{ContentKind, ResolvedContent};
use http::{HeaderMap, Method};
use indexed_json::{IndexEntry, IndexableField, IndexedJson, Query};
use serde_json::{json, Map as JsonMap, Value as Json};
use serve::ctx::http::{ContextError, RequestContext};
use serve::resolver::ContentResolver;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc as std_mpsc, LazyLock, Mutex};
use std::thread;
use thiserror::Error;
use tracing::debug;

// ─────────────────────────────────────────────
// Errors for FM lookup (internal to resolver)
// ─────────────────────────────────────────────

#[derive(Debug, Error)]
enum FrontMatterLookupError {
    #[error("FM index directory not configured")]
    NoIndexDir,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("indexed_json error: {0}")]
    IndexedJson(#[from] anyhow::Error),

    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),
}

// ─────────────────────────────────────────────
// Background worker for IndexedJson access
// ─────────────────────────────────────────────

enum ResolverJob {
    LookupById {
        id: String,
        resp: std_mpsc::Sender<Result<Option<Json>, FrontMatterLookupError>>,
    },
}

static RESOLVER_SENDER: LazyLock<Mutex<Option<std_mpsc::Sender<ResolverJob>>>> =
    LazyLock::new(|| Mutex::new(None));

fn ensure_resolver_worker() -> Result<std_mpsc::Sender<ResolverJob>, FrontMatterLookupError> {
    let mut guard = RESOLVER_SENDER.lock().unwrap();

    if let Some(tx) = &*guard {
        return Ok(tx.clone());
    }

    let (tx, rx) = std_mpsc::channel::<ResolverJob>();

    // Determine FM index directory once, at worker startup.
    let index_dir = fm_index_dir().ok_or(FrontMatterLookupError::NoIndexDir)?;

    thread::spawn(move || {
        // Dedicated Tokio runtime for IndexedJson I/O.
        let rt = tokio::runtime::Runtime::new().expect("failed to create resolver tokio runtime");

        // Open the IndexedJson archive once and keep it for the lifetime of the worker.
        let open_res: Result<IndexedJson<IndexRecord>, FrontMatterLookupError> =
            rt.block_on(async {
                IndexedJson::<IndexRecord>::open(&index_dir)
                    .await
                    .map_err(|e| FrontMatterLookupError::IndexedJson(e.into()))
            });

        let mut db = match open_res {
            Ok(db) => db,
            Err(err) => {
                eprintln!("Failed to open IndexedJson front-matter archive: {err}");
                // If we cannot open the index, just drain jobs and respond with errors.
                while let Ok(job) = rx.recv() {
                    let ResolverJob::LookupById { resp, .. } = job;
                    let _ = resp.send(Err(FrontMatterLookupError::NoIndexDir));
                }
                return;
            }
        };

        while let Ok(job) = rx.recv() {
            match job {
                ResolverJob::LookupById { id, resp } => {
                    let result: Result<Option<Json>, FrontMatterLookupError> = rt.block_on(async {
                        // Build a Query: id == <served path>
                        let field: std::sync::Arc<dyn IndexableField + Send + Sync> =
                            std::sync::Arc::new(StringField::new("id", id.clone()));

                        let q = Query::Eq(field);

                        // Run the query
                        let set = db
                            .query(&q)
                            .map_err(|e| FrontMatterLookupError::IndexedJson(e.into()))?;

                        // Take the first matching entry, if any.
                        let entry: Option<IndexEntry> = set.into_iter().next().cloned();

                        if let Some(entry) = entry {
                            // Fetch the corresponding record.
                            if let Ok(Some((_next, rec))) = db.get(entry).await {
                                let json = serde_json::to_value(rec)
                                    .map_err(FrontMatterLookupError::Serde)?;
                                Ok(Some(json))
                            } else {
                                Ok(None)
                            }
                        } else {
                            Ok(None)
                        }
                    });

                    let _ = resp.send(result);
                }
            }
        }
    });

    *guard = Some(tx.clone());
    Ok(tx)
}

/// Synchronous helper: look up front matter JSON by "served path" id.
///
/// - On any error, returns `Ok(json!({}))` (empty FM) so callers can
///   still proceed to serve content.
/// - Only hard failures (no index dir) are logged but not bubbled
///   out of the `ContentResolver` trait.
fn lookup_front_matter_by_id(id: &str) -> Json {
    let sender = match ensure_resolver_worker() {
        Ok(s) => s,
        Err(err) => {
            debug!("FM resolver worker not available: {err}");
            return json!({});
        }
    };

    let (tx, rx) = std_mpsc::channel::<Result<Option<Json>, FrontMatterLookupError>>();

    if let Err(err) = sender.send(ResolverJob::LookupById {
        id: id.to_string(),
        resp: tx,
    }) {
        debug!("Failed to send FM lookup job: {err}");
        return json!({});
    }

    match rx.recv() {
        Ok(Ok(Some(json))) => json,
        Ok(Ok(None)) => json!({}), // no entry for this id
        Ok(Err(err)) => {
            debug!("FM lookup error for id {id}: {err}");
            json!({})
        }
        Err(err) => {
            debug!("FM lookup response channel error for id {id}: {err}");
            json!({})
        }
    }
}

// ─────────────────────────────────────────────
// Path normalization helpers
// ─────────────────────────────────────────────

/// HTTP-level "served path" id used in IndexedJson.
///
/// This should match the ID we used when creating `IndexRecord`
/// (e.g. `/posts/hello.html`).
fn normalize_served_path_id(request_path: &str) -> String {
    let trimmed = request_path.trim_start_matches('/');

    if trimmed.is_empty() {
        return "/index.html".to_string();
    }

    if request_path.ends_with('/') {
        return format!("/{trimmed}index.html");
    }

    if trimmed.contains('.') {
        format!("/{trimmed}")
    } else {
        format!("/{trimmed}.html")
    }
}

/// Map a request path to a concrete on-disk file path under `root` and detect `ContentKind`.
///
/// Same behavior as the old FsContentResolver, but we no longer parse FM from disk.
fn normalize_body_path(root: &Path, path: &str) -> Option<(PathBuf, ContentKind)> {
    let trimmed = path.trim_start_matches('/');

    let mut candidates: Vec<(PathBuf, Option<ContentKind>)> = Vec::new();

    // 1: exact relative path
    if !trimmed.is_empty() {
        candidates.push((root.join(trimmed), None));
    }

    // 2: no extension → ".html"
    if !trimmed.is_empty() && !trimmed.contains('.') {
        candidates.push((
            root.join(format!("{trimmed}.html")),
            Some(ContentKind::Html),
        ));
    }

    // 3: "/" or ends with "/" → "index.html"
    if trimmed.is_empty() || path.ends_with('/') {
        let base = if trimmed.is_empty() {
            "index.html".to_string()
        } else {
            format!("{trimmed}index.html")
        };
        candidates.push((root.join(base), Some(ContentKind::Html)));
    }

    for (candidate, kind_hint) in candidates {
        if candidate.is_file() {
            let ck = kind_hint.unwrap_or_else(|| detect_content_kind_from_ext(&candidate));
            debug!("Resolved path '{}' → {:?}", path, candidate);
            return Some((candidate, ck));
        }
    }

    None
}

fn detect_content_kind_from_ext(path: &Path) -> ContentKind {
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        match ext.to_ascii_lowercase().as_str() {
            "html" | "htm" => ContentKind::Html,
            "json" => ContentKind::Json,
            _ => ContentKind::Asset,
        }
    } else {
        ContentKind::Asset
    }
}

// ─────────────────────────────────────────────
// IndexedJson-backed content resolver
// ─────────────────────────────────────────────

/// IndexedJson-based content resolver.
///
/// Responsibilities:
/// - Map HTTP request paths (e.g. `/`, `/about`, `/about.html`) to
///   concrete files under `root` (for the body).
/// - Compute the "served path" id (e.g. `/about.html`, `/index.html`).
/// - Look up front matter for that id from the `IndexedJson<IndexRecord>`
///   archive written by the document indexing pipeline.
/// - On miss or error, fall back to empty FM JSON `{}`.
#[derive(Clone, Debug)]
pub struct IndexedContentResolver {
    root: PathBuf,
}

impl IndexedContentResolver {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Internal helper used by both trait impl and `edge_resolve`.
    pub fn resolve_indexed(
        &self,
        path: &str,
        _method: &Method,
    ) -> Result<ResolvedContent, ContextError> {
        // 1. Normalize to body path (filesystem) and content kind.
        if let Some((body_path, content_kind)) = normalize_body_path(&self.root, path) {
            // 2. Compute served path id used by IndexedJson.
            let served_id = normalize_served_path_id(path);

            // 3. Look up front matter from IndexedJson archive.
            let front_matter = lookup_front_matter_by_id(&served_id);

            Ok(ResolvedContent {
                content_kind,
                front_matter,
                body_path,
            })
        } else {
            // No matching file: still run plugins/themes with empty FM.
            Ok(ResolvedContent {
                content_kind: ContentKind::Asset,
                front_matter: json!({}),
                body_path: PathBuf::new(),
            })
        }
    }
}

// Implement the trait used by serve/adapt.
impl ContentResolver for IndexedContentResolver {
    #[tracing::instrument(skip_all)]
    fn resolve(&self, path: &str, method: &Method) -> Result<ResolvedContent, ContextError> {
        self.resolve_indexed(path, method)
    }
}

// ─────────────────────────────────────────────
// Global resolver wiring for adapt/serve
// ─────────────────────────────────────────────

static GLOBAL_RESOLVER: LazyLock<Mutex<Option<IndexedContentResolver>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn init_global_indexed_resolver(resolver: IndexedContentResolver) {
    *GLOBAL_RESOLVER.lock().unwrap() = Some(resolver);
}

pub fn edge_resolve(path: &str, method: &Method) -> Result<ResolvedContent, ContextError> {
    let guard = GLOBAL_RESOLVER.lock().unwrap();
    let resolver = guard
        .as_ref()
        .expect("init_global_indexed_resolver must be called before edge_resolve");
    resolver.resolve_indexed(path, method)
}

// ─────────────────────────────────────────────
// RequestContext builder wiring
// ─────────────────────────────────────────────

pub fn edge_build_request_context(
    path: String,
    method: Method,
    headers: HeaderMap,
    query_params: HashMap<String, String>,
    resolved: ResolvedContent,
) -> RequestContext {
    // Normalize headers
    let mut hdr_obj = JsonMap::new();
    for (name, value) in headers.iter() {
        let key = canonicalize_header_name(name.as_str());
        hdr_obj.insert(key, json!(value.to_str().unwrap_or("")));
    }

    // Serialize params
    let mut qp_obj = JsonMap::new();
    for (k, v) in query_params {
        qp_obj.insert(k, Json::String(v));
    }

    RequestContext::builder()
        .path(Json::String(path))
        .method(Json::String(method.to_string()))
        .headers(Json::Object(hdr_obj))
        .params(Json::Object(qp_obj))
        // 🔑 This is now directly the JSON produced by IndexedJson (IndexRecord),
        // i.e. "front matter from the index".
        .content_meta(resolved.front_matter)
        .theme_config(json!({}))
        .plugin_configs(HashMap::new())
        .build()
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
