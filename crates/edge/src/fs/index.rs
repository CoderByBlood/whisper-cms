// crates/edge/src/fs/index.rs

// Edge-side indexing + stream injection:
//
// - Wires domain::doc's open_* for filesystem-backed Documents.
// - Wires domain::stream's open_* for CAS-backed ResolvedContent bodies.
// - Owns a single IndexedJson<IndexRecord> writer worker (Tokio runtime on
//   a dedicated thread) for front-matter.
// - Owns a global Tantivy ContentIndex for rendered HTML.
// - Exposes start_scan / index_front_matter / index_body with signatures
//   expected by serve::indexer.

use crate::db::tantivy::{ContentIndex, ContentIndexError};
use crate::fs::scan::start_folder_scan;
use crate::proxy::EdgeError;

use adapt::mql::index::IndexRecord;
use anyhow::Error as AnyError;
use bytes::Bytes;
use domain::doc::BodyKind;
use domain::doc::{inject_open_bytes_fn, inject_open_utf8_fn};
use domain::stream::{
    inject_open_bytes_from_handle_fn, inject_open_utf8_from_handle_fn, BytesStream, StreamHandle,
    Utf8Stream,
};
use futures::stream::{self, BoxStream};
use indexed_json::IndexedJson;
use serde_json::Value as Json;
use serve::indexer::{FolderScanConfig, ScanStopFn};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{mpsc as std_mpsc, Arc, LazyLock, RwLock};
use std::{fs, io, thread};
use thiserror::Error;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tracing::debug;

// ======================================================================
// GLOBAL SINGLETON STATE
// ======================================================================

/// Directory used for storing IndexedJson front-matter archive.
static FM_INDEX_DIR: LazyLock<RwLock<Option<PathBuf>>> = LazyLock::new(|| RwLock::new(None));

/// Root folder for *source* content (e.g. `/…/site/content`).
///
/// We strip this prefix from absolute filesystem paths to derive the
/// HTTP-style served ID, like `/index.html` or `/docs/search.html`.
static CONTENT_ROOT: LazyLock<RwLock<Option<PathBuf>>> = LazyLock::new(|| RwLock::new(None));

/// Global Tantivy content index (rendered HTML).
static CONTENT_INDEX: LazyLock<RwLock<Option<Arc<ContentIndex>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Sender into the IndexedJson worker thread.
static INDEX_WORKER_SENDER: LazyLock<RwLock<Option<std_mpsc::Sender<IndexJob>>>> =
    LazyLock::new(|| RwLock::new(None));

// ======================================================================
// FILESYSTEM STREAM HELPERS (for Document)
// ======================================================================

enum ReadState {
    Opening(PathBuf),
    Reading(File),
    Done,
}

fn open_bytes(path: &PathBuf) -> BoxStream<'static, io::Result<Bytes>> {
    let state = ReadState::Opening(path.clone());

    let s = stream::unfold(state, |state| async move {
        match state {
            ReadState::Opening(path) => match File::open(path).await {
                Err(e) => Some((Err(e), ReadState::Done)),
                Ok(f) => read_next_chunk(f).await,
            },

            ReadState::Reading(file) => read_next_chunk(file).await,

            ReadState::Done => None,
        }
    });

    Box::pin(s)
}

fn open_utf8(path: &PathBuf) -> BoxStream<'static, io::Result<String>> {
    let path = path.clone();
    let s = stream::once(async move {
        let mut file = File::open(path).await?;
        let mut s = String::new();
        file.read_to_string(&mut s).await?;
        Ok(s)
    });
    Box::pin(s)
}

async fn read_next_chunk(mut file: File) -> Option<(io::Result<Bytes>, ReadState)> {
    let mut buf = vec![0; 8 * 1024];
    match file.read(&mut buf).await {
        Ok(0) => None,
        Ok(n) => {
            buf.truncate(n);
            Some((Ok(Bytes::from(buf)), ReadState::Reading(file)))
        }
        Err(e) => Some((Err(e), ReadState::Done)),
    }
}

// ======================================================================
// ERRORS
// ======================================================================

#[derive(Debug, Error)]
pub enum FrontMatterIndexError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("IndexedJson: {0}")]
    IndexedJson(#[source] AnyError),
}

#[derive(Debug, Error)]
pub enum ContentBodyIndexError {
    #[error("Tantivy: {0}")]
    Tantivy(#[from] ContentIndexError),
}

// ======================================================================
// INDEX WORKER (IndexedJson front-matter)
// ======================================================================

enum IndexJob {
    FrontMatter {
        /// Absolute *source* path from the scanner.
        served_path: PathBuf,
        fm: Json,
        resp: std_mpsc::Sender<Result<(), FrontMatterIndexError>>,
    },
    GetFrontMatterByPath {
        /// HTTP-style served path, e.g. `/index.html`.
        served_path: PathBuf,
        resp: std_mpsc::Sender<Result<Option<Json>, FrontMatterIndexError>>,
    },
    GetFrontMatterBySlug {
        slug: String,
        resp: std_mpsc::Sender<Result<Option<Json>, FrontMatterIndexError>>,
    },
}

/// Ensure the IndexedJson worker thread is running.
///
/// The worker owns a single current-thread Tokio runtime and processes
/// jobs sequentially. This avoids nested runtimes and keeps the
/// indexer APIs synchronous at the edge layer.
fn ensure_index_worker() {
    let mut guard = INDEX_WORKER_SENDER
        .write()
        .expect("INDEX_WORKER_SENDER RwLock poisoned");

    if guard.is_some() {
        return;
    }

    let (tx, rx) = std_mpsc::channel::<IndexJob>();
    *guard = Some(tx);

    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build IndexedJson worker runtime");

        while let Ok(job) = rx.recv() {
            match job {
                IndexJob::FrontMatter {
                    served_path,
                    fm,
                    resp,
                } => {
                    let result = handle_fm_index(&rt, served_path, fm);
                    let _ = resp.send(result);
                }
                IndexJob::GetFrontMatterByPath { served_path, resp } => {
                    let result = handle_get_front_matter_by_path(&rt, served_path);
                    let _ = resp.send(result);
                }
                IndexJob::GetFrontMatterBySlug { slug, resp } => {
                    let result = handle_get_front_matter_by_slug(&rt, slug);
                    let _ = resp.send(result);
                }
            }
        }
    });
}

// ======================================================================
// ID NORMALIZATION
// ======================================================================

/// Canonicalize an absolute *source* path into an HTTP-style ID.
///
/// Example:
///   CONTENT_ROOT = "/…/docsy-gitlab/content"
///   src          = "/…/docsy-gitlab/content/index.html"
///   -> "/index.html"
fn canonical_id_from_source(path: &Path) -> String {
    let rel = {
        let guard = CONTENT_ROOT.read().expect("CONTENT_ROOT RwLock poisoned");
        if let Some(root) = &*guard {
            path.strip_prefix(root).unwrap_or(path).to_owned()
        } else {
            path.to_owned()
        }
    };

    let s = rel.to_string_lossy();
    if s.starts_with('/') {
        s.into_owned()
    } else {
        format!("/{}", s)
    }
}

/// Canonicalize a served path coming from the resolver.
///
/// Example:
///   path = "index.html"   -> "/index.html"
///   path = "/index.html"  -> "/index.html"
fn canonical_id_from_served(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.starts_with('/') {
        s.into_owned()
    } else {
        format!("/{}", s)
    }
}

// ======================================================================
// IndexedJson handlers
// ======================================================================

fn handle_fm_index(
    rt: &tokio::runtime::Runtime,
    served_path: PathBuf,
    fm: Json,
) -> Result<(), FrontMatterIndexError> {
    let index_dir = FM_INDEX_DIR
        .read()
        .expect("FM_INDEX_DIR RwLock poisoned")
        .clone()
        .expect("FM index dir not set");

    fs::create_dir_all(&index_dir)?;

    // IMPORTANT: use canonical *served* ID, not absolute FS path.
    let id = canonical_id_from_source(&served_path);
    let mut record = IndexRecord::from_json_with_id(id, &fm);

    // If your IndexRecord has a slug field and the FM has `slug`,
    // you can optionally hydrate it here (keeps lookup_by_slug fast).
    if record.slug.is_none() {
        if let Some(slug_val) = fm
            .get("slug")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        {
            record.slug = Some(slug_val);
        }
    }

    rt.block_on(async {
        let mut db = IndexedJson::<IndexRecord>::open(&index_dir)
            .await
            .map_err(FrontMatterIndexError::IndexedJson)?;

        db.append(&record)
            .await
            .map_err(FrontMatterIndexError::IndexedJson)?;

        db.flush().await.map_err(FrontMatterIndexError::IndexedJson)
    })
}

/// Linear scan by `record.id == served_path` (served-path form).
///
/// We serialize the IndexRecord back to JSON to use as front_matter.
/// This is the *projection* shape, not necessarily the original FM.
fn handle_get_front_matter_by_path(
    rt: &tokio::runtime::Runtime,
    served_path: PathBuf,
) -> Result<Option<Json>, FrontMatterIndexError> {
    let index_dir = FM_INDEX_DIR
        .read()
        .expect("FM_INDEX_DIR RwLock poisoned")
        .clone()
        .expect("FM index dir not set");

    let target = canonical_id_from_served(&served_path);

    rt.block_on(async {
        let mut db = IndexedJson::<IndexRecord>::open(&index_dir)
            .await
            .map_err(FrontMatterIndexError::IndexedJson)?;

        let mut current = match db.first() {
            None => return Ok(None),
            Some(entry) => entry,
        };

        loop {
            match db.get(current).await {
                Ok(Some((next, rec))) => {
                    if rec.id == target {
                        let json = serde_json::to_value(rec)
                            .map_err(|e| FrontMatterIndexError::IndexedJson(e.into()))?;
                        return Ok(Some(json));
                    }
                    current = next;
                }
                Ok(None) => return Ok(None),
                Err(e) => return Err(FrontMatterIndexError::IndexedJson(e.into())),
            }
        }
    })
}

fn handle_get_front_matter_by_slug(
    rt: &tokio::runtime::Runtime,
    slug: String,
) -> Result<Option<Json>, FrontMatterIndexError> {
    let index_dir = FM_INDEX_DIR
        .read()
        .expect("FM_INDEX_DIR RwLock poisoned")
        .clone()
        .expect("FM index dir not set");

    rt.block_on(async {
        let mut db = IndexedJson::<IndexRecord>::open(&index_dir)
            .await
            .map_err(FrontMatterIndexError::IndexedJson)?;

        let mut current = match db.first() {
            None => return Ok(None),
            Some(entry) => entry,
        };

        loop {
            match db.get(current).await {
                Ok(Some((next, rec))) => {
                    // rec.slug must exist and equal the requested slug
                    match &rec.slug {
                        Some(s) if s == &slug => {
                            let json = serde_json::to_value(rec)
                                .map_err(|e| FrontMatterIndexError::IndexedJson(e.into()))?;
                            return Ok(Some(json));
                        }
                        _ => {}
                    }
                    current = next;
                }
                Ok(None) => return Ok(None),
                Err(e) => return Err(FrontMatterIndexError::IndexedJson(e.into())),
            }
        }
    })
}

// ======================================================================
// STREAM HANDLE INJECTION (CAS via Tantivy ContentIndex)
// ======================================================================

fn cas_bytes_from_handle(handle: &StreamHandle) -> BytesStream {
    use futures::stream::once;

    // Take an OWNED key so the async block doesn't borrow `handle`.
    let id = handle
        .as_cas_key()
        .expect("Constraint violated - handle created with a key")
        .to_owned();

    // Clone the Arc<ContentIndex> out of the global, also owned by the future.
    let index_opt = {
        CONTENT_INDEX
            .read()
            .expect("CONTENT_INDEX RwLock poisoned")
            .clone()
    };

    let fut = async move {
        let index = match index_opt {
            Some(idx) => idx,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Content index not initialized",
                ))
            }
        };

        let path = PathBuf::from(id);
        let mut cursor = index
            .get(&path)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let mut buf = Vec::new();
        // Disambiguate the trait method.
        std::io::Read::read_to_end(&mut cursor, &mut buf)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        Ok(Bytes::from(buf))
    };

    Box::pin(once(fut))
}

fn cas_utf8_from_handle(handle: &StreamHandle) -> Utf8Stream {
    use futures::stream::once;

    // Take an OWNED key so the async block doesn't borrow `handle`.
    let id = handle
        .as_cas_key()
        .expect("Constraint violated - handle was created without a key")
        .to_owned();

    // Clone the Arc<ContentIndex> out of the global, also owned by the future.
    let index_opt = {
        CONTENT_INDEX
            .read()
            .expect("CONTENT_INDEX RwLock poisoned")
            .clone()
    };

    let fut = async move {
        let index = match index_opt {
            Some(idx) => idx,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Content index not initialized",
                ))
            }
        };

        let path = PathBuf::from(id);
        let mut cursor = index
            .get(&path)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let mut buf = String::new();
        // Disambiguate the trait method.
        std::io::Read::read_to_string(&mut cursor, &mut buf)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        Ok(buf)
    };

    Box::pin(once(fut))
}

// ======================================================================
// PUBLIC INIT: called once from CLI before scanning
// ======================================================================

/// Set the *content root* used to compute served IDs.
///
/// Example:
///   root = "/…/docsy-gitlab/content"
/// This is used only to normalize absolute paths coming from the scanner.
pub fn set_content_root(root: PathBuf) {
    let mut w = CONTENT_ROOT.write().expect("CONTENT_ROOT RwLock poisoned");
    *w = Some(root);
}

/// Injects:
/// - Document FS stream functions (domain::doc::*).
/// - CAS stream functions (domain::stream::*).
/// - Front-matter index directory.
/// - ContentIndex (Tantivy) instance.
/// - IndexedJson worker thread.
pub fn set_fm_index_dir(p: PathBuf) {
    {
        let mut w = FM_INDEX_DIR.write().expect("FM_INDEX_DIR RwLock poisoned");
        *w = Some(p.clone());
    }

    // Inject filesystem-based Document streams.
    inject_open_bytes_fn(open_bytes);
    inject_open_utf8_fn(open_utf8);

    // Initialize Tantivy content index.
    let index =
        ContentIndex::open_or_create(&p, 15_000_000).expect("Failed to open/create Tantivy index");
    {
        let mut w = CONTENT_INDEX
            .write()
            .expect("CONTENT_INDEX RwLock poisoned");
        *w = Some(Arc::new(index));
    }

    // Inject CAS-based stream handle readers.
    inject_open_bytes_from_handle_fn(cas_bytes_from_handle);
    inject_open_utf8_from_handle_fn(cas_utf8_from_handle);

    // Ensure the IndexedJson worker is running.
    ensure_index_worker();
}

// ======================================================================
// 1. FOLDER SCAN ADAPTER — matches serve::indexer::StartFolderScanFn
// ======================================================================

pub fn start_scan(
    root: &Path,
    cfg: &FolderScanConfig,
) -> Result<(mpsc::Receiver<PathBuf>, ScanStopFn), EdgeError> {
    let (tx, rx) = mpsc::channel(cfg.channel_capacity);
    let stop = start_folder_scan(root, cfg.clone(), tx)?;
    let stop_fn: ScanStopFn = Box::new(move || stop());
    Ok((rx, stop_fn))
}

// ======================================================================
// 2. FRONT MATTER INDEXER (sync) — matches IndexFrontMatterFn
// ======================================================================

#[tracing::instrument(skip_all)]
pub fn index_front_matter(served_path: &Path, fm: &Json) -> Result<(), FrontMatterIndexError> {
    ensure_index_worker();

    debug!("Indexing front matter for {}", served_path.display());

    let sender = INDEX_WORKER_SENDER
        .read()
        .expect("INDEX_WORKER_SENDER RwLock poisoned")
        .clone()
        .expect("Index worker not started");

    let (tx, rx) = std_mpsc::channel();

    sender
        .send(IndexJob::FrontMatter {
            served_path: served_path.to_path_buf(),
            fm: fm.clone(),
            resp: tx,
        })
        .expect("failed to send IndexJob::FrontMatter");

    rx.recv().expect("Index worker dropped response channel") // propagate result
}

/// Public helper used by the resolver to load front matter by served path.
///
/// `served_path` here is already HTTP-style (e.g. `/index.html`).
/// Returns Ok(None) if the id is not present in the index.
pub fn lookup_front_matter_by_path(
    served_path: &Path,
) -> Result<Option<Json>, FrontMatterIndexError> {
    ensure_index_worker();

    let sender = INDEX_WORKER_SENDER
        .read()
        .expect("INDEX_WORKER_SENDER RwLock poisoned")
        .clone()
        .expect("Index worker not started");

    let (tx, rx) = std_mpsc::channel();

    sender
        .send(IndexJob::GetFrontMatterByPath {
            served_path: served_path.to_path_buf(),
            resp: tx,
        })
        .expect("failed to send IndexJob::GetFrontMatterByPath");

    rx.recv().expect("Index worker dropped response channel") // propagate result
}

/// Public helper used by the resolver to load front matter by **slug**.
///
/// This scans the IndexedJson archive for a record whose `slug` field
/// matches the provided slug. Returns Ok(None) if not found.
pub fn lookup_front_matter_by_slug(slug: &str) -> Result<Option<Json>, FrontMatterIndexError> {
    ensure_index_worker();

    let sender = INDEX_WORKER_SENDER
        .read()
        .expect("INDEX_WORKER_SENDER RwLock poisoned")
        .clone()
        .expect("Index worker not started");

    let slug = slug.to_string();
    let (tx, rx) = std_mpsc::channel();

    sender
        .send(IndexJob::GetFrontMatterBySlug { slug, resp: tx })
        .expect("failed to send IndexJob::GetFrontMatterBySlug");

    rx.recv().expect("Index worker dropped response channel") // propagate result
}

// ======================================================================
// 3. BODY → TANTIVY INDEXER (sync) — matches IndexBodyFn
// ======================================================================

pub fn index_body(
    served_path: &Path,
    html: &str,
    _kind: BodyKind,
) -> Result<(), ContentBodyIndexError> {
    let index_arc = CONTENT_INDEX
        .read()
        .expect("CONTENT_INDEX RwLock poisoned")
        .clone()
        .expect("CONTENT_INDEX not set");

    // Canonicalize to served ID so CAS lookups by HTTP path work.
    let id = canonical_id_from_source(served_path);
    let mut cursor = Cursor::new(html.as_bytes().to_vec());
    index_arc.add(Path::new(&id), &mut cursor)?;
    Ok(())
}
