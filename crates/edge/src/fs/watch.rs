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
