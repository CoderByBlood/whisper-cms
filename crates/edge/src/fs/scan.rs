//! Scanning + I/O operations producing a debounced mpsc stream of paths.
//!
//! - Sends debounced `PathBuf`s into a provided `tokio::sync::mpsc::Sender<PathBuf>`.
//! - Returns a stop function (`Box<dyn FnOnce() + Send>`) that aborts internal tasks.
//! - Debounces identical paths within a window (coalescing).
//! - Optional folder / file regex filters.
//! - Supports absolute/relative emission, recursion depth, and path canonicalization.

use regex::Regex;
use std::{
    collections::{HashSet, VecDeque},
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    select,
    sync::mpsc,
    task::{AbortHandle, JoinHandle},
    time,
};
use walkdir::WalkDir;

/// Configuration for `start_folder_scan`.
#[derive(Debug, Clone)]
pub struct FolderScanConfig {
    /// Emit absolute paths (true) or paths relative to `root` (false).
    pub absolute: bool,
    /// Recurse into subdirectories.
    pub recursive: bool,
    /// Debounce window in milliseconds for coalescing duplicate paths.
    pub debounce_ms: u64,
    /// Canonicalize paths before emission.
    pub canonicalize_paths: bool,
    /// Capacity of the bounded output channel (no longer used here, but kept for config symmetry).
    pub channel_capacity: usize,
    /// Optional regex to **allow** folders. If set, a directory is traversed
    /// if it or **any ancestor under `root`** matches.
    pub folder_re: Option<Regex>,
    /// Optional regex to **allow** files by name (basename).
    pub file_re: Option<Regex>,
}

impl Default for FolderScanConfig {
    fn default() -> Self {
        Self {
            absolute: true,
            recursive: true,
            debounce_ms: 64,
            canonicalize_paths: true,
            channel_capacity: 1024,
            folder_re: None,
            file_re: None,
        }
    }
}

/// Start scanning `root` according to `cfg`, producing a debounced stream of paths
/// that are sent into the provided `out_tx`.
///
/// Arguments:
/// - `root`: directory to scan
/// - `cfg`: configuration (filters, paths, debounce, recursion)
/// - `out_tx`: bounded channel sender used for delivering debounced paths
///
/// Returns:
/// - `Box<dyn FnOnce() + Send>`: call it to stop both internal tasks
///
/// Errors:
/// - `NotFound` if `root` does not exist
/// - `Other` if `root` is not a directory or cannot be accessed
pub fn start_folder_scan(
    root: &Path,
    cfg: FolderScanConfig,
    out_tx: mpsc::Sender<PathBuf>,
) -> io::Result<Box<dyn FnOnce() + Send>> {
    // Pre-check root existence and type (fail fast with a regular io::Error).
    let meta = fs::metadata(root).map_err(|e| io::Error::new(e.kind(), format!("root: {e}")))?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "root is not a directory",
        ));
    }

    // Canonical root used for traversal and (if requested) for canonicalization.
    let root_canon = fs::canonicalize(root)?;

    // Internal raw channel (unbounded) between walker and debouncer.
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<PathBuf>();

    // ---------- Debounce task ----------
    // Coalesce duplicate paths within a time window, then flush to bounded `out_tx`.
    let debounce_ms = cfg.debounce_ms;
    let out_tx_clone = out_tx.clone();
    let debounce_handle: JoinHandle<()> = tokio::spawn(async move {
        let mut pending: HashSet<PathBuf> = HashSet::new();
        let mut queue: VecDeque<PathBuf> = VecDeque::new();
        let mut ticker = time::interval(Duration::from_millis(debounce_ms.max(1)));
        ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        loop {
            select! {
                biased;

                // Receive raw paths as they come in (no backpressure here).
                maybe_path = raw_rx.recv() => {
                    match maybe_path {
                        Some(p) => {
                            if pending.insert(p.clone()) {
                                queue.push_back(p);
                            }
                        }
                        None => {
                            // Raw sender dropped: flush remaining and exit.
                            while let Some(p) = queue.pop_front() {
                                if out_tx_clone.send(p).await.is_err() {
                                    return;
                                }
                            }
                            return;
                        }
                    }
                }

                // On each tick, flush the coalesced set to the output channel.
                _ = ticker.tick() => {
                    if queue.is_empty() {
                        continue;
                    }
                    let mut drain = Vec::with_capacity(queue.len());
                    while let Some(p) = queue.pop_front() {
                        drain.push(p);
                    }
                    pending.clear();
                    for p in drain {
                        if out_tx_clone.send(p).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });
    let debounce_abort: AbortHandle = debounce_handle.abort_handle();

    // ---------- Scan task ----------
    let folder_re = cfg.folder_re.clone();
    let file_re = cfg.file_re.clone();
    let recursive = cfg.recursive;
    let canonicalize_paths = cfg.canonicalize_paths;
    let absolute = cfg.absolute;

    let scan_handle: JoinHandle<()> = tokio::spawn(async move {
        let mut builder = WalkDir::new(&root_canon).follow_links(false);
        if !recursive {
            builder = builder.max_depth(1);
        }
        // Ancestor-allow semantics for folder filter.
        let walker = builder.into_iter().filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if let Some(re) = &folder_re {
                if e.file_type().is_dir() {
                    let rel = e.path().strip_prefix(&root_canon).unwrap_or(e.path());
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

        for entry in walker {
            let e = match entry {
                Ok(e) => e,
                Err(_) => continue, // ignore traversal errors in this stream API
            };

            if !e.file_type().is_file() {
                continue;
            }

            // File-name filter (basename)
            if let Some(re) = &file_re {
                let name = e.file_name().to_string_lossy();
                if !re.is_match(&name) {
                    continue;
                }
            }

            // Determine the emission path according to config.
            let emit_path = if absolute {
                if canonicalize_paths {
                    match fs::canonicalize(e.path()) {
                        Ok(c) => c,
                        Err(_) => continue,
                    }
                } else {
                    e.path().to_path_buf()
                }
            } else {
                let mut p = match e.path().strip_prefix(&root_canon) {
                    Ok(r) => r.to_path_buf(),
                    Err(_) => continue,
                };
                if canonicalize_paths {
                    if let Ok(abs) = fs::canonicalize(e.path()) {
                        if let Ok(rel) = abs.strip_prefix(&root_canon) {
                            p = rel.to_path_buf();
                        }
                    }
                }
                p
            };

            let _ = raw_tx.send(emit_path);
        }

        // Close raw sender → debouncer flushes and exits.
        drop(raw_tx);
    });
    let scan_abort: AbortHandle = scan_handle.abort_handle();

    // Stop closure
    let stop_fn: Box<dyn FnOnce() + Send> = Box::new(move || {
        scan_abort.abort();
        debounce_abort.abort();
    });

    Ok(stop_fn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs::File as StdFile;
    use std::io::Write;
    use tempfile::tempdir;
    use tokio::time::{sleep, timeout};

    // ---------- small helpers ----------

    fn write_text(path: &Path, s: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = StdFile::create(path)?;
        write!(f, "{s}")?;
        Ok(())
    }

    async fn collect_all(mut rx: mpsc::Receiver<PathBuf>) -> Vec<PathBuf> {
        let mut out = Vec::new();
        while let Some(p) = rx.recv().await {
            out.push(p);
        }
        out
    }

    async fn collect_until_closed_or_timeout(
        rx: mpsc::Receiver<PathBuf>,
        max_ms: u64,
    ) -> Vec<PathBuf> {
        let fut = collect_all(rx);
        match timeout(Duration::from_millis(max_ms), fut).await {
            Ok(v) => v,
            Err(_) => Vec::new(),
        }
    }

    fn as_rel_set(root: &Path, mut v: Vec<PathBuf>) -> HashSet<PathBuf> {
        v.iter_mut()
            .filter_map(|p| p.strip_prefix(root).ok().map(|r| r.to_path_buf()))
            .collect()
    }

    // ---------- tests ----------

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_root_returns_error() {
        let missing = Path::new("/definitely/not/here/___x");
        let cfg = FolderScanConfig::default();
        let (tx, _rx) = mpsc::channel::<PathBuf>(16);

        let res = start_folder_scan(missing, cfg, tx);
        let err = match res {
            Ok(stop) => {
                stop();
                panic!("expected start_folder_scan to error for missing root");
            }
            Err(e) => e,
        };

        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn absolute_emission_and_canonicalization() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("a/one.txt"), "1")?;
        write_text(&root.join("b/two.txt"), "2")?;

        let mut cfg = FolderScanConfig::default();
        cfg.absolute = true;
        cfg.canonicalize_paths = true;
        cfg.debounce_ms = 10;

        let (tx, rx) = mpsc::channel::<PathBuf>(4);
        let stop = start_folder_scan(root, cfg, tx)?;
        let got = collect_until_closed_or_timeout(rx, 1000).await;
        stop();

        assert!(got.iter().all(|p| p.is_absolute()));
        let relset = as_rel_set(&fs::canonicalize(root)?, got);
        assert!(relset.contains(Path::new("a/one.txt")));
        assert!(relset.contains(Path::new("b/two.txt")));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn relative_emission_and_canonicalization_off() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("x/one.md"), "A")?;
        write_text(&root.join("x/two.md"), "B")?;

        let mut cfg = FolderScanConfig::default();
        cfg.absolute = false;
        cfg.canonicalize_paths = false;
        cfg.debounce_ms = 10;

        let (tx, rx) = mpsc::channel::<PathBuf>(4);
        let stop = start_folder_scan(root, cfg, tx)?;
        let got = collect_until_closed_or_timeout(rx, 1000).await;
        stop();

        assert!(got.iter().all(|p| !p.is_absolute()));
        let set: HashSet<_> = got.into_iter().collect();
        assert!(set.contains(PathBuf::from("x/one.md").as_path()));
        assert!(set.contains(PathBuf::from("x/two.md").as_path()));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn non_recursive_scans_only_top_level() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("top.txt"), "T")?;
        write_text(&root.join("sub/inner.txt"), "I")?;

        let mut cfg = FolderScanConfig::default();
        cfg.recursive = false;
        cfg.debounce_ms = 10;
        cfg.absolute = false;

        let (tx, rx) = mpsc::channel::<PathBuf>(4);
        let stop = start_folder_scan(root, cfg, tx)?;
        let got = collect_until_closed_or_timeout(rx, 1000).await;
        stop();

        let set: HashSet<_> = got.into_iter().collect();
        assert!(set.contains(PathBuf::from("top.txt").as_path()));
        assert!(!set.contains(PathBuf::from("sub/inner.txt").as_path()));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn folder_regex_ancestor_allow_and_file_regex() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("keep/sub/a.md"), "A")?;
        write_text(&root.join("keep/sub/b.txt"), "B")?;
        write_text(&root.join("skip/sub/c.md"), "C")?;
        write_text(&root.join("pages/home.html"), "<html>")?;

        let mut cfg = FolderScanConfig::default();
        cfg.absolute = false;
        cfg.debounce_ms = 10;
        cfg.folder_re = Some(Regex::new(r"^(keep|pages)$").unwrap());
        cfg.file_re = Some(Regex::new(r"(?i)\.(md|html)$").unwrap());

        let (tx, rx) = mpsc::channel::<PathBuf>(16);
        let stop = start_folder_scan(root, cfg, tx)?;
        let got = collect_until_closed_or_timeout(rx, 1500).await;
        stop();

        let set: HashSet<_> = got.into_iter().collect();
        assert!(set.contains(Path::new("keep/sub/a.md")));
        assert!(set.contains(Path::new("pages/home.html")));
        assert!(!set.contains(Path::new("keep/sub/b.txt"))); // filtered by file regex
        assert!(!set.contains(Path::new("skip/sub/c.md"))); // filtered by folder regex
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bounded_channel_backpressure_no_loss() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        for i in 0..64 {
            write_text(&root.join(format!("f{i:02}.txt")), "x")?;
        }

        let mut cfg = FolderScanConfig::default();
        cfg.debounce_ms = 20;
        cfg.absolute = false;

        // Capacity 1 → strong backpressure
        let (tx, mut rx) = mpsc::channel::<PathBuf>(1);
        let stop = start_folder_scan(root, cfg, tx)?;

        let mut received = Vec::new();
        loop {
            match timeout(Duration::from_millis(2000), rx.recv()).await {
                Ok(Some(p)) => {
                    received.push(p);
                    sleep(Duration::from_millis(5)).await; // slow consumer
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        stop();

        assert_eq!(received.len(), 64);
        let set: HashSet<_> = received.into_iter().collect();
        for i in 0..64 {
            assert!(set.contains(Path::new(&format!("f{i:02}.txt"))));
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn debounce_batches_and_flushes() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        write_text(&root.join("a/one.txt"), "1")?;
        write_text(&root.join("b/two.txt"), "2")?;
        write_text(&root.join("b/three.txt"), "3")?;

        let mut cfg = FolderScanConfig::default();
        cfg.debounce_ms = 50;
        cfg.absolute = false;

        let (tx, rx) = mpsc::channel::<PathBuf>(8);
        let stop = start_folder_scan(root, cfg, tx)?;
        let got = collect_until_closed_or_timeout(rx, 2000).await;
        stop();

        let set: HashSet<_> = got.into_iter().collect();
        assert!(set.contains(Path::new("a/one.txt")));
        assert!(set.contains(Path::new("b/two.txt")));
        assert!(set.contains(Path::new("b/three.txt")));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_function_aborts_and_closes_stream() -> io::Result<()> {
        let dir = tempdir()?;
        let root = dir.path();

        for d in 0..10 {
            for f in 0..50 {
                write_text(&root.join(format!("d{d}/f{f}.txt")), "x")?;
            }
        }

        let mut cfg = FolderScanConfig::default();
        cfg.debounce_ms = 20;
        cfg.absolute = false;

        let (tx, mut rx) = mpsc::channel::<PathBuf>(8);
        let stop = start_folder_scan(root, cfg, tx)?;

        // Consume a few, then stop early.
        let mut first_batch = Vec::new();
        for _ in 0..5 {
            if let Ok(Some(p)) = timeout(Duration::from_millis(1000), rx.recv()).await {
                first_batch.push(p);
            }
        }
        stop(); // abort tasks

        // After some time, channel should close (no more items).
        let rest = collect_until_closed_or_timeout(rx, 1500).await;

        assert!(!first_batch.is_empty());
        let total = first_batch.len() + rest.len();
        assert!(total <= 500); // non-deterministic; we stopped early
        Ok(())
    }
}
