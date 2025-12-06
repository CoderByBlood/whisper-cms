// crates/domain/src/doc.rs

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

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
    pub cache: Option<Arc<str>>,
    /// Cached body-only text (after front matter), if extracted.
    pub cached_body: Option<Arc<str>>,

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

    pub fn with_cache(mut self, cache: Arc<str>) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn with_body(mut self, cached_body: Arc<str>) -> Self {
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
}
