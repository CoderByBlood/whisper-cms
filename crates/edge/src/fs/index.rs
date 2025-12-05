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

use crate::db::tantivy::ContentIndex;
use crate::fs::scan::start_folder_scan;
use crate::Error;
use adapt::mql::index::IndexRecord;
use async_trait::async_trait;
use domain::doc::BodyKind;
use indexed_json::IndexedJson;
use serde_json::Value as Json;
use serve::indexer::{ContentManager, FolderScanConfig, ScanStopFn};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use tokio::sync::{mpsc, RwLock};

static CAS: LazyLock<RwLock<Option<ContentIndex>>> = LazyLock::new(|| RwLock::new(None));

static INDEX: LazyLock<RwLock<Option<IndexedJson<IndexRecord>>>> =
    LazyLock::new(|| RwLock::new(None));

pub async fn set_cas_index(index_dir: PathBuf) -> Result<(), Error> {
    let cas = ContentIndex::open_or_create(&index_dir, 15_000_000)
        .expect("Failed to open/create Tantivy index");
    {
        let mut c = CAS.write().await;
        *c = Some(cas);
    }
    let index = IndexedJson::<IndexRecord>::open(&index_dir)
        .await
        .map_err(Error::IndexedJson)?;

    {
        let mut i = INDEX.write().await;
        *i = Some(index);
    }

    Ok(())
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
fn canonical_id_from_source(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path).to_owned();
    let s = rel.to_string_lossy();

    if s.starts_with('/') {
        s.into_owned()
    } else {
        format!("/{}", s)
    }
}

// ======================================================================
// 1. FOLDER SCAN ADAPTER — matches serve::indexer::StartFolderScanFn
// ======================================================================

pub fn start_scan(
    root: &Path,
    cfg: &FolderScanConfig,
) -> Result<(mpsc::Receiver<PathBuf>, ScanStopFn), Error> {
    let (tx, rx) = mpsc::channel(cfg.channel_capacity);
    let stop = start_folder_scan(root, cfg.clone(), tx)?;
    let stop_fn: ScanStopFn = Box::new(move || stop());
    Ok((rx, stop_fn))
}

// ======================================================================
// 2. FRONT MATTER INDEXER (sync) — matches IndexFrontMatterFn
// ======================================================================

#[tracing::instrument(skip_all)]
pub async fn index_front_matter(root: &Path, served_path: &Path, fm: &Json) -> Result<(), Error> {
    // IMPORTANT: use canonical *served* ID, not absolute FS path.
    let id = canonical_id_from_source(root, served_path);
    let mut record = IndexRecord::from_json_with_id(id, &fm);

    // Optionally hydrate slug from FM if not already set.
    if record.slug.is_none() {
        if let Some(slug_val) = fm
            .get("slug")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        {
            record.slug = Some(slug_val);
        }
    }

    if let Some(db) = INDEX.write().await.as_mut() {
        db.append(&record).await.map_err(Error::IndexedJson)?;

        db.flush().await.map_err(Error::IndexedJson)
    } else {
        Err(Error::NoIndex("No Database".into()))
    }
}

/// Public helper used by the resolver to load front matter by served path.
///
/// `served_path` here is already HTTP-style (e.g. `/index.html`).
/// Returns Ok(None) if the id is not present in the index.
pub async fn lookup_front_matter_by_path(served_path: &Path) -> Result<Option<Json>, Error> {
    if let Some(db) = INDEX.write().await.as_mut() {
        let mut current = match db.first() {
            Some(entry) => entry,
            None => return Ok(None),
        };

        loop {
            match db.get(current).await {
                Ok(Some((next, rec))) => {
                    // NOTE: If IndexRecord.id is a String, you may need to
                    // compare to served_path.to_string_lossy().
                    if rec.id == served_path.to_path_buf() {
                        let json =
                            serde_json::to_value(rec).map_err(|e| Error::IndexedJson(e.into()))?;
                        return Ok(Some(json));
                    }
                    current = next;
                }
                Ok(None) => return Ok(None),
                Err(e) => return Err(Error::IndexedJson(e.into())),
            }
        }
    } else {
        Err(Error::NoIndex("No Database".into()))
    }
}

/// Public helper used by the resolver to load front matter by **slug**.
///
/// This scans the IndexedJson archive for a record whose `slug` field
/// matches the provided slug. Returns Ok(None) if not found.
pub async fn lookup_front_matter_by_slug(slug: &str) -> Result<Option<Json>, Error> {
    if let Some(db) = INDEX.write().await.as_mut() {
        let mut current = match db.first() {
            None => return Ok(None),
            Some(entry) => entry,
        };

        loop {
            match db.get(current).await {
                Ok(Some((next, rec))) => {
                    if let Some(s) = &rec.slug {
                        if s == &slug {
                            let json = serde_json::to_value(rec)
                                .map_err(|e| Error::IndexedJson(e.into()))?;
                            return Ok(Some(json));
                        }
                    }
                    current = next;
                }
                Ok(None) => return Ok(None),
                Err(e) => return Err(Error::IndexedJson(e.into())),
            }
        }
    } else {
        Err(Error::NoIndex("No Database".into()))
    }
}

pub async fn lookup_body(key: &str) -> Result<Option<Arc<String>>, Error> {
    if let Some(cas) = CAS.write().await.as_mut() {
        let cursor = cas.get(Path::new(key))?;
        let bytes = cursor.into_inner(); // take ownership of the Vec<u8>
        Ok(Some(Arc::new(String::from_utf8(bytes)?)))
    } else {
        Err(Error::NoCas("No Database".into()))
    }
}

// ======================================================================
// 3. BODY → TANTIVY INDEXER (sync) — matches IndexBodyFn
// ======================================================================

pub async fn index_body(
    root: &Path,
    served_path: &Path,
    html: &str,
    _kind: BodyKind,
) -> Result<(), Error> {
    if let Some(cas) = CAS.write().await.as_mut() {
        // Canonicalize to served ID so CAS lookups by HTTP path work.
        let id = canonical_id_from_source(root, served_path);
        let mut cursor = Cursor::new(html.as_bytes().to_vec());
        cas.add(Path::new(&id), &mut cursor)?;
        Ok(())
    } else {
        Err(Error::NoCas("No Cas".into()))
    }
}

#[derive(Debug, Clone)]
pub struct ContentMgr {
    root: PathBuf,
}

impl ContentMgr {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}
#[async_trait]
impl ContentManager for ContentMgr {
    async fn scan_file(&self, path: &Path) -> Result<String, serve::Error> {
        let bytes = fs::read(path)?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    async fn scan_folder(
        &self,
        root: &Path,
        cfg: &FolderScanConfig,
    ) -> Result<(mpsc::Receiver<PathBuf>, ScanStopFn), serve::Error> {
        start_scan(root, cfg).map_err(|e| serve::Error::Scan(e.to_string()))
    }

    async fn index_front_matter(&self, served_path: &Path, fm: &Json) -> Result<(), serve::Error> {
        index_front_matter(self.root.as_path(), served_path, fm)
            .await
            .map_err(|e| serve::Error::FrontMatterIndex(e.to_string()))
    }

    async fn index_body(
        &self,
        served_path: &Path,
        html: &str,
        kind: BodyKind,
    ) -> Result<(), serve::Error> {
        index_body(self.root.as_path(), served_path, html, kind)
            .await
            .map_err(|e| serve::Error::ContentIndex(e.to_string()))
    }

    async fn lookup_slug(&self, slug: &str) -> Result<Option<Json>, serve::Error> {
        lookup_front_matter_by_slug(slug)
            .await
            .map_err(|e| serve::Error::Backend(e.to_string()))
    }

    async fn lookup_served(&self, served: &str) -> Result<Option<Json>, serve::Error> {
        lookup_front_matter_by_path(Path::new(served))
            .await
            .map_err(|e| serve::Error::Backend(e.to_string()))
    }

    async fn lookup_body(&self, key: &str) -> Result<Option<Arc<String>>, serve::Error> {
        lookup_body(key)
            .await
            .map_err(|e| serve::Error::Backend(e.to_string()))
    }
}
