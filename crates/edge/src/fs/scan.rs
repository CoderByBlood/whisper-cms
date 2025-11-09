//! Scanning + I/O operations. Constructs `file::ScannedFolder`/`file::File`
//! and performs all filesystem access here (no std::fs in `file.rs`).

use domain::file::{File, FileService, ScanReport, ScanStageError, ScannedFolder};
use rayon::prelude::*;
use regex::Regex;
use same_file::Handle;
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
                    let name = e.file_name().to_string_lossy();
                    return re.is_match(&name);
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
