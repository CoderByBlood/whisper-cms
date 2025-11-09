//! Types + errors for the scanning subsystem (no std::fs here).
//! Thin methods delegate to functions implemented in `scan.rs`.

use regex::Regex;
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

/// Bundled, vtable-free capabilities used by File & ScannedFolder.
/// Concrete wiring is provided from `scan.rs`.
#[derive(Clone, Copy, Debug)]
pub struct FileService {
    pub read_file_bytes: fn(&Path) -> io::Result<Vec<u8>>,
    pub write_file_bytes: fn(&Path, &[u8]) -> io::Result<()>,

    pub scan_with_report_and_filters: fn(
        root_dir: &Path,
        dir_name_re: Option<&Regex>,
        file_name_re: Option<&Regex>,
    ) -> io::Result<(ScannedFolder, ScanReport)>,

    pub lookup_by_absolute:
        fn(by_abs: &HashMap<PathBuf, Arc<File>>, abs: &Path) -> io::Result<Option<Arc<File>>>,
}

impl FileService {
    /// Build from explicit function pointers (handy for tests).
    pub const fn from_fns(
        read: fn(&Path) -> io::Result<Vec<u8>>,
        write: fn(&Path, &[u8]) -> io::Result<()>,
        scan: fn(&Path, Option<&Regex>, Option<&Regex>) -> io::Result<(ScannedFolder, ScanReport)>,
        lookup: fn(&HashMap<PathBuf, Arc<File>>, &Path) -> io::Result<Option<Arc<File>>>,
    ) -> Self {
        Self {
            read_file_bytes: read,
            write_file_bytes: write,
            scan_with_report_and_filters: scan,
            lookup_by_absolute: lookup,
        }
    }

    // Optional convenience wrappers (keeps call-sites tidy if you prefer methods)
    #[inline]
    pub fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        (self.read_file_bytes)(p)
    }
    #[inline]
    pub fn write(&self, p: &Path, data: &[u8]) -> io::Result<()> {
        (self.write_file_bytes)(p, data)
    }
}

/// Where a non-fatal scan error occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanErrorKind {
    WalkDir,
    Canonicalize,
    StripPrefix,
    SameFileHandle,
}

/// Rich, *non-fatal* error captured during scanning, suitable for reporting.
/// Lives in `ScanReport.errors`. Uses `thiserror` for pretty Display.
#[derive(Debug, Error)]
pub enum ScanStageError {
    #[error("walkdir at {path}: {source}")]
    WalkDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("canonicalize {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("strip_prefix {path}: {source}")]
    StripPrefix {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("same-file handle {path}: {source}")]
    SameFileHandle {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub entries_seen: usize,
    pub files_indexed: usize,
    pub errors: Vec<ScanStageError>,
}

/// A single file known to the store (canonical absolute + path relative to root).
#[derive(Debug)]
pub struct File {
    abs: PathBuf,
    rel: PathBuf,
    svc: FileService,
}

impl File {
    pub fn new(abs: PathBuf, rel: PathBuf, svc: FileService) -> Self {
        Self { abs, rel, svc }
    }

    /// Canonical absolute filesystem path of this file.
    #[inline]
    pub fn absolute_path(&self) -> &Path {
        &self.abs
    }

    /// Path of this file relative to the store root.
    #[inline]
    pub fn relative_path(&self) -> &Path {
        &self.rel
    }

    /// Read entire file as bytes (no cache; re-reads from disk each call).
    #[inline]
    pub fn read_bytes(&self) -> io::Result<Vec<u8>> {
        (self.svc.read_file_bytes)(&self.abs)
    }

    /// Read as UTF-8 string (no cache).
    #[inline]
    pub fn read_string(&self) -> io::Result<String> {
        let bytes = self.read_bytes()?;
        String::from_utf8(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("UTF-8 error: {e}")))
    }

    /// Overwrite file with given bytes.
    #[inline]
    pub fn write_bytes(&self, data: &[u8]) -> io::Result<()> {
        (self.svc.write_file_bytes)(&self.abs, data)
    }

    /// Overwrite file with given UTF-8 string.
    #[inline]
    pub fn write_string(&self, s: &str) -> io::Result<()> {
        self.write_bytes(s.as_bytes())
    }
}

/// In-memory index of files under a root directory.
/// Lookups by relative or absolute path return the same `Arc<File>` when they
/// refer to the same canonical file (and the same physical file when unified).
#[derive(Debug)]
pub struct ScannedFolder {
    root: PathBuf,
    files: Vec<Arc<File>>,
    by_abs: HashMap<PathBuf, Arc<File>>,
    by_rel: HashMap<PathBuf, Arc<File>>,
    // Remember filters for refresh()
    dir_name_re: Option<Regex>,
    file_name_re: Option<Regex>,
    svc: FileService,
}

impl ScannedFolder {
    pub fn new(
        root: PathBuf,
        files: Vec<Arc<File>>,
        by_abs: HashMap<PathBuf, Arc<File>>,
        by_rel: HashMap<PathBuf, Arc<File>>,
        dir_name_re: Option<Regex>,
        file_name_re: Option<Regex>,
        svc: FileService,
    ) -> Self {
        Self {
            root,
            files,
            by_abs,
            by_rel,
            dir_name_re,
            file_name_re,
            svc,
        }
    }

    /// Re-scan the directory and return a refreshed store using the SAME filters.
    #[inline]
    pub fn refresh(&self) -> io::Result<Self> {
        (self.svc.scan_with_report_and_filters)(
            &self.root,
            self.dir_name_re.as_ref(),
            self.file_name_re.as_ref(),
        )
        .map(|(s, _)| s)
    }

    /// Root directory (canonical absolute).
    #[inline]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// All files (as `Arc<File>`). Order is not guaranteed.
    #[inline]
    pub fn files(&self) -> &[Arc<File>] {
        &self.files
    }

    /// Lookup by relative path (from the store root).
    #[inline]
    pub fn get_by_relative(&self, rel: &Path) -> Option<Arc<File>> {
        self.by_rel.get(rel).cloned()
    }

    /// Lookup by absolute path (any absolute); will be canonicalized.
    #[inline]
    pub fn get_by_absolute(&self, abs: &Path) -> io::Result<Option<Arc<File>>> {
        (self.svc.lookup_by_absolute)(&self.by_abs, abs)
    }
}
