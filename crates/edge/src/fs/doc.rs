// crates/edge/src/fs/doc.rs
//
// Fully-aligned with serve::indexer function pointer signatures.
// No async in injected functions.
// All state stored in LazyLock singletons.
// No extraneous parameters.
// No &ContentIndex or fm_index_dir passed in.

use crate::db::tantivy::{ContentIndex, ContentIndexError};
use crate::fs::scan::start_folder_scan;
use crate::proxy::EdgeError;
use adapt::mql::index::IndexRecord;
use anyhow::Error as AnyError;
use domain::doc::BodyKind;
use indexed_json::IndexedJson;
use serde_json::Value as Json;
use serve::indexer::{FolderScanConfig, ScanStopFn};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use thiserror::Error;
use tokio::sync::mpsc;

// ======================================================================
// Global Singletons — Injected by CLI before scanning begins
// ======================================================================

/// Directory used for storing IndexedJson front-matter.
static FM_INDEX_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    panic!("FM_INDEX_DIR not set. Call set_fm_index_dir() before scanning.");
});

/// Tantivy content index — required for body HTML indexing.
static CONTENT_INDEX: LazyLock<std::sync::RwLock<Arc<ContentIndex>>> =
    LazyLock::new(|| panic!("CONTENT_INDEX not set. Call set_content_index() before scanning."));

/// Inject front-matter index directory.
pub fn set_fm_index_dir(p: PathBuf) {
    // Safety: LazyLock allows mutation BEFORE first read;
    // subsequent writes before first use are ok.
    LazyLock::force(&FM_INDEX_DIR).clone_from(&&p);
    let content_index = ContentIndex::open_or_create(p, 15_000_000);

    match content_index {
        Ok(index) => {
            LazyLock::force(&CONTENT_INDEX).clone_from(&&RwLock::from(Arc::new(index)));
        }
        Err(err) => {
            panic!("Failed to open or create content index: {}", err);
        }
    }
}

// ======================================================================
// Error Types
// ======================================================================

#[derive(Debug, Error)]
pub enum FrontMatterIndexError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("indexed_json error: {0}")]
    IndexedJson(#[source] AnyError),
}

#[derive(Debug, Error)]
pub enum ContentBodyIndexError {
    #[error("Tantivy error: {0}")]
    Tantivy(#[from] ContentIndexError),
}

// ======================================================================
// 1. Folder Scan Adapter — matches StartFolderScanFn exactly
// ======================================================================

pub fn edge_start_scan(
    root: &Path,
    cfg: &FolderScanConfig,
) -> Result<(mpsc::Receiver<PathBuf>, ScanStopFn), EdgeError> {
    let (tx, rx) = mpsc::channel::<PathBuf>(cfg.channel_capacity);

    let stop_handle = start_folder_scan(root, cfg.clone(), tx)?;

    let stop_fn: ScanStopFn = Box::new(move || {
        stop_handle();
    });

    Ok((rx, stop_fn))
}

// ======================================================================
// 2. IndexedJson Front-Matter Indexer — matches IndexFrontMatterFn exactly
// ======================================================================

pub fn edge_index_front_matter(served_path: &Path, fm: &Json) -> Result<(), FrontMatterIndexError> {
    let index_dir = &*FM_INDEX_DIR;

    fs::create_dir_all(index_dir)?;

    let id = served_path.to_string_lossy().to_string();
    let record = IndexRecord::from_json_with_id(id, fm);

    // Open DB
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(async {
        let mut db = IndexedJson::<IndexRecord>::open(index_dir)
            .await
            .map_err(FrontMatterIndexError::IndexedJson)?;

        db.append(&record)
            .await
            .map_err(FrontMatterIndexError::IndexedJson)?;

        db.flush().await.map_err(FrontMatterIndexError::IndexedJson)
    });

    result
}

// ======================================================================
// 3. Body → Tantivy Indexer — matches IndexBodyFn exactly
// ======================================================================

pub fn edge_index_body(
    served_path: &Path,
    html: &str,
    _kind: BodyKind,
) -> Result<(), ContentBodyIndexError> {
    // Turn HTML into a reader
    let mut cursor = Cursor::new(html.as_bytes().to_vec());

    // Take a read lock on the global ContentIndex
    let index_guard = CONTENT_INDEX.read().expect("CONTENT_INDEX RwLock poisoned");

    // `index_guard` is a RwLockReadGuard<Arc<ContentIndex>>
    // Method-call deref coercion means this calls ContentIndex::add(...)
    index_guard.add(served_path, &mut cursor)?;
    Ok(())
}
