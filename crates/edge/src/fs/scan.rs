//! Scanning + I/O operations. Constructs `file::ScannedFolder`/`file::File`
//! and performs all filesystem access here (no std::fs in `file.rs`).

use rayon::prelude::*;
use regex::Regex;
use same_file::Handle;
use serve::file::{File, FileService, ScanReport, ScanStageError, ScannedFolder};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};
use walkdir::{DirEntry, WalkDir};

const SRV: FileService = FileService::from_fns(
    read_file_bytes,
    write_file_bytes,
    scan_folder_with_report_and_filters,
    lookup_by_absolute,
);

/// Entry point preserving original signature (no filters). Discards the report.
pub fn scan_folder(root_dir: &Path) -> io::Result<ScannedFolder> {
    scan_folder_with_report_and_filters(root_dir, None, None).map(|(s, _)| s)
}

/// Full scan with a `ScanReport` (no filters).
pub fn scan_folder_with_report(root_dir: &Path) -> io::Result<(ScannedFolder, ScanReport)> {
    scan_folder_with_report_and_filters(root_dir, None, None)
}

/// Filtered scan (regexes on directory/file names).
pub fn scan_folder_with_filters(
    root_dir: &Path,
    dir_name_re: Option<&Regex>,
    file_name_re: Option<&Regex>,
) -> io::Result<ScannedFolder> {
    scan_folder_with_report_and_filters(root_dir, dir_name_re, file_name_re).map(|(s, _)| s)
}

/// Filtered scan that also returns a `ScanReport`.
pub fn scan_folder_with_report_and_filters(
    root_dir: &Path,
    dir_name_re: Option<&Regex>,
    file_name_re: Option<&Regex>,
) -> io::Result<(ScannedFolder, ScanReport)> {
    let root = fs::canonicalize(root_dir)?;
    let mut report = ScanReport::default();

    // Prune by directory name. Always allow the root (depth == 0).
    let walker = WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if let Some(re) = dir_name_re {
                if e.file_type().is_dir() {
                    // If this directory or any of its ancestors (under root) matches,
                    // traverse it (and thus its whole subtree).
                    let rel = e.path().strip_prefix(&root).unwrap_or(e.path());
                    for comp in rel.components() {
                        let name = comp.as_os_str().to_string_lossy();
                        if re.is_match(&name) {
                            return true;
                        }
                    }
                    return false;
                }
            }
            true
        });

    // Gather file entries (respect file-name filter); record walk errors.
    let mut entries: Vec<DirEntry> = Vec::new();
    for item in walker {
        match item {
            Ok(e) => {
                report.entries_seen += 1;
                if e.file_type().is_file() {
                    if let Some(re) = file_name_re {
                        let name = e.file_name().to_string_lossy();
                        if !re.is_match(&name) {
                            continue;
                        }
                    }
                    entries.push(e);
                }
            }
            Err(err) => {
                let path = err
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.clone());
                report.errors.push(ScanStageError::WalkDir {
                    path,
                    source: io::Error::new(
                        err.io_error()
                            .map(|e| e.kind())
                            .unwrap_or(io::ErrorKind::Other),
                        err.to_string(),
                    ),
                });
            }
        }
    }

    // Per-entry work in parallel.
    #[derive(Debug)]
    struct Item {
        abs: PathBuf,
        rel: PathBuf,
        handle: Option<Handle>,
        err: Option<ScanStageError>,
    }

    let items: Vec<Item> = entries
        .into_par_iter()
        .map(|e| {
            // Canonicalize
            let abs = match fs::canonicalize(e.path()) {
                Ok(p) => p,
                Err(source) => {
                    return Item {
                        abs: e.path().to_path_buf(),
                        rel: PathBuf::new(),
                        handle: None,
                        err: Some(ScanStageError::Canonicalize {
                            path: e.path().to_path_buf(),
                            source,
                        }),
                    };
                }
            };

            // Relative to root
            let rel = match abs.strip_prefix(&root) {
                Ok(r) => r.to_path_buf(),
                Err(e) => {
                    return Item {
                        abs: abs.clone(),
                        rel: PathBuf::new(),
                        handle: None,
                        err: Some(ScanStageError::StripPrefix {
                            path: abs,
                            source: io::Error::new(io::ErrorKind::Other, e.to_string()),
                        }),
                    };
                }
            };

            // Same-file handle (non-fatal if it fails)
            let handle = match Handle::from_path(&abs) {
                Ok(h) => Some(h),
                Err(source) => {
                    return Item {
                        abs: abs.clone(),
                        rel: rel.clone(),
                        handle: None,
                        err: Some(ScanStageError::SameFileHandle {
                            path: abs.clone(),
                            source,
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

    // Build indices with hard-link coalescing.
    let mut by_abs: HashMap<PathBuf, Arc<File>> = HashMap::with_capacity(items.len());
    let mut by_rel: HashMap<PathBuf, Arc<File>> = HashMap::with_capacity(items.len());
    let mut by_handle: HashMap<Handle, Arc<File>> = HashMap::new();
    let mut files: Vec<Arc<File>> = Vec::new();

    for it in items {
        if let Some(err) = it.err {
            report.errors.push(err);
            continue;
        }

        // Unify by handle first.
        let arc = if let Some(h) = it.handle {
            if let Some(existing) = by_handle.get(&h) {
                existing.clone()
            } else {
                let f = Arc::new(File::new(it.abs.clone(), it.rel.clone(), SRV));
                by_handle.insert(h, f.clone());
                f
            }
        } else if let Some(existing) = by_abs.get(&it.abs) {
            existing.clone()
        } else {
            Arc::new(File::new(it.abs.clone(), it.rel.clone(), SRV))
        };

        // Insert into abs map; push to files only on first sighting.
        if let std::collections::hash_map::Entry::Vacant(v) = by_abs.entry(it.abs.clone()) {
            v.insert(arc.clone());
            files.push(arc.clone());
            report.files_indexed += 1;
        }
        // Map relative to same Arc (don't overwrite).
        by_rel.entry(it.rel).or_insert_with(|| arc.clone());
    }

    let store = ScannedFolder::new(
        root,
        files,
        by_abs,
        by_rel,
        dir_name_re.cloned(),
        file_name_re.cloned(),
        SRV,
    );
    Ok((store, report))
}

// ===== Helper functions used by model (thin, testable) =====

#[inline]
pub fn read_file_bytes(abs: &Path) -> io::Result<Vec<u8>> {
    // Ensure parent exists/permissions are handled by caller when writing.
    fs::read(abs)
}

#[inline]
pub fn write_file_bytes(abs: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(abs, data)
}

#[inline]
pub fn lookup_by_absolute(
    by_abs: &HashMap<PathBuf, Arc<File>>,
    abs: &Path,
) -> io::Result<Option<Arc<File>>> {
    match fs::canonicalize(abs) {
        Ok(canon) => Ok(by_abs.get(&canon).cloned()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::collections::HashSet;
    use std::fs::File as StdFile;
    use std::io::Write;
    use tempfile::tempdir;

    // -------- helpers --------

    fn write_text(path: &Path, s: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = StdFile::create(path)?;
        write!(f, "{s}")?;
        Ok(())
    }

    fn relset(store: &ScannedFolder) -> HashSet<PathBuf> {
        store
            .files()
            .iter()
            .map(|f| f.relative_path().to_path_buf())
            .collect()
    }

    // -------- scan_folder / scan_folder_with_report --------

    #[test]
    fn scan_folder_basic_index_and_lazy_read_write() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("a/one.txt"), "hello")?;
        write_text(&root.join("b/two.txt"), "world")?;

        // Basic scan (no filters)
        let store = scan_folder(root)?;
        let rels = relset(&store);
        assert!(rels.contains(Path::new("a/one.txt")));
        assert!(rels.contains(Path::new("b/two.txt")));

        // Rel lookup
        let f_rel = store
            .get_by_relative(Path::new("a/one.txt"))
            .expect("rel missing");
        assert_eq!(f_rel.read_string()?, "hello");

        // Abs lookup (canonical)
        let f_abs = store
            .get_by_absolute(&root.join("a/one.txt"))?
            .expect("abs missing");
        assert!(Arc::ptr_eq(&f_rel, &f_abs));

        // Write via one handle, observe via the other (no cache)
        f_abs.write_string("HELLO")?;
        assert_eq!(f_rel.read_string()?, "HELLO");

        Ok(())
    }

    #[test]
    fn scan_folder_with_report_counts_and_no_errors() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();
        write_text(&root.join("x/1.txt"), "1")?;
        write_text(&root.join("x/2.txt"), "2")?;
        write_text(&root.join("y/3.txt"), "3")?;

        let (_store, report) = scan_folder_with_report(root)?;
        assert!(report.errors.is_empty());
        // WalkDir visits dirs and files; entries_seen >= files_indexed
        assert!(report.entries_seen >= report.files_indexed);
        assert_eq!(report.files_indexed, 3);

        Ok(())
    }

    // -------- filters --------

    #[test]
    fn dir_name_filter_limits_traversal() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("keep/sub/a.txt"), "A")?;
        write_text(&root.join("skip/sub/b.txt"), "B")?;

        let dir_re = Regex::new(r"^keep$").unwrap();
        let store = scan_folder_with_filters(root, Some(&dir_re), None)?;
        let rels = relset(&store);

        assert!(rels.contains(Path::new("keep/sub/a.txt")));
        assert!(!rels.contains(Path::new("skip/sub/b.txt")));
        Ok(())
    }

    #[test]
    fn file_name_filter_limits_inclusion() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("keep/a.md"), "A")?;
        write_text(&root.join("keep/b.html"), "B")?;
        write_text(&root.join("keep/c.png"), "C")?;

        let file_re = Regex::new(r"(?i)\.(md|html)$").unwrap();
        let store = scan_folder_with_filters(root, None, Some(&file_re))?;
        let rels = relset(&store);

        assert!(rels.contains(Path::new("keep/a.md")));
        assert!(rels.contains(Path::new("keep/b.html")));
        assert!(!rels.contains(Path::new("keep/c.png")));
        Ok(())
    }

    #[test]
    fn both_filters_together() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("posts/a.md"), "A")?;
        write_text(&root.join("posts/b.txt"), "B")?;
        write_text(&root.join("pages/home.html"), "<html>")?;
        write_text(&root.join("other/skip.md"), "X")?;

        let dir_re = Regex::new(r"^(posts|pages)$").unwrap();
        let file_re = Regex::new(r"(?i)\.(md|html)$").unwrap();

        let store = scan_folder_with_filters(root, Some(&dir_re), Some(&file_re))?;
        let rels = relset(&store);

        assert!(rels.contains(Path::new("posts/a.md")));
        assert!(rels.contains(Path::new("pages/home.html")));
        assert!(!rels.contains(Path::new("posts/b.txt"))); // filtered by file regex
        assert!(!rels.contains(Path::new("other/skip.md"))); // filtered by dir regex
        Ok(())
    }

    // -------- symlinks and hard links --------

    #[test]
    fn symlinks_are_not_followed_as_files() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("real/file.txt"), "ok")?;

        // Create symlink if platform allows; ignore failure gracefully.
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("real/file.txt"), root.join("link.txt")).ok();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(root.join("real/file.txt"), root.join("link.txt")).ok();

        let store = scan_folder(root)?;
        let rels = relset(&store);

        assert!(rels.contains(Path::new("real/file.txt")));
        assert!(!rels.contains(Path::new("link.txt"))); // not followed
        Ok(())
    }

    #[test]
    fn hard_links_coalesce_to_same_arc_when_supported() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("a/original.txt"), "v1")?;

        // Try to create a hard link; if unsupported, skip the identity assertion.
        match std::fs::hard_link(root.join("a/original.txt"), root.join("b/alias.txt")) {
            Ok(()) => {
                let store = scan_folder(root)?;
                let f1 = store
                    .get_by_relative(Path::new("a/original.txt"))
                    .expect("orig missing");
                let f2 = store
                    .get_by_relative(Path::new("b/alias.txt"))
                    .expect("alias missing");
                assert!(
                    Arc::ptr_eq(&f1, &f2),
                    "hard-linked files should coalesce to the same Arc<File>"
                );

                // Update via one, observe via the other (no cache)
                assert_eq!(f1.read_string()?, "v1");
                f2.write_string("v2")?;
                assert_eq!(f1.read_string()?, "v2");
            }
            Err(_) => {
                // Hard links not supported; accept as pass
            }
        }
        Ok(())
    }

    // -------- error reporting from WalkDir (Unix-only, permissions) --------

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_emits_walkdir_error_in_report() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("ok/visible.txt"), "ok")?;
        let blocked = root.join("blocked");
        fs::create_dir_all(&blocked)?;
        // Remove all permissions so the walker cannot descend
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000))?;

        let (_store, report) = scan_folder_with_report(root)?;
        // Restore perms so tempdir can clean up.
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755))?;

        assert!(
            !report.errors.is_empty(),
            "expected at least one traversal error from unreadable dir"
        );
        let has_walk_error = report
            .errors
            .iter()
            .any(|e| matches!(e, ScanStageError::WalkDir { .. }));
        assert!(has_walk_error, "expected a WalkDir stage error");
        Ok(())
    }

    // -------- helpers: read_file_bytes / write_file_bytes / lookup_by_absolute --------

    #[test]
    fn helper_read_write_file_bytes_create_parents_and_roundtrip() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        let nested = root.join("nested/dir/data.bin");
        write_file_bytes(&nested, b"\x01\x02\x03")?;
        let bytes = read_file_bytes(&nested)?;
        assert_eq!(bytes, b"\x01\x02\x03");

        Ok(())
    }

    #[test]
    fn helper_lookup_by_absolute_uses_canonicalization() -> io::Result<()> {
        let dir = tempdir()?;
        let root = fs::canonicalize(dir.path())?;

        write_text(&root.join("a/one.txt"), "1")?;
        let store = scan_folder(&root)?;

        // Build a non-canonical path to the same file (e.g., with "./")
        let noncanon = root.join("a").join("./one.txt");

        // The folder should contain the canonical key; lookup should still succeed.
        let f = store
            .get_by_absolute(&noncanon)?
            .expect("lookup failed via non-canonical path");
        assert_eq!(f.read_string()?, "1");

        // Missing path → Ok(None)
        assert!(store.get_by_absolute(&root.join("missing.txt"))?.is_none());

        Ok(())
    }

    // -------- refresh semantics (remember filters) --------

    #[test]
    fn refresh_reuses_filters_and_picks_up_new_files() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("posts/a.md"), "A")?;
        let dir_re = Regex::new(r"^posts$").unwrap();
        let file_re = Regex::new(r"(?i)\.md$").unwrap();

        let (store, _rpt) =
            scan_folder_with_report_and_filters(root, Some(&dir_re), Some(&file_re))?;
        let rels = relset(&store);
        assert!(rels.contains(Path::new("posts/a.md")));
        assert!(!rels.contains(Path::new("posts/b.html")));

        // Add a matching file and refresh
        write_text(&root.join("posts/new.md"), "N")?;
        let store2 = store.refresh()?;
        let rels2 = relset(&store2);

        assert!(rels2.contains(Path::new("posts/new.md")));
        Ok(())
    }
}
