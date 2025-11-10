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
    read_file_bytes: fn(&Path) -> io::Result<Vec<u8>>,
    write_file_bytes: fn(&Path, &[u8]) -> io::Result<()>,

    scan_folder_with_report_and_filters: fn(
        root_dir: &Path,
        dir_name_re: Option<&Regex>,
        file_name_re: Option<&Regex>,
    ) -> io::Result<(ScannedFolder, ScanReport)>,

    lookup_by_absolute:
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
            scan_folder_with_report_and_filters: scan,
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

    #[inline]
    pub fn scan_with_report_and_filters(
        &self,
        p: &Path,
        dir_name_re: Option<&Regex>,
        file_name_re: Option<&Regex>,
    ) -> io::Result<(ScannedFolder, ScanReport)> {
        (self.scan_folder_with_report_and_filters)(p, dir_name_re, file_name_re)
    }

    #[inline]
    pub fn lookup_by_absolute(
        &self,
        by_abs: &HashMap<PathBuf, Arc<File>>,
        abs: &Path,
    ) -> io::Result<Option<Arc<File>>> {
        (self.lookup_by_absolute)(by_abs, abs)
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
#[derive(Debug, Clone)]
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
        (self.svc.scan_folder_with_report_and_filters)(
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

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::cell::{Cell, RefCell};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    // ---------- In-memory mock "filesystem" + call tracking ----------

    thread_local! {
        static MEMFS: RefCell<HashMap<PathBuf, Vec<u8>>> = RefCell::new(HashMap::new());
        static CALLS_SCAN: Cell<usize> = Cell::new(0);
        static LAST_DIR_RE: RefCell<Option<String>> = RefCell::new(None);
        static LAST_FILE_RE: RefCell<Option<String>> = RefCell::new(None);
    }

    fn memfs_clear() {
        MEMFS.with(|m| m.borrow_mut().clear());
        CALLS_SCAN.with(|c| c.set(0));
        LAST_DIR_RE.with(|s| *s.borrow_mut() = None);
        LAST_FILE_RE.with(|s| *s.borrow_mut() = None);
    }

    fn memfs_insert(path: impl AsRef<Path>, data: impl AsRef<[u8]>) {
        MEMFS.with(|m| {
            m.borrow_mut()
                .insert(path.as_ref().to_path_buf(), data.as_ref().to_vec())
        });
    }

    fn memfs_get(path: &Path) -> Option<Vec<u8>> {
        MEMFS.with(|m| m.borrow().get(path).cloned())
    }

    // ---------- Mock FileService functions (fn pointers, not closures) ----------

    fn mock_read_file_bytes(p: &Path) -> io::Result<Vec<u8>> {
        MEMFS.with(|m| {
            m.borrow()
                .get(p)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "not found"))
        })
    }

    fn mock_write_file_bytes(p: &Path, data: &[u8]) -> io::Result<()> {
        MEMFS.with(|m| {
            m.borrow_mut().insert(p.to_path_buf(), data.to_vec());
        });
        Ok(())
    }

    fn mock_lookup_by_absolute(
        by_abs: &HashMap<PathBuf, Arc<File>>,
        abs: &Path,
    ) -> io::Result<Option<Arc<File>>> {
        // No canonicalization; direct map lookup is sufficient here.
        Ok(by_abs.get(abs).cloned())
    }

    /// Build a ScannedFolder from MEMFS entries under `root`.
    /// Respects (records) received filters, and returns a minimal ScanReport.
    fn mock_scan_with_report_and_filters(
        root: &Path,
        dir_name_re: Option<&Regex>,
        file_name_re: Option<&Regex>,
    ) -> io::Result<(ScannedFolder, ScanReport)> {
        CALLS_SCAN.with(|c| c.set(c.get() + 1));
        LAST_DIR_RE.with(|s| *s.borrow_mut() = dir_name_re.map(|r| r.as_str().to_string()));
        LAST_FILE_RE.with(|s| *s.borrow_mut() = file_name_re.map(|r| r.as_str().to_string()));

        // Build maps: include only files whose absolute path starts with `root`
        // and (if provided) whose last component matches file_name_re,
        // and whose intermediate directories match dir_name_re (by name).
        let mut files_vec: Vec<Arc<File>> = Vec::new();
        let mut by_abs: HashMap<PathBuf, Arc<File>> = HashMap::new();
        let mut by_rel: HashMap<PathBuf, Arc<File>> = HashMap::new();

        let svc = mock_service();

        let entries: Vec<PathBuf> = MEMFS.with(|m| {
            m.borrow()
                .keys()
                .filter(|p| p.starts_with(root))
                .cloned()
                .collect()
        });

        'outer: for abs in entries {
            // Directory filter: every component between root and leaf must match if regex present
            if let Some(re) = dir_name_re {
                if let Ok(rel_comp) = abs.strip_prefix(root) {
                    for comp in rel_comp.parent().into_iter().flat_map(|p| p.iter()) {
                        let name = comp.to_string_lossy();
                        if !re.is_match(&name) {
                            continue 'outer;
                        }
                    }
                }
            }

            // File-name filter
            if let Some(re) = file_name_re {
                if let Some(name) = abs.file_name().map(|s| s.to_string_lossy()) {
                    if !re.is_match(&name) {
                        continue;
                    }
                }
            }

            let rel = abs
                .strip_prefix(root)
                .unwrap_or_else(|_| Path::new(""))
                .to_path_buf();
            let f = Arc::new(File::new(abs.clone(), rel.clone(), svc));
            by_rel.entry(rel).or_insert_with(|| f.clone());
            by_abs.entry(abs).or_insert_with(|| f.clone());
            files_vec.push(f);
        }

        let report = ScanReport {
            entries_seen: files_vec.len(),
            files_indexed: files_vec.len(),
            errors: Vec::new(),
        };

        let folder = ScannedFolder::new(
            root.to_path_buf(),
            files_vec,
            by_abs,
            by_rel,
            dir_name_re.cloned(),
            file_name_re.cloned(),
            svc,
        );
        Ok((folder, report))
    }

    fn mock_service() -> FileService {
        FileService::from_fns(
            mock_read_file_bytes,
            mock_write_file_bytes,
            mock_scan_with_report_and_filters,
            mock_lookup_by_absolute,
        )
    }

    // ---------- Helpers ----------

    fn to_set<I, T: Eq + std::hash::Hash>(it: I) -> HashSet<T>
    where
        I: IntoIterator<Item = T>,
    {
        it.into_iter().collect()
    }

    // ======================= Tests for File =======================

    #[test]
    fn file_read_write_roundtrip_bytes() {
        memfs_clear();
        let svc = mock_service();

        let abs = PathBuf::from("/proj/a.txt");
        let rel = PathBuf::from("a.txt");
        let file = File::new(abs.clone(), rel, svc);

        assert!(mock_read_file_bytes(&abs).is_err()); // not present yet

        // Write via wrapper -> stored in MEMFS
        file.write_bytes(b"hello").unwrap();
        assert_eq!(memfs_get(&abs).as_deref(), Some(b"hello".as_ref()));

        // Read via wrapper
        let bytes = file.read_bytes().unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn file_read_string_and_invalid_utf8_error() {
        memfs_clear();
        let svc = mock_service();
        let abs = PathBuf::from("/proj/bin.dat");
        let file = File::new(abs.clone(), PathBuf::from("bin.dat"), svc);

        // Bad UTF-8
        memfs_insert(&abs, &[0xff, 0xfe, 0xfd]);
        let err = file.read_string().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        // Good UTF-8
        memfs_insert(&abs, "OK".as_bytes());
        assert_eq!(file.read_string().unwrap(), "OK");
    }

    #[test]
    fn file_write_string_and_overwrite() {
        memfs_clear();
        let svc = mock_service();
        let abs = PathBuf::from("/proj/note.txt");
        let file = File::new(abs.clone(), PathBuf::from("note.txt"), svc);

        file.write_string("first").unwrap();
        assert_eq!(memfs_get(&abs).unwrap(), b"first");

        file.write_string("second").unwrap();
        assert_eq!(file.read_string().unwrap(), "second");
    }

    // ======================= Tests for ScannedFolder =======================

    #[test]
    fn scannedfolder_get_by_relative_and_absolute() {
        memfs_clear();
        let _svc = mock_service();

        // Seed MEMFS under /root
        let root = PathBuf::from("/root");
        let abs_a = root.join("a/one.txt");
        let abs_b = root.join("b/two.txt");
        memfs_insert(&abs_a, b"1");
        memfs_insert(&abs_b, b"2");

        // Build folder via mock scan (no filters)
        let (store, _rpt) = mock_scan_with_report_and_filters(&root, None, None).unwrap();

        // get_by_relative
        let a_rel = Path::new("a/one.txt");
        let f_rel = store.get_by_relative(a_rel).expect("missing rel a/one.txt");
        assert_eq!(f_rel.relative_path(), a_rel);

        // get_by_absolute (mock lookup is direct match)
        let f_abs = store.get_by_absolute(&abs_a).unwrap().expect("missing abs");
        assert!(Arc::ptr_eq(&f_rel, &f_abs));

        // Missing paths
        assert!(store.get_by_relative(Path::new("nope.txt")).is_none());
        assert!(store
            .get_by_absolute(&root.join("nope.txt"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn scannedfolder_files_and_root_accessors() {
        memfs_clear();
        let _svc = mock_service();

        let root = PathBuf::from("/root");
        memfs_insert(root.join("x/1.txt"), b"1");
        memfs_insert(root.join("x/2.txt"), b"2");

        let (store, _rpt) = mock_scan_with_report_and_filters(&root, None, None).unwrap();

        // Root accessor
        assert_eq!(store.root(), Path::new("/root"));

        // Files accessor (order not guaranteed)
        let rels: HashSet<PathBuf> = to_set(
            store
                .files()
                .iter()
                .map(|f| f.relative_path().to_path_buf()),
        );
        assert!(rels.contains(Path::new("x/1.txt")));
        assert!(rels.contains(Path::new("x/2.txt")));
    }

    #[test]
    fn scannedfolder_refresh_uses_same_filters_and_calls_service() {
        memfs_clear();
        let _svc = mock_service();

        let root = PathBuf::from("/site");
        // seed initial state
        memfs_insert(root.join("posts/a.md"), b"A");

        let dir_re = Regex::new(r"^(posts|pages)$").unwrap();
        let file_re = Regex::new(r"(?i)\.(md|html)$").unwrap();

        // initial scan with filters
        let (store, _rpt) =
            mock_scan_with_report_and_filters(&root, Some(&dir_re), Some(&file_re)).unwrap();

        // before refresh
        let calls_before = CALLS_SCAN.with(|c| c.get());
        LAST_DIR_RE.with(|s| assert_eq!(s.borrow().as_deref(), Some(r"^(posts|pages)$")));
        LAST_FILE_RE.with(|s| assert_eq!(s.borrow().as_deref(), Some(r"(?i)\.(md|html)$")));

        // mutate MEMFS and refresh
        memfs_insert(root.join("pages/home.html"), b"<html/>");
        let store2 = store.refresh().unwrap();

        // service was called again
        let calls_after = CALLS_SCAN.with(|c| c.get());
        assert_eq!(calls_after, calls_before + 1);

        // same filters were passed again
        LAST_DIR_RE.with(|s| assert_eq!(s.borrow().as_deref(), Some(r"^(posts|pages)$")));
        LAST_FILE_RE.with(|s| assert_eq!(s.borrow().as_deref(), Some(r"(?i)\.(md|html)$")));

        // and the new file is present now
        let rels: HashSet<PathBuf> = to_set(
            store2
                .files()
                .iter()
                .map(|f| f.relative_path().to_path_buf()),
        );
        assert!(rels.contains(Path::new("pages/home.html")));
    }

    #[test]
    fn file_read_missing_returns_not_found() {
        memfs_clear();
        let svc = mock_service();
        let abs = PathBuf::from("/no/such/file.txt");
        let file = File::new(abs.clone(), PathBuf::from("file.txt"), svc);

        let err = file.read_bytes().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
