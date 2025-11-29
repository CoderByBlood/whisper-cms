// crates/domain/src/doc.rs

use bytes::Bytes;
use futures::stream::BoxStream;
use std::io;
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};
use std::time::SystemTime;

// ─────────────────────────────────────────────────────────────────────────────
// Injection points (GLOBAL function pointers) for filesystem documents
// ─────────────────────────────────────────────────────────────────────────────

/// Stream of raw bytes from a filesystem document.
pub type OpenBytesFn = fn(&PathBuf) -> BoxStream<'static, io::Result<Bytes>>;

/// Stream of UTF-8 text from a filesystem document.
pub type OpenUtf8Fn = fn(&PathBuf) -> BoxStream<'static, io::Result<String>>;

/// Default panic implementations (force early detect if not injected).
fn missing_open_bytes(_path: &PathBuf) -> BoxStream<'static, io::Result<Bytes>> {
    panic!("Document::open_bytes_fn not injected");
}

fn missing_open_utf8(_path: &PathBuf) -> BoxStream<'static, io::Result<String>> {
    panic!("Document::open_utf8_fn not injected");
}

/// GLOBAL injection targets (set once at startup).
pub static OPEN_BYTES_FN: LazyLock<RwLock<Option<OpenBytesFn>>> =
    LazyLock::new(|| RwLock::new(Some(missing_open_bytes)));

pub static OPEN_UTF8_FN: LazyLock<RwLock<Option<OpenUtf8Fn>>> =
    LazyLock::new(|| RwLock::new(Some(missing_open_utf8)));

/// API the edge crate calls *once* during initialization.
pub fn inject_open_bytes_fn(f: OpenBytesFn) {
    let mut open_bytes_fn = OPEN_BYTES_FN
        .write()
        .expect("OPEN_BYTES_FN RwLock poisoned");
    *open_bytes_fn = Some(f);
}

pub fn inject_open_utf8_fn(f: OpenUtf8Fn) {
    let mut open_utf8_fn = OPEN_UTF8_FN.write().expect("OPEN_UTF8_FN RwLock poisoned");
    *open_utf8_fn = Some(f);
}

// ─────────────────────────────────────────────────────────────────────────────
// Document data types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmKind {
    Toml,
    Yaml,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Markdown,
    AsciiDoc,
    Html,
    Plain,
    ReStructuredText,
    OrgMode,
}

// ─────────────────────────────────────────────────────────────────────────────
// Document struct
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Document {
    /// Filesystem path to the source document.
    pub path: PathBuf,
    /// Size in bytes, if known.
    pub size: Option<u64>,
    /// Last modified time, if known.
    pub mtime: Option<SystemTime>,

    /// Cached full UTF-8 contents of the file (optional).
    pub cache: Option<String>,
    /// Cached body-only text (after front matter), if extracted.
    pub cached_body: Option<String>,

    /// Detected front-matter kind.
    pub fm_kind: Option<FmKind>,
    /// Detected body kind (Markdown, Html, etc.).
    pub body_kind: Option<BodyKind>,
}

impl Document {
    /// Constructor (no function pointers needed here).
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            size: None,
            mtime: None,
            cache: None,
            cached_body: None,
            fm_kind: None,
            body_kind: None,
        }
    }

    // ───────────────────────────────
    // Builder-style setters
    // ───────────────────────────────

    pub fn with_size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    pub fn with_mtime(mut self, mtime: SystemTime) -> Self {
        self.mtime = Some(mtime);
        self
    }

    pub fn with_cache(mut self, cache: String) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn with_body(mut self, cached_body: String) -> Self {
        self.cached_body = Some(cached_body);
        self
    }

    pub fn with_fm_kind(mut self, kind: FmKind) -> Self {
        self.fm_kind = Some(kind);
        self
    }

    pub fn with_body_kind(mut self, kind: BodyKind) -> Self {
        self.body_kind = Some(kind);
        self
    }

    // ───────────────────────────────
    // Stream accessors (filesystem-backed)
    // ───────────────────────────────

    pub fn bytes_stream(&self) -> BoxStream<'static, io::Result<Bytes>> {
        let f = OPEN_BYTES_FN
            .read()
            .expect("OPEN_BYTES_FN RwLock poisoned")
            .expect("Document::open_bytes_fn not injected");
        f(&self.path)
    }

    pub fn utf8_stream(&self) -> BoxStream<'static, io::Result<String>> {
        let f = OPEN_UTF8_FN
            .read()
            .expect("OPEN_UTF8_FN RwLock poisoned")
            .expect("Document::open_utf8_fn not injected");
        f(&self.path)
    }
}
