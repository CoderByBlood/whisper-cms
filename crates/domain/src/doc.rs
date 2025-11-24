// crates/domain/src/doc.rs

use bytes::Bytes;
use futures::stream::BoxStream;
use std::io;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::SystemTime;

// ─────────────────────────────────────────────────────────────────────────────
// Injection points (GLOBAL function pointers)
// ─────────────────────────────────────────────────────────────────────────────

/// Stream of raw bytes
pub type OpenBytesFn = fn(&PathBuf) -> BoxStream<'static, io::Result<Bytes>>;

/// Stream of UTF-8 text
pub type OpenUtf8Fn = fn(&PathBuf) -> BoxStream<'static, io::Result<String>>;

/// Default panic implementations (force early detect if not injected)
fn missing_open_bytes(_path: &PathBuf) -> BoxStream<'static, io::Result<Bytes>> {
    panic!("Document::open_bytes_fn not injected");
}

fn missing_open_utf8(_path: &PathBuf) -> BoxStream<'static, io::Result<String>> {
    panic!("Document::open_utf8_fn not injected");
}

/// GLOBAL injection targets (set once at startup)
pub static OPEN_BYTES_FN: LazyLock<std::sync::RwLock<OpenBytesFn>> =
    LazyLock::new(|| std::sync::RwLock::new(missing_open_bytes));

pub static OPEN_UTF8_FN: LazyLock<std::sync::RwLock<OpenUtf8Fn>> =
    LazyLock::new(|| std::sync::RwLock::new(missing_open_utf8));

/// API the edge crate calls *once* during initialization
pub fn inject_open_bytes_fn(f: OpenBytesFn) {
    *OPEN_BYTES_FN.write().unwrap() = f;
}

pub fn inject_open_utf8_fn(f: OpenUtf8Fn) {
    *OPEN_UTF8_FN.write().unwrap() = f;
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
    pub path: PathBuf,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,

    pub cache: Option<String>,
    pub cached_body: Option<String>,

    pub fm_kind: Option<FmKind>,
    pub body_kind: Option<BodyKind>,
}

impl Document {
    // Constructor (NO function pointers needed anymore)
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

    pub fn clear_analysis(mut self) -> Self {
        self.fm_kind = None;
        self.body_kind = None;
        self.cache = None;
        self
    }

    // ───────────────────────────────
    // Stream accessors now use GLOBAL DI
    // ───────────────────────────────

    pub fn bytes_stream(&self) -> BoxStream<'static, io::Result<Bytes>> {
        let f = *OPEN_BYTES_FN.read().unwrap();
        f(&self.path)
    }

    pub fn utf8_stream(&self) -> BoxStream<'static, io::Result<String>> {
        let f = *OPEN_UTF8_FN.read().unwrap();
        f(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures::{stream, StreamExt};
    use std::io;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    // Helper: reset globals to their default panic implementations.
    fn reset_injected_fns() {
        // These are in the parent module and visible here even though they're private.
        inject_open_bytes_fn(missing_open_bytes);
        inject_open_utf8_fn(missing_open_utf8);
    }

    // ─────────────────────────────────────────────
    // Default behavior (no injection)
    // ─────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "Document::open_bytes_fn not injected")]
    fn bytes_stream_panics_when_not_injected() {
        reset_injected_fns();

        let doc = Document::new(PathBuf::from("foo.txt"));
        let mut stream = doc.bytes_stream();

        // Force evaluation of the stream; this should panic when the fn is called.
        futures::executor::block_on(async move { while let Some(_item) = stream.next().await {} });
    }

    #[test]
    #[should_panic(expected = "Document::open_utf8_fn not injected")]
    fn utf8_stream_panics_when_not_injected() {
        reset_injected_fns();

        let doc = Document::new(PathBuf::from("bar.md"));
        let mut stream = doc.utf8_stream();

        futures::executor::block_on(async move { while let Some(_item) = stream.next().await {} });
    }

    // ─────────────────────────────────────────────
    // Injection: positive paths
    // ─────────────────────────────────────────────

    fn test_open_bytes(path: &PathBuf) -> BoxStream<'static, io::Result<Bytes>> {
        assert_eq!(path, &PathBuf::from("content/test.bin"));
        let data = Bytes::from_static(b"hello-bytes");
        Box::pin(stream::once(async move { Ok(data) }))
    }

    fn test_open_utf8(path: &PathBuf) -> BoxStream<'static, io::Result<String>> {
        assert_eq!(path, &PathBuf::from("content/test.txt"));
        let data = "hello-utf8".to_string();
        Box::pin(stream::once(async move { Ok(data) }))
    }

    #[test]
    fn bytes_stream_uses_injected_impl() {
        reset_injected_fns();
        inject_open_bytes_fn(test_open_bytes);

        let doc = Document::new(PathBuf::from("content/test.bin"));
        let mut stream = doc.bytes_stream();

        let items: Vec<io::Result<Bytes>> = futures::executor::block_on(async move {
            let mut out = Vec::new();
            while let Some(item) = stream.next().await {
                out.push(item);
            }
            out
        });

        assert_eq!(items.len(), 1);
        let first = items.into_iter().next().unwrap().expect("expected Ok");
        assert_eq!(&first[..], b"hello-bytes");
    }

    #[test]
    fn utf8_stream_uses_injected_impl() {
        reset_injected_fns();
        inject_open_utf8_fn(test_open_utf8);

        let doc = Document::new(PathBuf::from("content/test.txt"));
        let mut stream = doc.utf8_stream();

        let items: Vec<io::Result<String>> = futures::executor::block_on(async move {
            let mut out = Vec::new();
            while let Some(item) = stream.next().await {
                out.push(item);
            }
            out
        });

        assert_eq!(items.len(), 1);
        let first = items.into_iter().next().unwrap().expect("expected Ok");
        assert_eq!(first, "hello-utf8");
    }

    // ─────────────────────────────────────────────
    // Injection: negative I/O scenarios
    // ─────────────────────────────────────────────

    fn error_open_bytes(_path: &PathBuf) -> BoxStream<'static, io::Result<Bytes>> {
        let err = io::Error::new(io::ErrorKind::Other, "bytes-io-error");
        Box::pin(stream::once(async move { Err(err) }))
    }

    fn error_open_utf8(_path: &PathBuf) -> BoxStream<'static, io::Result<String>> {
        let err = io::Error::new(io::ErrorKind::InvalidData, "utf8-io-error");
        Box::pin(stream::once(async move { Err(err) }))
    }

    #[test]
    fn bytes_stream_propagates_io_error() {
        reset_injected_fns();
        inject_open_bytes_fn(error_open_bytes);

        let doc = Document::new(PathBuf::from("content/error.bin"));
        let mut stream = doc.bytes_stream();

        let items: Vec<io::Result<Bytes>> = futures::executor::block_on(async move {
            let mut out = Vec::new();
            while let Some(item) = stream.next().await {
                out.push(item);
            }
            out
        });

        assert_eq!(items.len(), 1);
        let err = items.into_iter().next().unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(err.to_string(), "bytes-io-error");
    }

    #[test]
    fn utf8_stream_propagates_io_error() {
        reset_injected_fns();
        inject_open_utf8_fn(error_open_utf8);

        let doc = Document::new(PathBuf::from("content/error.txt"));
        let mut stream = doc.utf8_stream();

        let items: Vec<io::Result<String>> = futures::executor::block_on(async move {
            let mut out = Vec::new();
            while let Some(item) = stream.next().await {
                out.push(item);
            }
            out
        });

        assert_eq!(items.len(), 1);
        let err = items.into_iter().next().unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "utf8-io-error");
    }

    // ─────────────────────────────────────────────
    // Builder methods & clear_analysis behavior
    // ─────────────────────────────────────────────

    #[test]
    fn builder_methods_set_fields_correctly() {
        let path = PathBuf::from("content/page.md");
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1234);

        let doc = Document::new(path.clone())
            .with_size(42)
            .with_mtime(mtime)
            .with_cache("full-cache".to_string())
            .with_body("body-only".to_string())
            .with_fm_kind(FmKind::Yaml)
            .with_body_kind(BodyKind::Markdown);

        assert_eq!(doc.path, path);
        assert_eq!(doc.size, Some(42));
        assert_eq!(doc.mtime, Some(mtime));
        assert_eq!(doc.cache.as_deref(), Some("full-cache"));
        assert_eq!(doc.cached_body.as_deref(), Some("body-only"));
        assert_eq!(doc.fm_kind, Some(FmKind::Yaml));
        assert_eq!(doc.body_kind, Some(BodyKind::Markdown));
    }

    #[test]
    fn clear_analysis_resets_analysis_but_keeps_path_and_cached_body() {
        let path = PathBuf::from("content/clear.md");

        let doc = Document::new(path.clone())
            .with_cache("full-cache".to_string())
            .with_body("body-only".to_string())
            .with_fm_kind(FmKind::Toml)
            .with_body_kind(BodyKind::AsciiDoc);

        let cleared = doc.clear_analysis();

        // Path is unchanged.
        assert_eq!(cleared.path, path);

        // Analysis fields are cleared.
        assert_eq!(cleared.fm_kind, None);
        assert_eq!(cleared.body_kind, None);
        assert_eq!(cleared.cache, None);

        // cached_body is intentionally preserved.
        assert_eq!(cleared.cached_body.as_deref(), Some("body-only"));
    }
}
