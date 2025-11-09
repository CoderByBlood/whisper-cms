//! folder_watch.rs
//! Functional, module-level entry points that drive a `ReactiveQueue<PathBuf>` from OS filesystem events.
//!
//! Usage:
//!   let (queue, stop) = watch_folder_default("/some/root")?;
//!   queue.effect(|| {
//!       let _ = queue.seq.get();             // one tick per dispatched path
//!       if let Some(p) = queue.last_item.get() {
//!           // react to `p`
//!       }
//!   });
//!   // ... later:
//!   stop(); // synchronous shutdown (stops forwarder task, watcher, and the queue)
//!
//! If you need knobs, use `watch_folder(root, cfg)`.

use domain::react::ReactiveQueue;
use notify::{Event, RecursiveMode, Result as NotifyResult, Watcher};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot}; // adjust this path to where ReactiveQueue<T> lives

/// Configuration for how we forward events into the queue.
#[derive(Debug, Clone)]
pub struct FolderWatchConfig {
    /// Whether to recursively watch the directory tree.
    pub recursive: bool,
    /// Small debounce to smooth editor bursts before we forward to the queue (milliseconds).
    /// The `ReactiveQueue` still coalesces duplicates while queued; this only drains immediate bursts from `notify`.
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
pub fn watch_folder_default(
    root: impl AsRef<Path>,
) -> NotifyResult<(Arc<ReactiveQueue<PathBuf>>, Box<dyn FnOnce() + Send>)> {
    watch_folder(root, FolderWatchConfig::default())
}

/// Functional entry point: start watching `root` and forwarding paths into a **new** ReactiveQueue<PathBuf>.
/// Returns:
/// - `Arc<ReactiveQueue<PathBuf>>` you can use to register effects and read signals
/// - `stop: Box<dyn FnOnce() + Send>` synchronous closure that shuts down everything
pub fn watch_folder(
    root: impl AsRef<Path>,
    cfg: FolderWatchConfig,
) -> NotifyResult<(Arc<ReactiveQueue<PathBuf>>, Box<dyn FnOnce() + Send>)> {
    // Create the queue.
    let queue = Arc::new(ReactiveQueue::<PathBuf>::start());

    // Wire notify → mpsc so we can process on a Tokio task.
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let mut watcher = notify::recommended_watcher(move |res| {
        if let Ok(ev) = res {
            let _ = tx.send(ev);
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

    // Clone what we need into the task.
    let queue_for_task = Arc::clone(&queue);
    let canonicalize = cfg.canonicalize_paths;
    let debounce_ms = cfg.debounce_ms;

    // Spawn forwarder: debounce bursts, enqueue each path (coalescing handled by ReactiveQueue).
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                maybe = rx.recv() => {
                    let Some(ev) = maybe else { break; };

                    if debounce_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(debounce_ms)).await;
                    }

                    // Handle this batch.
                    for p in ev.paths {
                        let path_to_send = if canonicalize {
                            std::fs::canonicalize(&p).unwrap_or(p)
                        } else {
                            p
                        };
                        queue_for_task.enqueue(path_to_send);
                    }

                    // Drain immediate backlog without extra sleeps.
                    while let Ok(ev2) = rx.try_recv() {
                        for p in ev2.paths {
                            let path_to_send = if canonicalize {
                                std::fs::canonicalize(&p).unwrap_or(p)
                            } else {
                                p
                            };
                            queue_for_task.enqueue(path_to_send);
                        }
                    }
                }
            }
        }
    });

    // Build a synchronous stopper closure that:
    // 1) drops the watcher (stop native callbacks)
    // 2) signals the task to exit and aborts it if still running
    // 3) stops the ReactiveQueue (joins its reactive thread)
    let stop = {
        // Move ownership into the closure
        let mut maybe_watcher = Some(watcher);
        let mut maybe_task = Some(task);
        let queue_for_stop = Arc::clone(&queue);
        let mut maybe_shutdown = Some(shutdown_tx);

        // inside watch_folder / stop closure
        Box::new(move || {
            // 1) stop OS callbacks
            maybe_watcher.take();

            // 2) stop forwarder task (signal + abort if still pending)
            if let Some(tx) = maybe_shutdown.take() {
                let _ = tx.send(());
            }
            if let Some(h) = maybe_task.take() {
                h.abort(); // can't .await here; abort is fine
            }

            // 3) stop the queue synchronously (non-consuming API)
            queue_for_stop.stop();
        }) as Box<dyn FnOnce() + Send>
    };

    Ok((queue, stop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use leptos_reactive::SignalGet;
    use std::{
        collections::HashSet,
        fs,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::{Duration, Instant},
    };
    use tempfile::TempDir;

    // ---------- helpers ----------

    fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if pred() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn canon(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    fn register_counting_effect(
        q: &ReactiveQueue<PathBuf>,
        hits: &'static AtomicUsize,
        seen_initial: &'static AtomicBool,
    ) {
        let seq = q.seq;
        let last = q.last_item;

        q.effect(move || {
            let _ = seq.get(); // tracked
            let _ = last.get(); // tracked
            if !seen_initial.swap(true, Ordering::Relaxed) {
                return; // ignore first run
            }
            hits.fetch_add(1, Ordering::Relaxed);
        });
    }

    // ---------- tests ----------

    #[tokio::test(flavor = "multi_thread")]
    async fn starts_and_stops_without_events() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let (q, stop) = watch_folder_default(dir.path())?;

        static HITS: AtomicUsize = AtomicUsize::new(0);
        static INIT: AtomicBool = AtomicBool::new(false);
        register_counting_effect(&q, &HITS, &INIT);

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(HITS.load(Ordering::Relaxed), 0);

        stop();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn single_file_change_emits_one_or_more_ticks_and_last_matches() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, "init").unwrap();

        let (q, stop) = watch_folder_default(dir.path())?;

        static HITS: AtomicUsize = AtomicUsize::new(0);
        static INIT: AtomicBool = AtomicBool::new(false);
        register_counting_effect(&q, &HITS, &INIT);

        fs::write(&file, "v1").unwrap();

        let got_tick = wait_until(Duration::from_millis(1500), || {
            HITS.load(Ordering::Relaxed) >= 1
        });
        assert!(
            got_tick,
            "expected at least one tick after single file write"
        );

        let last = q.read_last_item().expect("last_item should be Some");
        assert_eq!(
            canon(&last),
            canon(&file),
            "last path should match (canonicalized)"
        );

        stop();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn burst_same_file_is_coalesced_non_deterministically() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("dup.txt");
        fs::write(&file, "init").unwrap();

        let (q, stop) = watch_folder(
            dir.path(),
            FolderWatchConfig {
                debounce_ms: 40,
                ..Default::default()
            },
        )?;

        static HITS: AtomicUsize = AtomicUsize::new(0);
        static INIT: AtomicBool = AtomicBool::new(false);
        register_counting_effect(&q, &HITS, &INIT);

        for n in 0..5 {
            fs::write(&file, format!("v{}", n)).unwrap();
        }

        let ok = wait_until(Duration::from_millis(2000), || {
            HITS.load(Ordering::Relaxed) >= 1
        });
        assert!(ok, "expected at least one tick for burst");

        let ticks = HITS.load(Ordering::Relaxed);
        assert!(
            ticks <= 5,
            "expected <= 5 ticks for 5 burst writes (coalescing while queued), got {}",
            ticks
        );

        stop();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_distinct_files_eventually_both_seen() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let f1 = dir.path().join("one.md");
        let f2 = dir.path().join("two.md");
        fs::write(&f1, "init1").unwrap();
        fs::write(&f2, "init2").unwrap();

        let (q, stop) = watch_folder_default(dir.path())?;

        let seen: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let seen_cl = Arc::clone(&seen);

        let seq = q.seq;
        let last = q.last_item;
        static INIT: AtomicBool = AtomicBool::new(false);
        q.effect(move || {
            let _ = seq.get();
            if let Some(p) = last.get() {
                if !INIT.swap(true, Ordering::Relaxed) {
                    return;
                }
                seen_cl.lock().unwrap().insert(canon(&p));
            }
        });

        fs::write(&f1, "ch1").unwrap();
        fs::write(&f2, "ch2").unwrap();

        let f1c = canon(&f1);
        let f2c = canon(&f2);

        let ok = wait_until(Duration::from_secs(3), || {
            let s = seen.lock().unwrap();
            s.contains(&f1c) && s.contains(&f2c)
        });
        assert!(ok, "expected to see both changed files (canonicalized)");

        stop();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn non_recursive_ignores_nested_changes() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let nested_dir = dir.path().join("sub");
        fs::create_dir_all(&nested_dir).unwrap();

        let top_file = dir.path().join("top.txt");
        let nested_file = nested_dir.join("nested.txt");
        fs::write(&top_file, "t0").unwrap();
        fs::write(&nested_file, "n0").unwrap();

        let (q, stop) = watch_folder(
            dir.path(),
            FolderWatchConfig {
                recursive: false,
                debounce_ms: 20,
                ..Default::default()
            },
        )?;

        static HITS: AtomicUsize = AtomicUsize::new(0);
        static INIT: AtomicBool = AtomicBool::new(false);

        let seq = q.seq;
        let last = q.last_item;
        q.effect(move || {
            let _ = seq.get();
            let _ = last.get();
            if !INIT.swap(true, Ordering::Relaxed) {
                return;
            }
            HITS.fetch_add(1, Ordering::Relaxed);
        });

        fs::write(&nested_file, "n1").unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        let hits_after_nested = HITS.load(Ordering::Relaxed);

        fs::write(&top_file, "t1").unwrap();
        let ok = wait_until(Duration::from_millis(1200), || {
            HITS.load(Ordering::Relaxed) > hits_after_nested
        });
        assert!(
            ok,
            "expected a tick for top-level change in non-recursive mode"
        );

        stop();
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn canonicalize_paths_true_reports_target_path() -> notify::Result<()> {
        use std::os::unix::fs as unix_fs;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        fs::write(&target, "init").unwrap();
        unix_fs::symlink(&target, &link).unwrap();

        let (q, stop) = watch_folder(
            dir.path(),
            FolderWatchConfig {
                canonicalize_paths: true,
                ..Default::default()
            },
        )?;

        static INIT: AtomicBool = AtomicBool::new(false);
        static HITS: AtomicUsize = AtomicUsize::new(0);

        let seq = q.seq;
        let last = q.last_item;
        let target_c = canon(&target);
        q.effect(move || {
            let _ = seq.get();
            if let Some(p) = last.get() {
                if !INIT.swap(true, Ordering::Relaxed) {
                    return;
                }
                if canon(&p) == target_c {
                    HITS.fetch_add(1, Ordering::Relaxed);
                }
            }
        });

        fs::write(&link, "v1").unwrap();

        let ok = wait_until(Duration::from_millis(3000), || {
            HITS.load(Ordering::Relaxed) >= 1
        });
        assert!(
            ok,
            "expected at least one tick showing canonicalized target path"
        );

        stop();
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_prevents_further_delivery() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("afterstop.txt");
        fs::write(&file, "init").unwrap();

        let (q, stop) = watch_folder_default(dir.path())?;

        static HITS: AtomicUsize = AtomicUsize::new(0);
        static INIT: AtomicBool = AtomicBool::new(false);
        register_counting_effect(&q, &HITS, &INIT);

        fs::write(&file, "v1").unwrap();
        let _ = wait_until(Duration::from_millis(1200), || {
            HITS.load(Ordering::Relaxed) >= 1
        });
        let hits_before = HITS.load(Ordering::Relaxed);

        stop();

        fs::write(&file, "v2").unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            HITS.load(Ordering::Relaxed),
            hits_before,
            "no ticks expected after stop"
        );

        Ok(())
    }
}
