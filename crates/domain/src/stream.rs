// crates/domain/src/stream.rs

//! Stream abstraction shared across Document, ResolvedContent, and resolvers.
//!
//! This module defines:
//!   - [`StreamKind`] + [`StreamHandle`]: a small, cloneable, Send + Sync token
//!     that can represent either filesystem-based content or CAS-backed content.
//!   - Global DI hooks to convert a [`StreamHandle`] into:
//!       * a byte stream:  `BytesStream`
//!       * a UTF-8 text stream: `Utf8Stream`
//!
//! The **edge/serve** layers are responsible for injecting concrete
//! implementations at startup. Domain/adapt stay agnostic: they only see a
//! `StreamHandle` and, if needed, use the injected functions to materialize a
//! stream.

use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

// ─────────────────────────────────────────────────────────────────────────────
// Stream kinds & handle
// ─────────────────────────────────────────────────────────────────────────────

/// Logical kind / backing of a stream.
///
/// This is used to distinguish between filesystem-backed streams and
/// CAS/Tantivy-backed streams when implementing the DI functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// Filesystem content, addressed by a `PathBuf`.
    Fs,
    /// CAS / index-backed content, addressed by an opaque key (e.g. normalized path).
    Cas,
}

/// Opaque handle describing where/what to stream from.
///
/// Domain/adapt never interpret this directly; they just pass it around.
/// Edge/serve define how to convert this handle into a real stream via DI.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum StreamHandle {
    /// Filesystem-backed stream.
    Fs { path: PathBuf },
    /// CAS / index-backed stream (e.g. from Tantivy or some other store).
    Cas { key: PathBuf },
}

impl StreamHandle {
    /// Convenience constructor for a filesystem-based handle.
    pub fn fs(path: PathBuf) -> Self {
        StreamHandle::Fs { path }
    }

    /// Convenience constructor for a CAS-based handle.
    pub fn cas(key: PathBuf) -> Self {
        StreamHandle::Cas { key }
    }

    /// What kind of backing store this handle represents.
    pub fn kind(&self) -> StreamKind {
        match self {
            StreamHandle::Fs { .. } => StreamKind::Fs,
            StreamHandle::Cas { .. } => StreamKind::Cas,
        }
    }

    /// Materialize this handle as a byte stream using the injected resolver.
    ///
    /// Panics if no resolver was injected (by design: should be wired at startup).
    pub fn get_bytes(&self) -> io::Result<Bytes> {
        let handle = self.clone();
        let join = std::thread::spawn(move || -> io::Result<Bytes> {
            // Build a fresh current-thread runtime on this new thread.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

            rt.block_on(async move {
                // Adjust this to *your* StreamHandle API.
                // I'm assuming something like: `into_stream() -> impl Stream<Item = io::Result<Bytes>>`.
                let mut stream = handle.open_bytes(); // <-- adapt this line

                let mut buf = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?; // io::Result<Bytes>
                    buf.extend_from_slice(&chunk);
                }

                Ok(Bytes::from(buf))
            })
        });

        // Map thread panic into an io::Error
        match join.join() {
            Ok(res) => res,
            Err(_) => Err(io::Error::new(
                io::ErrorKind::Other,
                "get_bytes thread panicked",
            )),
        }
    }

    /// Materialize this handle as a UTF-8 text stream using the injected resolver.
    ///
    /// Panics if no resolver was injected (by design: should be wired at startup).
    pub fn get_utf8(&self) -> io::Result<String> {
        Ok(String::from_utf8_lossy(&self.get_bytes()?).to_string())
    }

    /// Materialize this handle as a byte stream using the injected resolver.
    ///
    /// Panics if no resolver was injected (by design: should be wired at startup).
    pub fn open_bytes(&self) -> BytesStream {
        let guard = OPEN_BYTES_FROM_HANDLE_FN
            .read()
            .expect("OPEN_BYTES_FROM_HANDLE_FN RwLock poisoned");
        let f = guard
                .expect("open_bytes_from_handle_fn not injected; call inject_open_bytes_from_handle_fn at startup");
        f(self)
    }

    /// Materialize this handle as a UTF-8 text stream using the injected resolver.
    ///
    /// Panics if no resolver was injected (by design: should be wired at startup).
    pub fn open_utf8(&self) -> Utf8Stream {
        let guard = OPEN_UTF8_FROM_HANDLE_FN
            .read()
            .expect("OPEN_UTF8_FROM_HANDLE_FN RwLock poisoned");
        let f = guard
                .expect("open_utf8_from_handle_fn not injected; call inject_open_utf8_from_handle_fn at startup");
        f(self)
    }

    /// Accessor for the underlying filesystem path, if any.
    pub fn identity(&self) -> &PathBuf {
        match self {
            StreamHandle::Fs { path } => path,
            StreamHandle::Cas { key } => key,
        }
    }
}

impl fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamHandle::Fs { path } => f
                .debug_struct("StreamHandle::Fs")
                .field("path", path)
                .finish(),
            StreamHandle::Cas { key } => f
                .debug_struct("StreamHandle::Cas")
                .field("key", key)
                .finish(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Type aliases for the actual stream types
// ─────────────────────────────────────────────────────────────────────────────

pub type BytesStream = BoxStream<'static, io::Result<Bytes>>;
pub type Utf8Stream = BoxStream<'static, io::Result<String>>;

// ─────────────────────────────────────────────────────────────────────────────
// Global DI hooks: StreamHandle -> stream
// ─────────────────────────────────────────────────────────────────────────────

/// Function shape for converting a `StreamHandle` into a byte stream.
///
/// Implementations are typically provided in the edge crate:
///  - Fs ⇒ `tokio::fs::File`
///  - Cas ⇒ Tantivy index reader / CAS API
pub type OpenBytesFromHandleFn = fn(&StreamHandle) -> BytesStream;

/// Function shape for converting a `StreamHandle` into a UTF-8 text stream.
pub type OpenUtf8FromHandleFn = fn(&StreamHandle) -> Utf8Stream;

/// Default panic implementations (force early failure if not injected).
fn missing_open_bytes_from_handle(_h: &StreamHandle) -> BytesStream {
    panic!("open_bytes_from_handle_fn not injected");
}

fn missing_open_utf8_from_handle(_h: &StreamHandle) -> Utf8Stream {
    panic!("open_utf8_from_handle_fn not injected");
}

/// GLOBAL injection targets (set once at startup by the edge layer).
///
/// Pattern: `LazyLock<RwLock<Option<T>>>`, as agreed.
pub static OPEN_BYTES_FROM_HANDLE_FN: LazyLock<RwLock<Option<OpenBytesFromHandleFn>>> =
    LazyLock::new(|| RwLock::new(Some(missing_open_bytes_from_handle)));

pub static OPEN_UTF8_FROM_HANDLE_FN: LazyLock<RwLock<Option<OpenUtf8FromHandleFn>>> =
    LazyLock::new(|| RwLock::new(Some(missing_open_utf8_from_handle)));

/// API the edge/serve crate calls *once* during initialization.
pub fn inject_open_bytes_from_handle_fn(f: OpenBytesFromHandleFn) {
    let mut guard = OPEN_BYTES_FROM_HANDLE_FN
        .write()
        .expect("OPEN_BYTES_FROM_HANDLE_FN RwLock poisoned");
    *guard = Some(f);
}

pub fn inject_open_utf8_from_handle_fn(f: OpenUtf8FromHandleFn) {
    let mut guard = OPEN_UTF8_FROM_HANDLE_FN
        .write()
        .expect("OPEN_UTF8_FROM_HANDLE_FN RwLock poisoned");
    *guard = Some(f);
}
