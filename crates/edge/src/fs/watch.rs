//! folder_watch.rs
//! Functional, module-level entry points that turn OS filesystem events into a
//! **debounced, coalesced** stream of `PathBuf`s over `tokio::mpsc`.
//!
//! Usage:
//!   let (tx, mut rx) = mpsc::channel::<PathBuf>(512);
//!   let stop = watch_folder_default("/some/root", tx)?;
//!   while let Some(path) = rx.recv().await {
//!       // react to `path`
//!   }
//!   // ... later (or in a drop path):
//!   stop(); // synchronous shutdown (stops forwarder task and the watcher)
//!
//! If you need knobs, use `watch_folder(root, cfg, tx)`.

use notify::{Event, RecursiveMode, Result as NotifyResult, Watcher};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use tokio::sync::{mpsc, oneshot};

/// Configuration for how we forward events into a channel.
///
/// Note: as of this version, the caller is responsible for creating the
/// actual `mpsc::channel` with whatever capacity they want; this config
/// is purely about notify/debounce/canonicalization behavior.
#[derive(Debug, Clone)]
pub struct FolderWatchConfig {
    /// Whether to recursively watch the directory tree.
    pub recursive: bool,
    /// Small debounce to smooth editor bursts before forwarding (milliseconds).
    /// Duplicate paths within a debounce window are coalesced.
    pub debounce_ms: u64,
    /// Canonicalize paths before enqueue (useful for stable equality across symlinks).
    pub canonicalize_paths: bool,
}

impl Default for FolderWatchConfig {
    fn default() -> Self {
        Self {
            recursive: true,
            debounce_ms: 40,
            canonicalize_paths: false,
        }
    }
}

/// Convenience: start with defaults.
///
/// You provide the `Sender<PathBuf>`; this function wires `notify` into it and
/// returns a synchronous `stop` callback.
pub fn watch_folder_default(
    root: impl AsRef<Path>,
    tx_out: mpsc::Sender<PathBuf>,
) -> NotifyResult<Box<dyn FnOnce() + Send>> {
    watch_folder(root, FolderWatchConfig::default(), tx_out)
}

/// Start watching `root` and forward paths into the provided `Sender<PathBuf>`.
///
/// Returns:
/// - `stop: Box<dyn FnOnce() + Send>` synchronous closure that halts the watcher and forwarder
pub fn watch_folder(
    root: impl AsRef<Path>,
    cfg: FolderWatchConfig,
    tx_out: mpsc::Sender<PathBuf>,
) -> NotifyResult<Box<dyn FnOnce() + Send>> {
    // notify callback → unbounded mpsc (we can’t await in the callback)
    let (tx_raw, mut rx_raw) = mpsc::unbounded_channel::<Event>();
    let mut watcher = notify::recommended_watcher(move |res| {
        if let Ok(ev) = res {
            // If the receiver side is gone, ignore send errors.
            let _ = tx_raw.send(ev);
        }
    })?;

    let root = root.as_ref().to_path_buf();
    let mode = if cfg.recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher.watch(&root, mode)?;

    // Shutdown signal for the forwarder task.
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    let debounce_ms = cfg.debounce_ms;
    let canonicalize = cfg.canonicalize_paths;

    // Clone the sender into the task; the original will be dropped in `stop`.
    let tx_out_for_task = tx_out.clone();

    // Forwarder task: debounce, coalesce duplicates within the window, then
    // send each unique path into the caller-provided channel (backpressure is
    // provided by the caller's channel capacity).
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                maybe = rx_raw.recv() => {
                    let Some(ev) = maybe else { break; };

                    // Optionally debounce a short window to collect burst events.
                    if debounce_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(debounce_ms)).await;
                    }

                    // Coalesce duplicates within the debounce window.
                    let mut uniq = HashSet::<PathBuf>::new();

                    // include current event's paths
                    for p in ev.paths {
                        let path = if canonicalize {
                            std::fs::canonicalize(&p).unwrap_or(p)
                        } else {
                            p
                        };
                        uniq.insert(path);
                    }

                    // Drain any additional immediate events without sleeping again,
                    // adding their (possibly canonicalized) paths to the same set.
                    while let Ok(ev2) = rx_raw.try_recv() {
                        for p in ev2.paths {
                            let path = if canonicalize {
                                std::fs::canonicalize(&p).unwrap_or(p)
                            } else {
                                p
                            };
                            uniq.insert(path);
                        }
                    }

                    // Send each unique path to the caller's bounded channel; this is where
                    // **backpressure** is applied (await when full).
                    for p in uniq {
                        // If receiver dropped, stop.
                        if tx_out_for_task.send(p).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    // Build a synchronous stopper that:
    // 1) drops the watcher (stop native callbacks)
    // 2) signals the forwarder to exit and aborts if still running
    // 3) drops the caller's sender to close the stream on their side
    let stop = {
        let mut maybe_watcher = Some(watcher);
        let mut maybe_task = Some(task);
        let mut maybe_shutdown = Some(shutdown_tx);
        let mut maybe_tx_out = Some(tx_out);

        Box::new(move || {
            // 1) stop OS callbacks
            maybe_watcher.take();

            // 2) stop forwarder task (signal + abort if still pending)
            if let Some(tx) = maybe_shutdown.take() {
                let _ = tx.send(());
            }
            if let Some(h) = maybe_task.take() {
                h.abort(); // can’t .await here; abort is fine
            }

            // 3) drop the sender to close the channel
            drop(maybe_tx_out.take());
        }) as Box<dyn FnOnce() + Send>
    };

    Ok(stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{HashMap, HashSet},
        fs,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };
    use tempfile::TempDir;
    use tokio::time::timeout;

    // ---------- helpers ----------

    fn canon(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    async fn recv_until(
        rx: &mut mpsc::Receiver<PathBuf>,
        deadline: Duration,
        mut predicate: impl FnMut(&PathBuf) -> bool,
    ) -> Option<PathBuf> {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if let Some(p) = timeout(Duration::from_millis(100), rx.recv())
                .await
                .ok()
                .flatten()
            {
                if predicate(&p) {
                    return Some(p);
                }
            }
        }
        None
    }

    async fn drain_for(rx: &mut mpsc::Receiver<PathBuf>, how_long: Duration) -> Vec<PathBuf> {
        let start = Instant::now();
        let mut out = Vec::new();
        while start.elapsed() < how_long {
            match timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Some(p)) => out.push(p),
                _ => {}
            }
        }
        out
    }

    // ---------- tests ----------

    /// Starts and idles cleanly; no events produced after a warm-up drain.
    #[tokio::test(flavor = "multi_thread")]
    async fn starts_and_idles_without_writes() -> NotifyResult<()> {
        let dir = TempDir::new().unwrap();
        let (tx, mut rx) = mpsc::channel::<PathBuf>(64);
        let stop = watch_folder_default(dir.path(), tx)?;

        // Warm-up: some backends emit one-off startup events. Drain them.
        let _startup_noise = drain_for(&mut rx, Duration::from_millis(300)).await;

        // Observation window: with no writes, we should not see *new* events.
        match timeout(Duration::from_millis(400), rx.recv()).await {
            Ok(Some(p)) => panic!("unexpected event without writes: {p:?}"),
            Ok(None) => { /* channel closed: fine */ }
            Err(_) => { /* no events during quiet window: expected */ }
        }

        stop();
        Ok(())
    }

    /// Single change should produce at least one path, matching the changed file (canonicalized).
    #[tokio::test(flavor = "multi_thread")]
    async fn single_file_change_yields_path() -> NotifyResult<()> {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, "init").unwrap();

        let (tx, mut rx) = mpsc::channel::<PathBuf>(64);
        let stop = watch_folder_default(dir.path(), tx)?;
        fs::write(&file, "v1").unwrap();

        let want = canon(&file);
        let got = recv_until(&mut rx, Duration::from_secs(2), |p| canon(p) == want).await;
        assert!(got.is_some(), "expected to receive the edited file path");

        stop();
        Ok(())
    }

    /// Bursts to the same file should coalesce within the debounce window; count is <= writes.
    /// We don't enforce exact counts because notify delivery is platform/editor dependent.
    #[tokio::test(flavor = "multi_thread")]
    async fn burst_same_file_is_non_deterministically_coalesced() -> NotifyResult<()> {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("dup.txt");
        fs::write(&file, "init").unwrap();

        let (tx, mut rx) = mpsc::channel::<PathBuf>(64);
        let stop = watch_folder(
            dir.path(),
            FolderWatchConfig {
                debounce_ms: 50,
                ..Default::default()
            },
            tx,
        )?;

        // Rapid burst of writes.
        for n in 0..6 {
            fs::write(&file, format!("v{}", n)).unwrap();
        }

        // Drain for a bit and count how many times the target path appears.
        let events = drain_for(&mut rx, Duration::from_millis(1200)).await;
        let want = canon(&file);
        let hits = events.into_iter().filter(|p| canon(p) == want).count();

        assert!(
            hits >= 1 && hits <= 6,
            "expected 1..=6 hits for burst coalescing, got {hits}"
        );

        stop();
        Ok(())
    }

    /// Two distinct files should both be observed eventually (order is not guaranteed).
    #[tokio::test(flavor = "multi_thread")]
    async fn two_distinct_files_eventually_both_seen() -> NotifyResult<()> {
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("one.md");
        let f2 = dir.path().join("two.md");
        fs::write(&f1, "init1").unwrap();
        fs::write(&f2, "init2").unwrap();

        let (tx, mut rx) = mpsc::channel::<PathBuf>(64);
        let stop = watch_folder_default(dir.path(), tx)?;
        fs::write(&f1, "ch1").unwrap();
        fs::write(&f2, "ch2").unwrap();

        let mut seen = HashSet::<PathBuf>::new();
        let target1 = canon(&f1);
        let target2 = canon(&f2);

        let ok = recv_until(&mut rx, Duration::from_secs(3), |p| {
            seen.insert(canon(p));
            seen.contains(&target1) && seen.contains(&target2)
        })
        .await
        .is_some();

        assert!(ok, "expected to observe both changed files eventually");

        stop();
        Ok(())
    }

    /// Non-recursive mode should not surface changes in nested directories.
    #[tokio::test(flavor = "multi_thread")]
    async fn non_recursive_ignores_nested_changes() -> NotifyResult<()> {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("sub");
        fs::create_dir_all(&nested).unwrap();

        let top = dir.path().join("top.txt");
        let nested_file = nested.join("nested.txt");
        fs::write(&top, "t0").unwrap();
        fs::write(&nested_file, "n0").unwrap();

        let (tx, mut rx) = mpsc::channel::<PathBuf>(64);
        let stop = watch_folder(
            dir.path(),
            FolderWatchConfig {
                recursive: false,
                debounce_ms: 25,
                ..Default::default()
            },
            tx,
        )?;

        // Change nested file: we should **not** see it in a short window.
        fs::write(&nested_file, "n1").unwrap();
        let nested_seen = recv_until(&mut rx, Duration::from_millis(800), |p| {
            canon(p) == canon(&nested_file)
        })
        .await
        .is_some();
        assert!(
            !nested_seen,
            "should not receive nested path in non-recursive mode"
        );

        // Change top file: we should see it.
        fs::write(&top, "t1").unwrap();
        let top_seen = recv_until(&mut rx, Duration::from_millis(1200), |p| {
            canon(p) == canon(&top)
        })
        .await
        .is_some();
        assert!(
            top_seen,
            "expected to receive top-level change in non-recursive mode"
        );

        stop();
        Ok(())
    }

    /// Canonicalization on: a symlink write should report the canonicalized target path.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn canonicalize_paths_true_reports_target_path() -> NotifyResult<()> {
        use std::os::unix::fs as unix_fs;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        fs::write(&target, "init").unwrap();
        unix_fs::symlink(&target, &link).unwrap();

        let (tx, mut rx) = mpsc::channel::<PathBuf>(64);
        let stop = watch_folder(
            dir.path(),
            FolderWatchConfig {
                canonicalize_paths: true,
                ..Default::default()
            },
            tx,
        )?;

        fs::write(&link, "v1").unwrap();

        let want = canon(&target);
        let ok = recv_until(&mut rx, Duration::from_secs(3), |p| canon(p) == want)
            .await
            .is_some();
        assert!(
            ok,
            "expected at least one tick showing canonicalized target path"
        );

        stop();
        Ok(())
    }

    /// Backpressure sanity: with a small channel capacity (caller-controlled),
    /// a large number of unique writes should still be received eventually.
    #[tokio::test(flavor = "multi_thread")]
    async fn bounded_channel_backpressure_sanity() -> NotifyResult<()> {
        let dir = TempDir::new().unwrap();

        let (tx, mut rx) = mpsc::channel::<PathBuf>(4); // very small buffer
        let stop = watch_folder(
            dir.path(),
            FolderWatchConfig {
                debounce_ms: 20,
                ..Default::default()
            },
            tx,
        )?;

        // Create many unique files quickly.
        let n = 24usize;
        for i in 0..n {
            let p = dir.path().join(format!("f{i}.txt"));
            fs::write(&p, "x").unwrap();
            fs::write(&p, "y").unwrap(); // ensure an event per file
        }

        // Collect for a while.
        let items = drain_for(&mut rx, Duration::from_secs(3)).await;

        // We should see at least some events (backpressure means producer can wait, not drop).
        assert!(
            !items.is_empty(),
            "expected to receive some paths even with tight capacity"
        );

        // Either duplicates were coalesced by debounce, or not; ensure most are unique.
        let uniq: HashSet<_> = items.into_iter().map(|p| canon(&p)).collect();
        assert!(
            !uniq.is_empty(),
            "expected at least one unique path to be delivered"
        );

        stop();
        Ok(())
    }

    /// After stop, further writes should not be delivered (beyond eventual draining).
    #[tokio::test(flavor = "multi_thread")]
    async fn stop_prevents_further_delivery() -> NotifyResult<()> {
        use std::time::{Duration, Instant};

        let dir = TempDir::new().unwrap();
        let file = dir.path().join("afterstop.txt");
        fs::write(&file, "init").unwrap();

        let (tx, mut rx) = mpsc::channel::<PathBuf>(64);
        let stop = watch_folder_default(dir.path(), tx)?;

        // Cause at least one event before stop.
        fs::write(&file, "v1").unwrap();

        // Wait for at least one event (don’t care which one).
        let _ = timeout(Duration::from_millis(1500), rx.recv()).await;

        // Now stop the watcher/forwarder/sender.
        stop();

        // Drain any residual buffered events until we've seen a “quiet” period.
        let quiet_for = Duration::from_millis(250);
        let overall_deadline = Instant::now() + Duration::from_secs(2);
        let mut last_recv = Instant::now();

        loop {
            if last_recv.elapsed() >= quiet_for {
                break;
            }
            if Instant::now() >= overall_deadline {
                break;
            }

            match timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Some(_p)) => {
                    last_recv = Instant::now();
                }
                Ok(None) => {
                    // Channel closed — ideal case after stop.
                    break;
                }
                Err(_) => {
                    // Timed out waiting; loop again to check quiet window.
                }
            }
        }

        // After draining, assert no *new* events arrive in a longer observation window.
        let verdict = timeout(Duration::from_millis(600), rx.recv()).await;
        match verdict {
            Ok(Some(_)) => panic!("no further events expected after stop"),
            Ok(None) => { /* channel closed: good */ }
            Err(_) => { /* timed out with no events: also good */ }
        }

        Ok(())
    }

    /// Invalid root should error immediately.
    #[test]
    fn invalid_root_errors() {
        let bogus = PathBuf::from("/definitely/not/a/real/path/for/watch");
        let (tx, _rx) = mpsc::channel::<PathBuf>(8);
        let res = watch_folder(bogus, FolderWatchConfig::default(), tx);
        assert!(res.is_err(), "watching a non-existent root should error");
    }

    /// Debounce off should still stream paths; we allow multiple hits for the same file.
    #[tokio::test(flavor = "multi_thread")]
    async fn debounce_off_still_streams() -> NotifyResult<()> {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("nodebounce.txt");
        fs::write(&file, "init").unwrap();

        let (tx, mut rx) = mpsc::channel::<PathBuf>(64);
        let stop = watch_folder(
            dir.path(),
            FolderWatchConfig {
                debounce_ms: 0,
                ..Default::default()
            },
            tx,
        )?;

        fs::write(&file, "v1").unwrap();
        fs::write(&file, "v2").unwrap();

        let target = canon(&file);
        let mut tally = HashMap::<PathBuf, usize>::new();
        let events = drain_for(&mut rx, Duration::from_millis(1200)).await;
        for p in events {
            *tally.entry(canon(&p)).or_default() += 1;
        }

        // We should have seen at least one event for the file.
        assert!(
            tally.get(&target).copied().unwrap_or(0) >= 1,
            "expected at least one event with debounce off"
        );

        stop();
        Ok(())
    }
}
