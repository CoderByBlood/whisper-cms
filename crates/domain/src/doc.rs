// doc.rs

use bytes::Bytes;
use futures::stream::BoxStream;
use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

/// The kind of front matter (if any) found in the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmKind {
    Toml,
    Yaml,
    // Add Json, etc. later.
}

/// The kind of body payload the document has.
///
/// Note: there is no `None` variant; use `Option<BodyKind>` on `Document` to
/// represent "unknown / not detected yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Markdown,
    AsciiDoc,
    Html,
    Plain,
    ReStructuredText,
    OrgMode,
}

// If you prefer to avoid futures::BoxStream, you can define your own aliases:
//
// pub type ByteStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send + 'static>>;
// pub type Utf8Stream  = Pin<Box<dyn Stream<Item = io::Result<String>> + Send + 'static>>;
//
// and set the function pointers to return those. Using `BoxStream` is just a
// convenient type alias for the same thing.

/// Function pointer type for opening a byte stream for a given path.
pub type OpenBytesFn = fn(&PathBuf) -> BoxStream<'static, io::Result<Bytes>>;

/// Function pointer type for opening a UTF-8 text stream for a given path.
pub type OpenUtf8Fn = fn(&PathBuf) -> BoxStream<'static, io::Result<String>>;

/// Core unit flowing through the pipeline.
///
/// Design:
/// - **No caching** of bytes/utf8/html in this type.
/// - It represents:
///     • identity & metadata (`path`, `size`, `mtime`)
///     • analysis state (`fm_kind`, `body_kind`)
///     • and how to obtain content (function pointers that produce streams).
///
/// All actual IO happens via the function pointers, not inside Document itself.
#[derive(Debug, Clone)]
pub struct Document {
    /// Canonical source path.
    pub path: PathBuf,

    /// File size, if known. `None` means “not measured / unknown yet”.
    pub size: Option<u64>,

    /// Last modification time, if known.
    pub mtime: Option<SystemTime>,

    /// Detected front matter kind (if any).
    pub fm_kind: Option<FmKind>,

    /// Detected body kind (Markdown, AsciiDoc, Html, etc.), if known.
    pub body_kind: Option<BodyKind>,

    /// How to open a byte stream for this document.
    open_bytes: OpenBytesFn,

    /// How to open a UTF-8 text stream for this document.
    open_utf8: OpenUtf8Fn,
}

impl Document {
    /// Sole constructor.
    ///
    /// You inject the IO strategy via function pointers:
    ///   • `open_bytes` produces a stream of `io::Result<Bytes>`
    ///   • `open_utf8` produces a stream of `io::Result<String>`
    ///
    /// These functions are expected to be cheap to call and can be
    /// reused across many Documents.
    pub fn new(path: PathBuf, open_bytes: OpenBytesFn, open_utf8: OpenUtf8Fn) -> Self {
        Self {
            path,
            size: None,
            mtime: None,
            fm_kind: None,
            body_kind: None,
            open_bytes,
            open_utf8,
        }
    }

    // ─────────────────────────────────────────────
    // Builder-style setters (chainable)
    // Each consumes self and returns a modified Self so you can do:
    //
    //   let doc = Document::new(path, open_bytes, open_utf8)
    //       .with_size(1234)
    //       .with_mtime(mtime)
    //       .with_body_kind(BodyKind::Markdown);
    // ─────────────────────────────────────────────

    pub fn with_size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    pub fn with_mtime(mut self, mtime: SystemTime) -> Self {
        self.mtime = Some(mtime);
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

    /// Clear all derived analysis (front matter + body kind).
    ///
    /// Useful if upstream content has changed and you want to force
    /// re-analysis in downstream pipeline stages.
    pub fn clear_analysis(mut self) -> Self {
        self.fm_kind = None;
        self.body_kind = None;
        self
    }

    // ─────────────────────────────────────────────
    // Stream accessors
    // ─────────────────────────────────────────────

    /// Open a stream of raw bytes for this document.
    ///
    /// This delegates to the function pointer injected at construction time.
    pub fn bytes_stream(&self) -> BoxStream<'static, io::Result<Bytes>> {
        (self.open_bytes)(&self.path)
    }

    /// Open a stream of UTF-8 text lines/chunks for this document.
    ///
    /// Also delegates to the injected function pointer.
    pub fn utf8_stream(&self) -> BoxStream<'static, io::Result<String>> {
        (self.open_utf8)(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures::{stream, StreamExt};
    use std::io;

    // Simple helpers for test streams -------------------------------------

    fn open_bytes_ok(path: &PathBuf) -> BoxStream<'static, io::Result<Bytes>> {
        // Encode the path into the bytes so we can assert it was used.
        let s = format!("bytes:{}", path.display());
        let b = Bytes::from(s);
        stream::once(async move { Ok::<Bytes, io::Error>(b) }).boxed()
    }

    fn open_utf8_ok(path: &PathBuf) -> BoxStream<'static, io::Result<String>> {
        let s = format!("utf8:{}", path.display());
        stream::once(async move { Ok::<String, io::Error>(s) }).boxed()
    }

    fn open_bytes_err(_path: &PathBuf) -> BoxStream<'static, io::Result<Bytes>> {
        stream::once(async {
            Err::<Bytes, io::Error>(io::Error::new(io::ErrorKind::Other, "boom-bytes"))
        })
        .boxed()
    }

    fn open_utf8_err(_path: &PathBuf) -> BoxStream<'static, io::Result<String>> {
        stream::once(async {
            Err::<String, io::Error>(io::Error::new(io::ErrorKind::Other, "boom-utf8"))
        })
        .boxed()
    }

    // 1. Constructor sets defaults and stores path & function pointers -----

    #[test]
    fn new_initializes_with_defaults() {
        let path = PathBuf::from("/tmp/test.md");
        let doc = Document::new(path.clone(), open_bytes_ok, open_utf8_ok);

        assert_eq!(doc.path, path);
        assert_eq!(doc.size, None);
        assert_eq!(doc.mtime, None);
        assert_eq!(doc.fm_kind, None);
        assert_eq!(doc.body_kind, None);

        // Sanity: function pointers are set (just compare addresses).
        let bytes_fn: OpenBytesFn = open_bytes_ok;
        let utf8_fn: OpenUtf8Fn = open_utf8_ok;

        // Compare fn pointers by casting to usize (Rust allows fn pointer equality directly too).
        assert_eq!(doc.open_bytes as usize, bytes_fn as usize);
        assert_eq!(doc.open_utf8 as usize, utf8_fn as usize);
    }

    // 2. Builder-style setters chain and set fields correctly -------------

    #[test]
    fn builder_style_setters_chain_and_update_fields() {
        let path = PathBuf::from("/tmp/doc.md");
        let mtime = SystemTime::now();

        let doc = Document::new(path.clone(), open_bytes_ok, open_utf8_ok)
            .with_size(1234)
            .with_mtime(mtime)
            .with_fm_kind(FmKind::Toml)
            .with_body_kind(BodyKind::Markdown);

        assert_eq!(doc.path, path);
        assert_eq!(doc.size, Some(1234));
        assert_eq!(doc.mtime, Some(mtime));
        assert_eq!(doc.fm_kind, Some(FmKind::Toml));
        assert_eq!(doc.body_kind, Some(BodyKind::Markdown));
    }

    // 3. clear_analysis leaves other fields intact and resets analysis ----

    #[test]
    fn clear_analysis_resets_fm_and_body_only() {
        let path = PathBuf::from("/tmp/doc.adoc");
        let mtime = SystemTime::now();

        let doc = Document::new(path.clone(), open_bytes_ok, open_utf8_ok)
            .with_size(999)
            .with_mtime(mtime)
            .with_fm_kind(FmKind::Yaml)
            .with_body_kind(BodyKind::AsciiDoc);

        let cleared = doc.clone().clear_analysis();

        // Path, size, mtime preserved.
        assert_eq!(cleared.path, path);
        assert_eq!(cleared.size, Some(999));
        assert_eq!(cleared.mtime, Some(mtime));

        // Analysis reset.
        assert_eq!(cleared.fm_kind, None);
        assert_eq!(cleared.body_kind, None);

        // Original doc unchanged (clear_analysis consumes self, but just sanity check behavior).
        assert_eq!(doc.fm_kind, Some(FmKind::Yaml));
        assert_eq!(doc.body_kind, Some(BodyKind::AsciiDoc));
    }

    // 4. bytes_stream uses the injected OpenBytesFn and returns data ------

    #[test]
    fn bytes_stream_uses_injected_function() {
        futures::executor::block_on(async {
            let path = PathBuf::from("/tmp/content.txt");
            let doc = Document::new(path.clone(), open_bytes_ok, open_utf8_ok);

            let mut stream = doc.bytes_stream();
            let first = stream.next().await.expect("one item in stream");
            let bytes = first.expect("Ok bytes");

            let expected_prefix = format!("bytes:{}", path.display());
            assert_eq!(bytes, Bytes::from(expected_prefix));

            // No more items.
            assert!(stream.next().await.is_none());
        });
    }

    // 5. utf8_stream uses the injected OpenUtf8Fn and returns data --------

    #[test]
    fn utf8_stream_uses_injected_function() {
        futures::executor::block_on(async {
            let path = PathBuf::from("/tmp/content2.txt");
            let doc = Document::new(path.clone(), open_bytes_ok, open_utf8_ok);

            let mut stream = doc.utf8_stream();
            let first = stream.next().await.expect("one item in stream");
            let text = first.expect("Ok string");

            let expected = format!("utf8:{}", path.display());
            assert_eq!(text, expected);

            assert!(stream.next().await.is_none());
        });
    }

    // 6. Error path: bytes_stream propagates io::Error from OpenBytesFn ----

    #[test]
    fn bytes_stream_propagates_errors() {
        futures::executor::block_on(async {
            let path = PathBuf::from("/tmp/error.bin");
            let doc = Document::new(path, open_bytes_err, open_utf8_ok);

            let mut stream = doc.bytes_stream();
            let first = stream.next().await.expect("one item in stream");

            let err = first.expect_err("expected error from bytes stream");
            assert_eq!(err.kind(), io::ErrorKind::Other);
            assert_eq!(err.to_string(), "boom-bytes");

            assert!(stream.next().await.is_none());
        });
    }

    // 7. Error path: utf8_stream propagates io::Error from OpenUtf8Fn ------

    #[test]
    fn utf8_stream_propagates_errors() {
        futures::executor::block_on(async {
            let path = PathBuf::from("/tmp/error.txt");
            let doc = Document::new(path, open_bytes_ok, open_utf8_err);

            let mut stream = doc.utf8_stream();
            let first = stream.next().await.expect("one item in stream");

            let err = first.expect_err("expected error from utf8 stream");
            assert_eq!(err.kind(), io::ErrorKind::Other);
            assert_eq!(err.to_string(), "boom-utf8");

            assert!(stream.next().await.is_none());
        });
    }

    // 8. Multiple calls to bytes_stream / utf8_stream create fresh streams -

    #[test]
    fn multiple_stream_calls_are_independent() {
        futures::executor::block_on(async {
            let path = PathBuf::from("/tmp/independent.txt");
            let doc = Document::new(path.clone(), open_bytes_ok, open_utf8_ok);

            // bytes_stream calls
            let mut s1 = doc.bytes_stream();
            let mut s2 = doc.bytes_stream();

            let b1 = s1.next().await.unwrap().unwrap();
            let b2 = s2.next().await.unwrap().unwrap();
            assert_eq!(b1, b2);
            assert!(s1.next().await.is_none());
            assert!(s2.next().await.is_none());

            // utf8_stream calls
            let mut t1 = doc.utf8_stream();
            let mut t2 = doc.utf8_stream();

            let u1 = t1.next().await.unwrap().unwrap();
            let u2 = t2.next().await.unwrap().unwrap();
            assert_eq!(u1, u2);
            assert!(t1.next().await.is_none());
            assert!(t2.next().await.is_none());

            // Strings encode the path so we know the right path was used.
            let expected = format!("utf8:{}", path.display());
            assert_eq!(u1, expected);
        });
    }

    // 9. Clone: cloned Document reuses same fns & metadata -----------------

    #[test]
    fn clone_keeps_function_pointers_and_metadata() {
        let path = PathBuf::from("/tmp/clone.md");
        let mtime = SystemTime::now();

        let original = Document::new(path.clone(), open_bytes_ok, open_utf8_ok)
            .with_size(42)
            .with_mtime(mtime)
            .with_fm_kind(FmKind::Toml)
            .with_body_kind(BodyKind::Html);

        let clone = original.clone();

        assert_eq!(clone.path, original.path);
        assert_eq!(clone.size, original.size);
        assert_eq!(clone.mtime, original.mtime);
        assert_eq!(clone.fm_kind, original.fm_kind);
        assert_eq!(clone.body_kind, original.body_kind);

        assert_eq!(clone.open_bytes as usize, original.open_bytes as usize);
        assert_eq!(clone.open_utf8 as usize, original.open_utf8 as usize);
    }
}
