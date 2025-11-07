//! Headless Leptos (leptos_reactive 0.6.15) + notify helper.
//
//! ✦ One dedicated “reactive thread” owns the Leptos runtime + owner.
//! ✦ All signal writes AND reads go through that thread (no TLS/runtime panics).
//! ✦ Watches a DIRECTORY TREE (Recursive).
//! ✦ Emits **one change at a time**; uses a FIFO **set** to coalesce duplicates while queued.
//!
//! Developer DX:
//!   let fw = watch_folder(dir)?;
//!   fw.effect(|| {
//!       let _ = fw.seq.get();                 // tick per dispatched path
//!       if let Some(p) = fw.last_path.get() { /* react to p */ }
//!   });
//!
//!   // reads that run safely (inside the reactive thread):
//!   fw.read_seq(); fw.read_last_path(); fw.read_dirty();
//!   fw.stop().await;

use leptos_reactive::*;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

/// Messages for the reactive thread.
enum Apply {
    // --- Mutations (queue / dispatch) ---
    /// Try to enqueue a path; if it's not already queued, push it and (if it was empty) kick off dispatch.
    Enqueue(PathBuf),
    /// Pop one path and publish it to signals; if more remain, schedule another `DispatchNext`.
    DispatchNext,

    // --- Sync Reads (reply via std::sync::mpsc) ---
    ReadSeq {
        resp: std::sync::mpsc::Sender<u64>,
    },
    ReadLastPath {
        resp: std::sync::mpsc::Sender<Option<PathBuf>>,
    },
    ReadDirty {
        resp: std::sync::mpsc::Sender<bool>,
    },

    // --- Run a closure inside Owner (e.g., to create effects) and ack when registered ---
    RunSync {
        f: Box<dyn FnOnce(Owner) + Send + 'static>,
        ack: std::sync::mpsc::Sender<()>,
    },

    // --- Shutdown (dispose runtime and exit thread) ---
    Stop,
}

/// Public handle with signals and lifecycle.
pub struct WatchedFolder {
    pub root: PathBuf,

    /// NOTE: These signal handles belong to the reactive thread’s runtime.
    /// Do not call get()/set() from other threads; use `effect()` or the read_* helpers.
    pub seq: RwSignal<u64>, // increments once per dispatched path
    pub last_path: RwSignal<Option<PathBuf>>, // last dispatched path
    pub dirty: RwSignal<bool>,                // true while queue is non-empty

    inner: Inner,
}

/// Private inner state owned by the caller thread (for control/IO).
struct Inner {
    apply_tx: std::sync::mpsc::Sender<Apply>,
    reactive_thread: Option<thread::JoinHandle<()>>,
    watcher: Option<RecommendedWatcher>,
    task: Option<tokio::task::JoinHandle<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Best-effort shutdown if stop() wasn’t called
        self.watcher.take();
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.task.take() {
            h.abort();
        }
        let _ = self.apply_tx.send(Apply::Stop);
        if let Some(j) = self.reactive_thread.take() {
            let _ = j.join();
        }
    }
}

impl WatchedFolder {
    /// Create a Leptos `Effect` inside the owning runtime/owner and wait until it’s registered.
    /// Inside `f`, read from the signals using `.get()` (don’t capture &self).
    pub fn effect<F>(&self, f: F)
    where
        F: Fn() + Send + 'static,
    {
        let (ack_tx, ack_rx) = std::sync::mpsc::channel::<()>();
        let seq = self.seq;
        let last = self.last_path;
        let dirty = self.dirty;

        let _ = self.inner.apply_tx.send(Apply::RunSync {
            f: Box::new(move |owner: Owner| {
                with_owner(owner, || {
                    create_effect(move |_| {
                        // call the user body safely
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            // TRACKED reads so the effect re-runs
                            let _ = seq.get();
                            let _ = dirty.get();
                            let _ = last.get();
                            f();
                        }));
                        if let Err(_payload) = result {
                            // 1) log it
                            println!("effect panicked; add logging");
                            // 2) optional: flip an error signal, metrics, etc.
                            // 3) optional: guard future runs (e.g., with a static AtomicBool) to no-op
                        }
                    });
                });
            }),
            ack: ack_tx,
        });
        let _ = ack_rx.recv();
    }

    /// Safe read of `seq` from outside the reactive thread.
    pub fn read_seq(&self) -> u64 {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.inner.apply_tx.send(Apply::ReadSeq { resp: tx });
        rx.recv().unwrap_or_default()
    }

    /// Safe read of `last_path` from outside the reactive thread.
    pub fn read_last_path(&self) -> Option<PathBuf> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.inner.apply_tx.send(Apply::ReadLastPath { resp: tx });
        rx.recv().unwrap_or(None)
    }

    /// Safe read of `dirty` from outside the reactive thread.
    pub fn read_dirty(&self) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.inner.apply_tx.send(Apply::ReadDirty { resp: tx });
        rx.recv().unwrap_or(false)
    }

    /// Graceful shutdown: stop watcher → stop async task → stop reactive thread (disposes runtime).
    pub async fn stop(mut self) {
        self.inner.watcher.take();
        if let Some(tx) = self.inner.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.inner.task.take() {
            let _ = handle.await;
        }
        let _ = self.inner.apply_tx.send(Apply::Stop);
        if let Some(j) = self.inner.reactive_thread.take() {
            let _ = j.join();
        }
    }

    /// Bypass the OS and inject a synthetic FS path into the queue.
    /// Lets us test FIFO/coalescing deterministically without touching the filesystem.
    #[cfg(test)]
    fn inject_path_for_test(&self, p: PathBuf) {
        let _ = self.inner.apply_tx.send(Apply::Enqueue(p));
    }
}

/// Watch a DIRECTORY TREE; expose signals: `seq`, `last_path`, `dirty`.
/// Internals:
/// • Reactive thread owns the runtime/owner/signals + queue (VecDeque) + in_set (HashSet).
/// • FS watcher + async task send messages with changes; only the reactive thread touches signals.
pub fn watch_folder<P: AsRef<Path>>(root: P) -> notify::Result<WatchedFolder> {
    let root: PathBuf = root.as_ref().to_path_buf();

    // Channel to the reactive thread.
    let (apply_tx, apply_rx) = std::sync::mpsc::channel::<Apply>();
    let apply_tx_for_thread = apply_tx.clone(); // for self-scheduling DispatchNext

    // Start reactive thread and create runtime/owner/signals there.
    let (sig_tx, sig_rx) =
        std::sync::mpsc::channel::<(RwSignal<u64>, RwSignal<Option<PathBuf>>, RwSignal<bool>)>();
    let reactive_thread = thread::spawn(move || {
        let rt: RuntimeId = create_runtime();
        let owner: Owner = Owner::current().expect("root owner (reactive thread)");

        // Signals
        let (seq, last_path, dirty) = with_owner(owner, || {
            (
                create_rw_signal(0_u64),
                create_rw_signal(None::<PathBuf>),
                create_rw_signal(false),
            )
        });

        // Queue + set for coalescing (deliver one-at-a-time)
        let mut q: VecDeque<PathBuf> = VecDeque::new();
        let mut in_set: HashSet<PathBuf> = HashSet::new();

        sig_tx.send((seq, last_path, dirty)).expect("send signals");

        for msg in apply_rx {
            match msg {
                // --- queue management ---
                Apply::Enqueue(path) => {
                    // Only enqueue if not already queued (coalescing)
                    if in_set.insert(path.clone()) {
                        q.push_back(path);
                        with_owner(owner, || dirty.set(true));

                        // If we just transitioned from empty → one element, kick off dispatch
                        if q.len() == 1 {
                            let _ = apply_tx_for_thread.send(Apply::DispatchNext);
                        }
                    }
                }
                Apply::DispatchNext => {
                    if let Some(p) = q.pop_front() {
                        in_set.remove(&p);
                        // Publish this single path to signals
                        with_owner(owner, || {
                            last_path.set(Some(p));
                            seq.set(seq.get_untracked().wrapping_add(1));
                            dirty.set(!q.is_empty());
                        });

                        // If more remain, schedule another dispatch (one-at-a-time)
                        if !q.is_empty() {
                            let _ = apply_tx_for_thread.send(Apply::DispatchNext);
                        }
                    } else {
                        with_owner(owner, || dirty.set(false));
                    }
                }

                // --- reads ---
                Apply::ReadSeq { resp } => {
                    let value = with_owner(owner, || seq.get_untracked());
                    let _ = resp.send(value);
                }
                Apply::ReadLastPath { resp } => {
                    let value = with_owner(owner, || last_path.get_untracked().clone());
                    let _ = resp.send(value);
                }
                Apply::ReadDirty { resp } => {
                    let value = with_owner(owner, || dirty.get_untracked());
                    let _ = resp.send(value);
                }

                // --- in-scope work (effects, memos, additional signals, etc.) ---
                Apply::RunSync { f, ack } => {
                    f(owner);
                    let _ = ack.send(());
                }

                // --- shutdown ---
                Apply::Stop => {
                    rt.dispose();
                    break;
                }
            }
        }
    });

    // Receive signal handles from the reactive thread.
    let (seq, last_path, dirty) = sig_rx.recv().expect("receive signals from reactive thread");

    // Tokio channel for FS events + a stop signal for the task.
    let (fs_tx, mut fs_rx) = mpsc::unbounded_channel::<Event>();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    // Start watcher on the directory TREE (recursive).
    let root_for_watch = root.clone();
    let mut watcher = notify::recommended_watcher(move |res| {
        if let Ok(ev) = res {
            let _ = fs_tx.send(ev);
        }
    })?;
    watcher.watch(&root_for_watch, RecursiveMode::Recursive)?;

    // Async processor: forward each discovered path to the reactive thread,
    // applying de-bounce-lite by draining immediate bursts from notify.
    let apply_for_task = apply_tx.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                maybe = fs_rx.recv() => {
                    if let Some(ev) = maybe {
                        // Debounce: short sleep, then drain any accumulated events,
                        // but we still enqueue paths individually (coalesced by set).
                        tokio::time::sleep(Duration::from_millis(40)).await;
                        // Handle this event
                        for p in ev.paths {
                            let _ = apply_for_task.send(Apply::Enqueue(p));
                        }
                        // Drain bursts without sleeping again
                        while let Ok(ev2) = fs_rx.try_recv() {
                            for p in ev2.paths {
                                let _ = apply_for_task.send(Apply::Enqueue(p));
                            }
                        }
                    } else {
                        break; // channel closed
                    }
                }
            }
        }
    });

    Ok(WatchedFolder {
        root,
        seq,
        last_path,
        dirty,
        inner: Inner {
            apply_tx,
            reactive_thread: Some(reactive_thread),
            watcher: Some(watcher),
            task: Some(task),
            shutdown_tx: Some(shutdown_tx),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{HashSet, VecDeque},
        fs,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };
    use tempfile::TempDir;

    // -------- helpers (non-deterministic friendly) --------

    async fn eventually(mut cond: impl FnMut() -> bool, timeout_ms: u64) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        while tokio::time::Instant::now() < deadline {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        cond()
    }

    fn start_observer(fw: &WatchedFolder) -> Arc<Mutex<VecDeque<PathBuf>>> {
        static SEEN_INITIAL: AtomicBool = AtomicBool::new(false);
        let bag: Arc<Mutex<VecDeque<PathBuf>>> = Arc::new(Mutex::new(VecDeque::new()));
        let bag_cl = bag.clone();
        let seq = fw.seq;
        let last = fw.last_path;

        fw.effect(move || {
            // track reactive dependencies; ignore first run
            let _ = seq.get();
            if let Some(p) = last.get() {
                if !SEEN_INITIAL.swap(true, Ordering::Relaxed) {
                    return;
                }
                bag_cl.lock().unwrap().push_back(p);
            }
        });
        bag
    }

    // -------- tests (no exact counts/order) --------

    /// Initial state: allow minimal assumptions (seq >= 0; last_path may be None; dirty is false).
    #[tokio::test(flavor = "multi_thread")]
    async fn initial_state_is_reasonable() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let fw = watch_folder(dir.path())?;
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert_eq!(fw.read_last_path(), None, "last_path should start None");
        assert!(!fw.read_dirty(), "dirty should start false");

        fw.stop().await;
        Ok(())
    }

    /// No external changes → effect should not accumulate ticks beyond the initial run.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_changes_no_extra_ticks() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let fw = watch_folder(dir.path())?;

        static HITS: AtomicUsize = AtomicUsize::new(0);
        static SEEN_INITIAL: AtomicBool = AtomicBool::new(false);
        let seq = fw.seq;
        let last = fw.last_path;

        fw.effect(move || {
            // track deps
            let _ = seq.get();
            let _ = last.get();
            if !SEEN_INITIAL.swap(true, Ordering::Relaxed) {
                return;
            }
            HITS.fetch_add(1, Ordering::Relaxed);
        });

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(HITS.load(Ordering::Relaxed), 0);

        fw.stop().await;
        Ok(())
    }

    /// Injecting the SAME path several times should produce >=1 dispatches and < injections.
    /// (Coalescing is best-effort; we only assert a strict reduction.)
    #[tokio::test(flavor = "multi_thread")]
    async fn inject_same_path_produces_fewer_than_injections() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let fw = watch_folder(dir.path())?;
        let bag = start_observer(&fw);

        let p = dir.path().join("a.txt");
        let injections = 5usize;
        for _ in 0..injections {
            fw.inject_path_for_test(p.clone());
        }

        let saw_at_least_one = eventually(|| bag.lock().unwrap().len() >= 1, 800).await;
        assert!(saw_at_least_one, "expected some dispatches");

        // after a grace period, ensure we got fewer than injections
        tokio::time::sleep(Duration::from_millis(250)).await;
        let n = bag.lock().unwrap().len();
        assert!(
            n >= 1 && n < injections,
            "expected 1..{} dispatches, got {}",
            injections,
            n
        );

        // dirty eventually false post-drain
        let drained = eventually(|| !fw.read_dirty(), 800).await;
        assert!(drained, "dirty should end false");

        fw.stop().await;
        Ok(())
    }

    /// Injecting distinct paths: we only assert that all distinct paths appear eventually (order-free).
    #[tokio::test(flavor = "multi_thread")]
    async fn inject_distinct_paths_all_seen_order_free() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let fw = watch_folder(dir.path())?;
        let bag = start_observer(&fw);

        let a = dir.path().join("x/one.md");
        let b = dir.path().join("x/two.md");
        fw.inject_path_for_test(a.clone());
        fw.inject_path_for_test(b.clone());

        let ok = eventually(
            || {
                let seen = bag.lock().unwrap();
                let set: HashSet<_> = seen.iter().cloned().collect();
                set.contains(&a) && set.contains(&b)
            },
            1200,
        )
        .await;
        assert!(ok, "expected both paths to be seen eventually");

        // seq advanced by at least the number of distinct paths
        assert!(fw.read_seq() >= 2);

        fw.stop().await;
        Ok(())
    }

    /// Dirty flag only needs to be true at some point during processing, then false when drained.
    #[tokio::test(flavor = "multi_thread")]
    async fn dirty_true_sometime_then_false_at_end() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let fw = watch_folder(dir.path())?;

        // enqueue a few items
        for i in 0..3 {
            fw.inject_path_for_test(dir.path().join(format!("p{i}.txt")));
        }

        let went_true = eventually(|| fw.read_dirty(), 800).await;
        assert!(went_true, "dirty should become true while work exists");

        let went_false = eventually(|| !fw.read_dirty(), 1200).await;
        assert!(went_false, "dirty should become false after drain");

        fw.stop().await;
        Ok(())
    }

    /// Filesystem smoke test: two different files on disk → eventually both names observed (order-free).
    #[tokio::test(flavor = "multi_thread")]
    async fn fs_smoke_two_files_observed_order_free() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let fw = watch_folder(dir.path())?;
        let bag = start_observer(&fw);

        let f1 = dir.path().join("nested/one.md");
        let f2 = dir.path().join("nested/two.md");
        fs::create_dir_all(f1.parent().unwrap()).unwrap();
        fs::write(&f1, "1").unwrap();
        fs::write(&f2, "2").unwrap();

        // mutate both; different backends fire differently — we only require eventual observation
        fs::write(&f1, "1*").unwrap();
        fs::write(&f2, "2*").unwrap();

        let ok = eventually(
            || {
                let names: HashSet<String> = bag
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                    .collect();
                names.contains("one.md") && names.contains("two.md")
            },
            2000,
        )
        .await;
        assert!(ok, "both filenames should appear at least once");

        assert!(fw.read_seq() >= 2, "seq should have advanced");
        let drained = eventually(|| !fw.read_dirty(), 800).await;
        assert!(drained, "dirty should be false at rest");

        fw.stop().await;
        Ok(())
    }

    /// Panicking effect: we only assert that the system stays alive and processes *future* ticks.
    #[tokio::test(flavor = "multi_thread")]
    async fn panicking_effect_does_not_kill_runtime() -> notify::Result<()> {
        let dir = TempDir::new().unwrap();
        let fw = watch_folder(dir.path())?;

        static DID_PANIC: AtomicBool = AtomicBool::new(false);
        let seq = fw.seq;

        fw.effect(move || {
            let _ = seq.get();
            if !DID_PANIC.swap(true, Ordering::Relaxed) {
                panic!("intentional one-time panic inside effect");
            }
        });

        // first tick (may trigger the panic)
        fw.inject_path_for_test(dir.path().join("panic-once"));
        let ok1 = eventually(|| fw.read_seq() >= 1, 1200).await;
        assert!(ok1, "seq should advance at least once");

        // second tick proves the runtime is still alive
        fw.inject_path_for_test(dir.path().join("still-alive"));
        let ok2 = eventually(|| fw.read_seq() >= 2, 1200).await;
        assert!(ok2, "runtime should continue after a panicking effect");

        fw.stop().await;
        Ok(())
    }
}
