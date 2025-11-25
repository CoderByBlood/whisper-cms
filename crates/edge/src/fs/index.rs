// crates/edge/src/fs/doc.rs

// Fully aligned with serve::indexer signatures.
// No async in injected functions. No nested runtimes.
// Uses a dedicated worker thread + channel for async operations.

use crate::db::tantivy::{ContentIndex, ContentIndexError};
use crate::fs::scan::start_folder_scan;
use crate::proxy::EdgeError;

use adapt::mql::index::IndexRecord;
use anyhow::Error as AnyError;
use bytes::Bytes;
use domain::doc::BodyKind;
use futures::stream::{self, BoxStream};
use indexed_json::IndexedJson;
use serde_json::Value as Json;
use serve::indexer::{FolderScanConfig, ScanStopFn};
use std::io::Cursor;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{mpsc as std_mpsc, Arc, LazyLock, Mutex, RwLock},
    thread,
};
use thiserror::Error;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

// ======================================================================
// GLOBAL SINGLETON STATE
// ======================================================================

static FM_INDEX_DIR: LazyLock<RwLock<Option<PathBuf>>> = LazyLock::new(|| RwLock::new(None));

static CONTENT_INDEX_DIR: LazyLock<RwLock<Option<PathBuf>>> = LazyLock::new(|| RwLock::new(None));

static CONTENT_INDEX: LazyLock<RwLock<Option<Arc<ContentIndex>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Expose the FM index directory so other edge modules (e.g. resolver)
/// can open the IndexedJson archive.
pub fn fm_index_dir() -> Option<PathBuf> {
    FM_INDEX_DIR.read().ok().and_then(|guard| guard.clone())
}

// ======================================================================
// WORKER THREAD FOR INDEXING (avoid nested Tokio runtimes)
// ======================================================================

enum IndexJob {
    FrontMatter {
        served_path: PathBuf,
        fm: Json,
        resp: std_mpsc::Sender<Result<(), FrontMatterIndexError>>,
    },
    Body {
        served_path: PathBuf,
        html: String,
        resp: std_mpsc::Sender<Result<(), ContentBodyIndexError>>,
    },
}

static INDEX_SENDER: LazyLock<Mutex<Option<std_mpsc::Sender<IndexJob>>>> =
    LazyLock::new(|| Mutex::new(None));

fn spawn_index_worker() {
    let (tx, rx) = std_mpsc::channel::<IndexJob>();

    // Store tx globally
    *INDEX_SENDER.lock().unwrap() = Some(tx);

    thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            match job {
                IndexJob::FrontMatter {
                    served_path,
                    fm,
                    resp,
                } => {
                    let result = handle_fm_index(served_path, fm);
                    let _ = resp.send(result);
                }
                IndexJob::Body {
                    served_path,
                    html,
                    resp,
                } => {
                    let result = handle_body_index(served_path, html);
                    let _ = resp.send(result);
                }
            }
        }
    });
}

// ======================================================================
// INIT FUNCTIONS CALLED BY CLI
// ======================================================================

pub fn set_fm_index_dir(p: PathBuf) {
    {
        let mut w = FM_INDEX_DIR.write().unwrap();
        *w = Some(p.clone());
    }
    // cascade additional depdencies
    domain::doc::inject_open_bytes_fn(open_bytes);
    domain::doc::inject_open_utf8_fn(open_utf8);
    set_content_index_dir(p);
}

pub fn set_content_index_dir(p: PathBuf) {
    let index =
        ContentIndex::open_or_create(&p, 15_000_000).expect("Failed to open/create Tantivy index");

    {
        let mut d = CONTENT_INDEX_DIR.write().unwrap();
        *d = Some(p);
    }

    {
        let mut w = CONTENT_INDEX.write().unwrap();
        *w = Some(Arc::new(index));
    }

    // spawn index worker if not already spawned
    if INDEX_SENDER.lock().unwrap().is_none() {
        spawn_index_worker();
    }
}

// ======================================================================
// FILE STREAM HELPERS
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
// 1. FOLDER SCAN ADAPTER — matches serve::indexer signature
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
// 2. FRONT MATTER INDEXER (sync fn)
// ======================================================================

pub fn index_front_matter(served_path: &Path, fm: &Json) -> Result<(), FrontMatterIndexError> {
    let sender = INDEX_SENDER
        .lock()
        .unwrap()
        .clone()
        .expect("Index worker not started");

    let (tx, rx) = std_mpsc::channel();

    sender
        .send(IndexJob::FrontMatter {
            served_path: served_path.to_path_buf(),
            fm: fm.clone(),
            resp: tx,
        })
        .unwrap();

    rx.recv().unwrap()
}

// worker handler
fn handle_fm_index(served_path: PathBuf, fm: Json) -> Result<(), FrontMatterIndexError> {
    let index_dir = FM_INDEX_DIR
        .read()
        .unwrap()
        .clone()
        .expect("FM index dir not set");

    fs::create_dir_all(&index_dir)?;

    let id = served_path.to_string_lossy().to_string();
    let record = IndexRecord::from_json_with_id(id, &fm);

    // run async IndexedJson inside a local runtime
    let rt = tokio::runtime::Runtime::new().unwrap();
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

// ======================================================================
// 3. BODY → TANTIVY INDEXER (sync fn)
// ======================================================================

pub fn index_body(
    served_path: &Path,
    html: &str,
    _kind: BodyKind,
) -> Result<(), ContentBodyIndexError> {
    let sender = INDEX_SENDER
        .lock()
        .unwrap()
        .clone()
        .expect("Index worker not started");

    let (tx, rx) = std_mpsc::channel();

    sender
        .send(IndexJob::Body {
            served_path: served_path.to_path_buf(),
            html: html.to_owned(),
            resp: tx,
        })
        .unwrap();

    rx.recv().unwrap()
}

fn handle_body_index(served_path: PathBuf, html: String) -> Result<(), ContentBodyIndexError> {
    let index_arc = CONTENT_INDEX
        .read()
        .unwrap()
        .clone()
        .expect("CONTENT_INDEX not set");

    let mut cursor = Cursor::new(html.into_bytes());
    index_arc.add(&served_path, &mut cursor)?;
    Ok(())
}
