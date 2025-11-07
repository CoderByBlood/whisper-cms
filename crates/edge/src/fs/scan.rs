//! ScannedFolder — parallel directory scan + lazy (re)reads on demand, no caching.
//!
//! Uses rayon 1.11 docs: https://docs.rs/rayon/1.11.0/rayon/
//! Uses walkdir 2.5 docs: https://docs.rs/walkdir/2.5.0/walkdir/

use rayon::prelude::*;
use same_file::Handle;
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};
use walkdir::{DirEntry, WalkDir};

#[derive(Debug)]
pub struct File {
    abs: PathBuf, // canonical absolute path
    rel: PathBuf, // path relative to the store root
}

impl File {
    pub fn absolute_path(&self) -> &Path {
        &self.abs
    }
    pub fn relative_path(&self) -> &Path {
        &self.rel
    }

    /// Read entire file as bytes (no cache; re-reads from disk each call).
    pub fn read_bytes(&self) -> io::Result<Vec<u8>> {
        fs::read(&self.abs)
    }

    /// Read as UTF-8 string (no cache).
    pub fn read_string(&self) -> io::Result<String> {
        let bytes = self.read_bytes()?;
        String::from_utf8(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("UTF-8 error: {e}")))
    }

    /// Overwrite file with given bytes.
    pub fn write_bytes(&self, data: &[u8]) -> io::Result<()> {
        if let Some(parent) = self.abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.abs, data)
    }

    /// Overwrite file with given UTF-8 string.
    pub fn write_string(&self, s: &str) -> io::Result<()> {
        self.write_bytes(s.as_bytes())
    }
}

/// What went wrong during scanning (non-fatal; we keep going).
#[derive(Debug)]
pub struct ScanError {
    pub stage: &'static str,
    pub path: PathBuf,
    pub error: io::Error,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub entries_seen: usize,
    pub files_indexed: usize,
    pub errors: Vec<ScanError>,
}

#[derive(Debug)]
pub struct ScannedFolder {
    root: PathBuf, // canonical absolute root
    files: Vec<Arc<File>>,
    by_abs: HashMap<PathBuf, Arc<File>>, // key: canonical absolute path
    by_rel: HashMap<PathBuf, Arc<File>>, // key: relative path from root
}

impl ScannedFolder {
    pub fn refresh(&self) -> io::Result<Self> {
        // Discard report like the simple `scan_folder` does.
        scan_folder(&self.root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn files(&self) -> &[Arc<File>] {
        &self.files
    }

    pub fn get_by_relative(&self, rel: impl AsRef<Path>) -> Option<Arc<File>> {
        self.by_rel.get(rel.as_ref()).cloned()
    }

    pub fn get_by_absolute(&self, abs: impl AsRef<Path>) -> io::Result<Option<Arc<File>>> {
        match fs::canonicalize(abs.as_ref()) {
            Ok(canon) => Ok(self.by_abs.get(&canon).cloned()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Entry point preserving your original signature. Scans and discards the report.
/// Call `scan_folder_with_report` if you want details about skipped/failed entries.
pub fn scan_folder(root_dir: impl AsRef<Path>) -> io::Result<ScannedFolder> {
    let (store, _report) = scan_folder_with_report(root_dir)?;
    Ok(store)
}

/// Full scan with an accompanying `ScanReport`.
pub fn scan_folder_with_report(
    root_dir: impl AsRef<Path>,
) -> io::Result<(ScannedFolder, ScanReport)> {
    let root = fs::canonicalize(root_dir.as_ref())?;
    let walker = WalkDir::new(&root).follow_links(false).into_iter();

    let mut report = ScanReport::default();

    // Collect candidate DirEntries first (recording per-entry errors).
    // We keep this single-threaded collection of entries (WalkDir itself
    // is single-threaded), but parallelize the *per-entry processing* below.
    let mut entries: Vec<DirEntry> = Vec::new();
    for item in walker {
        match item {
            Ok(e) => {
                report.entries_seen += 1;
                if e.file_type().is_file() {
                    entries.push(e);
                }
            }
            Err(err) => {
                // We can’t get the path reliably here; WalkDir exposes it via error.path()
                let path = err
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.clone());
                report.errors.push(ScanError {
                    stage: "walkdir",
                    path,
                    error: io::Error::new(
                        err.io_error()
                            .map(|e| e.kind())
                            .unwrap_or(io::ErrorKind::Other),
                        err.to_string(),
                    ),
                });
            }
        }
    }

    // Process each entry in parallel:
    #[derive(Debug)]
    struct Item {
        abs: PathBuf,
        rel: PathBuf,
        handle: Option<Handle>,
        err: Option<ScanError>,
    }

    let items: Vec<Item> = entries
        .into_par_iter()
        .map(|e| {
            // Canonicalize
            let abs = match fs::canonicalize(e.path()) {
                Ok(p) => p,
                Err(err) => {
                    return Item {
                        abs: e.path().to_path_buf(),
                        rel: PathBuf::new(),
                        handle: None,
                        err: Some(ScanError {
                            stage: "canonicalize",
                            path: e.path().to_path_buf(),
                            error: err,
                        }),
                    };
                }
            };

            // Derive relative path
            let rel = match abs.strip_prefix(&root) {
                Ok(r) => r.to_path_buf(),
                Err(err) => {
                    return Item {
                        abs: abs.clone(),
                        rel: PathBuf::new(),
                        handle: None,
                        err: Some(ScanError {
                            stage: "strip_prefix",
                            path: abs,
                            error: io::Error::new(io::ErrorKind::Other, err.to_string()),
                        }),
                    };
                }
            };

            // Obtain a handle for hard-link / same-file coalescing.
            // If it fails, we still index by absolute path.
            let handle = match Handle::from_path(&abs) {
                Ok(h) => Some(h),
                Err(err) => {
                    // Treat as non-fatal; just record the error and proceed without a handle.
                    return Item {
                        abs: abs.clone(),
                        rel: rel.clone(),
                        handle: None,
                        err: Some(ScanError {
                            stage: "same_file_handle",
                            path: abs.clone(),
                            error: err,
                        }),
                    };
                }
            };

            Item {
                abs,
                rel,
                handle,
                err: None,
            }
        })
        .collect();

    // Build indices, coalescing by same-file handle when available.
    let mut by_abs: HashMap<PathBuf, Arc<File>> = HashMap::with_capacity(items.len());
    let mut by_rel: HashMap<PathBuf, Arc<File>> = HashMap::with_capacity(items.len());
    let mut by_handle: HashMap<Handle, Arc<File>> = HashMap::new(); // coalesce hard links
    let mut files: Vec<Arc<File>> = Vec::new();

    for it in items {
        if let Some(err) = it.err {
            report.errors.push(err);
            continue;
        }

        // If we got a handle, prefer unifying by that first.
        let arc = if let Some(h) = it.handle {
            if let Some(existing) = by_handle.get(&h) {
                existing.clone()
            } else {
                let f = Arc::new(File {
                    abs: it.abs.clone(),
                    rel: it.rel.clone(),
                });
                by_handle.insert(h, f.clone());
                f
            }
        } else {
            // Fall back to absolute-path-based identity
            match by_abs.get(&it.abs) {
                Some(existing) => existing.clone(),
                None => {
                    let f = Arc::new(File {
                        abs: it.abs.clone(),
                        rel: it.rel.clone(),
                    });
                    f
                }
            }
        };

        // Insert into abs map; only push into `files` if we haven't seen this abs before.
        if let std::collections::hash_map::Entry::Vacant(v) = by_abs.entry(it.abs.clone()) {
            v.insert(arc.clone());
            files.push(arc.clone());
            report.files_indexed += 1;
        }

        // Map the relative path to the same Arc<File> (don’t overwrite existing).
        by_rel.entry(it.rel).or_insert_with(|| arc.clone());
    }

    let store = ScannedFolder {
        root,
        files,
        by_abs,
        by_rel,
    };
    Ok((store, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File as StdFile, io::Write};
    use tempfile::tempdir;

    /// Helper: write text to a file (create parents).
    fn write_text(path: &Path, s: &str) -> io::Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        let mut f = StdFile::create(path)?;
        write!(f, "{}", s)?;
        Ok(())
    }

    #[test]
    fn rel_and_abs_lookup_same_arc_and_lazy_reads() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        // Arrange
        write_text(&root.join("a/one.txt"), "hello")?;
        write_text(&root.join("b/two.txt"), "world")?;

        // Act
        let store = scan_folder(root)?;

        // Positive: find by relative
        let f_rel = store.get_by_relative("a/one.txt").expect("missing rel");
        assert_eq!(f_rel.relative_path(), Path::new("a/one.txt"));

        // Positive: find by absolute — must be the same Arc
        let f_abs = store
            .get_by_absolute(root.join("a/one.txt"))?
            .expect("missing abs");
        assert!(
            Arc::ptr_eq(&f_rel, &f_abs),
            "rel/abs should yield same Arc<File>"
        );

        // Lazy (no cache): modify file then re-read and observe change
        assert_eq!(f_rel.read_string()?, "hello");
        f_rel.write_string("HELLO AGAIN")?;
        assert_eq!(f_abs.read_string()?, "HELLO AGAIN");

        Ok(())
    }

    #[test]
    fn missing_paths_return_none_or_error() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("exists.txt"), "x")?;
        let store = scan_folder(root)?;

        // Missing relative → None
        assert!(store.get_by_relative("nope.txt").is_none());

        // Missing absolute → Ok(None)
        assert!(store.get_by_absolute(root.join("nope.txt"))?.is_none());

        Ok(())
    }

    #[test]
    fn read_string_fails_on_non_utf8() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();
        let bin_path = root.join("bin.dat");

        // Write invalid UTF-8 bytes
        if let Some(parent) = bin_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&bin_path, b"\xff\xfe\xfd")?;

        let store = scan_folder(root)?;
        let f = store.get_by_relative("bin.dat").expect("indexed bin.dat");
        let err = f.read_string().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        // read_bytes still works
        let bytes = f.read_bytes()?;
        assert_eq!(bytes, b"\xff\xfe\xfd");
        Ok(())
    }

    #[test]
    fn symlinks_are_not_followed_as_files() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("real/file.txt"), "ok")?;

        // Create a symlink if the platform allows it.
        // On Unix this is straightforward; on Windows it may require dev mode/admin.
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("real/file.txt"), root.join("link.txt")).ok();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(root.join("real/file.txt"), root.join("link.txt")).ok();

        let store = scan_folder(root)?;

        // The symlink itself should NOT appear as a file when follow_links(false)
        assert!(store.get_by_relative("real/file.txt").is_some());
        assert!(store.get_by_relative("link.txt").is_none());

        Ok(())
    }

    #[test]
    fn hardlink_points_to_same_arc_when_supported() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("a/original.txt"), "v1")?;
        // Try to create a hard link; skip test if the FS refuses.
        if std::fs::hard_link(root.join("a/original.txt"), root.join("b/alias.txt")).is_err() {
            // Hard links unsupported: nothing to assert here; treat as pass.
            return Ok(());
        }

        let store = scan_folder(root)?;

        // Both paths should be indexed
        let f1 = store
            .get_by_relative("a/original.txt")
            .expect("orig missing");
        let f2 = store.get_by_relative("b/alias.txt").expect("alias missing");

        // They should point to the same underlying Arc<File> when same-file unification is enabled.
        assert!(
            Arc::ptr_eq(&f1, &f2),
            "hard-linked files should coalesce to the same Arc<File>"
        );

        // Changing via one reflects in the other (no cache)
        assert_eq!(f1.read_string()?, "v1");
        f2.write_string("v2")?;
        assert_eq!(f1.read_string()?, "v2");

        Ok(())
    }

    #[test]
    fn files_list_contains_indexed_files() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("x/1.txt"), "1")?;
        write_text(&root.join("x/2.txt"), "2")?;
        let store = scan_folder(root)?;

        // Basic sanity: both files are present in an indexable way.
        let rels: std::collections::HashSet<_> = store
            .files()
            .iter()
            .map(|f| f.relative_path().to_path_buf())
            .collect();

        assert!(rels.contains(Path::new("x/1.txt")));
        assert!(rels.contains(Path::new("x/2.txt")));
        Ok(())
    }

    /// Unix-only: make an unreadable directory to ensure we capture WalkDir errors
    /// in `scan_folder_with_report`.
    #[cfg(unix)]
    #[test]
    fn scan_report_captures_walk_errors() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("ok/visible.txt"), "ok")?;
        let blocked = root.join("blocked");
        fs::create_dir_all(&blocked)?;
        // Remove all permissions so the walker cannot read inside it.
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000))?;

        let (_store, report) = scan_folder_with_report(root)?;
        // Restore perms so tempdir can clean up.
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755))?;

        // We saw at least the visible file’s directory entry
        assert!(report.entries_seen >= 1);

        // We should have recorded at least one error from trying to traverse `blocked/`
        assert!(
            !report.errors.is_empty(),
            "expected at least one traversal error from unreadable dir"
        );

        let has_walk_error = report.errors.iter().any(|e| e.stage == "walkdir");
        assert!(has_walk_error, "expected a 'walkdir' stage error");

        Ok(())
    }
}
