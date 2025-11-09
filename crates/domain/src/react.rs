//! ReactiveQueue: a generic, notify-agnostic reactive queue built on Leptos signals.
//!
//! ✦ A dedicated “reactive thread” owns the Leptos runtime + owner.
//! ✦ Drive it by calling `enqueue(...)` from any source (notify, network, manual, etc.).
//! ✦ Emits **one change at a time**; FIFO **set** coalesces duplicates while queued.
//! ✦ `stop(&self)` is synchronous and non-consuming (safe to call from multiple places; joins once).
//!
//! Example:
//!   let q = ReactiveQueue::<PathBuf>::start();
//!   q.effect(|| {
//!       let _ = q.seq.get();                 // tick per dispatched item
//!       if let Some(x) = q.last_item.get() { /* react to x */ }
//!   });
//!   q.enqueue("/some/changed/file".into());
//!   q.stop(); // synchronous shutdown

use leptos_reactive::*;
use std::{
    collections::{HashSet, VecDeque},
    hash::Hash,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Mutex,
    thread,
};

/// Messages handled on the reactive thread.
enum Apply<T> {
    // --- Mutations (queue / dispatch) ---
    /// Try to enqueue an item; if it's not already queued, push it and (if it was empty) kick off dispatch.
    Enqueue(T),
    /// Pop one item and publish it to signals; if more remain, schedule another `DispatchNext`.
    DispatchNext,

    // --- Sync Reads (reply via std::sync::mpsc) ---
    ReadSeq {
        resp: std::sync::mpsc::Sender<u64>,
    },
    ReadLastItem {
        resp: std::sync::mpsc::Sender<Option<T>>,
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

/// Generic, notify-agnostic queue that drives Leptos signals.
pub struct ReactiveQueue<T>
where
    T: Clone + Eq + Hash + Send + 'static,
{
    /// Increments once per dispatched item.
    pub seq: RwSignal<u64>,
    /// Last dispatched item (Some) or None if nothing dispatched yet.
    pub last_item: RwSignal<Option<T>>,
    /// True while queue is non-empty.
    pub dirty: RwSignal<bool>,

    inner: Inner<T>,
}

struct Inner<T>
where
    T: Clone + Eq + Hash + Send + 'static,
{
    apply_tx: std::sync::mpsc::Sender<Apply<T>>,
    /// Join handle protected by a mutex so `stop(&self)` can join exactly once.
    reactive_thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl<T> Drop for Inner<T>
where
    T: Clone + Eq + Hash + Send + 'static,
{
    fn drop(&mut self) {
        // Best-effort shutdown if `stop()` wasn’t called.
        let _ = self.apply_tx.send(Apply::Stop);
        if let Some(j) = self.reactive_thread.lock().unwrap().take() {
            let _ = j.join();
        }
    }
}

impl<T> ReactiveQueue<T>
where
    T: Clone + Eq + Hash + Send + 'static,
{
    /// Start the reactive thread and return a handle.
    pub fn start() -> Self {
        // Channel to the reactive thread.
        let (apply_tx, apply_rx) = std::sync::mpsc::channel::<Apply<T>>();
        let apply_tx_for_thread = apply_tx.clone(); // for self-scheduling DispatchNext

        // Send signal handles back to the caller.
        let (sig_tx, sig_rx) =
            std::sync::mpsc::channel::<(RwSignal<u64>, RwSignal<Option<T>>, RwSignal<bool>)>();

        let reactive_thread = thread::spawn(move || {
            let rt: RuntimeId = create_runtime();
            let owner: Owner = Owner::current().expect("root owner (reactive thread)");

            // Signals
            let (seq, last_item, dirty) = with_owner(owner, || {
                (
                    create_rw_signal(0_u64),
                    create_rw_signal(None::<T>),
                    create_rw_signal(false),
                )
            });

            // Queue + set (deliver one-at-a-time; coalesce duplicates)
            let mut q: VecDeque<T> = VecDeque::new();
            let mut in_set: HashSet<T> = HashSet::new();

            sig_tx.send((seq, last_item, dirty)).expect("send signals");

            for msg in apply_rx {
                match msg {
                    // --- queue management ---
                    Apply::Enqueue(item) => {
                        // Only enqueue if not already queued (coalescing)
                        if in_set.insert(item.clone()) {
                            q.push_back(item);
                            with_owner(owner, || dirty.set(true));

                            // If we just transitioned from empty → one element, kick off dispatch
                            if q.len() == 1 {
                                let _ = apply_tx_for_thread.send(Apply::DispatchNext);
                            }
                        }
                    }
                    Apply::DispatchNext => {
                        if let Some(x) = q.pop_front() {
                            in_set.remove(&x);
                            // Publish this single item to signals
                            with_owner(owner, || {
                                last_item.set(Some(x));
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
                    Apply::ReadLastItem { resp } => {
                        let value = with_owner(owner, || last_item.get_untracked().clone());
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
        let (seq, last_item, dirty) = sig_rx.recv().expect("receive signals from reactive thread");

        ReactiveQueue {
            seq,
            last_item,
            dirty,
            inner: Inner {
                apply_tx,
                reactive_thread: Mutex::new(Some(reactive_thread)),
            },
        }
    }

    /// Enqueue a single item (coalesced if already queued).
    pub fn enqueue(&self, item: T) {
        let _ = self.inner.apply_tx.send(Apply::Enqueue(item));
    }

    /// Enqueue multiple items (each coalesced individually).
    pub fn enqueue_many<I>(&self, iter: I)
    where
        I: IntoIterator<Item = T>,
    {
        for x in iter {
            let _ = self.inner.apply_tx.send(Apply::Enqueue(x));
        }
    }

    /// Create a Leptos `Effect` inside the owning runtime/owner and wait until it’s registered.
    /// Inside `f`, read from the signals using `.get()` (don’t capture &self).
    pub fn effect<F>(&self, f: F)
    where
        F: Fn() + Send + 'static,
    {
        use std::sync::mpsc::channel;

        let (ack_tx, ack_rx) = channel::<()>();
        let seq = self.seq;
        let last = self.last_item;
        let dirty = self.dirty;

        let _ = self.inner.apply_tx.send(Apply::RunSync {
            f: Box::new(move |owner: Owner| {
                with_owner(owner, || {
                    create_effect(move |_| {
                        // Panic containment so the reactive thread survives user mistakes.
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            // TRACKED reads so the effect re-runs
                            let _ = seq.get();
                            let _ = dirty.get();
                            let _ = last.get();
                            f();
                        }));
                        if let Err(_payload) = result {
                            // TODO: plug your logger/metrics here; we intentionally swallow to keep the thread alive.
                            // e.g., log::error!("ReactiveQueue effect panicked");
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

    /// Safe read of `last_item` from outside the reactive thread.
    pub fn read_last_item(&self) -> Option<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.inner.apply_tx.send(Apply::ReadLastItem { resp: tx });
        rx.recv().unwrap_or(None)
    }

    /// Safe read of `dirty` from outside the reactive thread.
    pub fn read_dirty(&self) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.inner.apply_tx.send(Apply::ReadDirty { resp: tx });
        rx.recv().unwrap_or(false)
    }

    /// Graceful shutdown: stop the reactive thread (disposes runtime). **Synchronous & non-consuming.**
    pub fn stop(&self) {
        let _ = self.inner.apply_tx.send(Apply::Stop);
        if let Some(j) = self.inner.reactive_thread.lock().unwrap().take() {
            let _ = j.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    // -------- test helpers --------

    /// Spin until `pred()` returns true or `timeout` elapses.
    fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if pred() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Returns (initial_seq, initial_last, initial_dirty) snapshot.
    fn snapshot<T: Clone + Eq + Hash + Send + 'static>(
        q: &ReactiveQueue<T>,
    ) -> (u64, Option<T>, bool) {
        (q.read_seq(), q.read_last_item(), q.read_dirty())
    }

    // -------- tests --------

    /// Effects register and run (initial run occurs), but we only count *post-initial* ticks.
    #[test]
    fn effect_registers_and_fires_on_single_enqueue() {
        let q = ReactiveQueue::<String>::start();

        static HITS: AtomicUsize = AtomicUsize::new(0);
        static SEEN_INITIAL: AtomicBool = AtomicBool::new(false);

        // Observe seq/last; count only post-initial runs.
        q.effect(move || {
            let _s = q.seq.get();
            let _l = q.last_item.get();
            if !SEEN_INITIAL.swap(true, Ordering::Relaxed) {
                return; // ignore initial effect run
            }
            HITS.fetch_add(1, Ordering::Relaxed);
        });

        // Enqueue a single item.
        q.enqueue("a".to_string());

        // We should see at least one post-initial run within a reasonable window.
        let ok = wait_until(Duration::from_millis(400), || {
            HITS.load(Ordering::Relaxed) >= 1
        });
        assert!(ok, "effect should have fired after a single enqueue");

        // Clean shutdown is synchronous.
        q.stop();
    }

    /// Bursty duplicates get coalesced while queued; do not assert exact counts.
    /// We only assert the seq advanced, and *likely* fewer than total duplicate sends.
    #[test]
    fn bursty_duplicates_are_coalesced_non_deterministically() {
        let q = ReactiveQueue::<String>::start();
        let (s0, _, _) = snapshot(&q);

        // Send the same item several times back-to-back.
        for _ in 0..5 {
            q.enqueue("dup".to_string());
        }

        // Eventually, seq should advance by at least 1.
        let ok = wait_until(Duration::from_millis(500), || q.read_seq() > s0);
        assert!(ok, "sequence should advance after burst");

        // Non-deterministic upper bound: it should not exceed the number of sends,
        // but we don't assert exact coalescing factor, just that it's <= 5.
        let advanced_by = q.read_seq().saturating_sub(s0);
        assert!(
            advanced_by <= 5,
            "non-deterministic coalescing: seq advanced by {} (expected <= 5)",
            advanced_by
        );

        q.stop();
    }

    /// Multiple distinct items should all eventually appear as last_item,
    /// and seq should advance by at least the number of distinct items (within time).
    #[test]
    fn multiple_distinct_items_eventually_dispatch() {
        let q = ReactiveQueue::<String>::start();
        let (s0, _, _) = snapshot(&q);

        let items = vec![
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string(),
            "delta".to_string(),
        ];
        for it in &items {
            q.enqueue(it.clone());
        }

        // Wait until seq has advanced by at least the number of distinct items.
        let want = items.len() as u64;
        let ok = wait_until(Duration::from_secs(2), || {
            q.read_seq().saturating_sub(s0) >= want
        });
        assert!(
            ok,
            "expected seq to advance by >= {} after enqueuing {} distinct items (start seq: {})",
            want,
            items.len(),
            s0
        );

        // last_item should be one of the enqueued items at the end.
        if let Some(li) = q.read_last_item() {
            assert!(
                items.contains(&li),
                "last_item should be one of the sent items; got: {:?}",
                li
            );
        } else {
            panic!("last_item should be Some after dispatch");
        }

        q.stop();
    }

    /// Dirty flag invariants: becomes true when queue receives items, eventually false when drained.
    #[test]
    fn dirty_flag_behaves_reasonably() {
        let q = ReactiveQueue::<String>::start();

        // Initially, likely false (empty).
        let (_, _, d0) = snapshot(&q);
        assert!(!d0, "dirty should start false");

        // Enqueue several items.
        for i in 0..3 {
            q.enqueue(format!("x{}", i));
        }

        // Dirty should eventually be true.
        let went_dirty = wait_until(Duration::from_millis(200), || q.read_dirty());
        assert!(went_dirty, "dirty should become true after enqueues");

        // Then eventually false again after draining.
        let went_clean = wait_until(Duration::from_secs(2), || !q.read_dirty());
        assert!(
            went_clean,
            "dirty should eventually return to false after draining"
        );

        q.stop();
    }

    /// Panic inside the user effect must not crash the reactive thread; subsequent enqueues still dispatch.
    #[test]
    fn effect_panic_is_contained_and_processing_continues() {
        let q = ReactiveQueue::<String>::start();

        static PANIC_ONCE: AtomicBool = AtomicBool::new(false);
        static PROGRESS: AtomicUsize = AtomicUsize::new(0);

        // Register an effect that will panic exactly once (post-initial),
        // then continue to count subsequent ticks.
        q.effect(move || {
            let _ = q.seq.get();
            if !PANIC_ONCE.swap(true, Ordering::Relaxed) {
                // First post-initial run: induce a panic the effect code catches internally.
                panic!("intentional effect panic");
            } else {
                PROGRESS.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Kick the system to run several ticks.
        for i in 0..3 {
            q.enqueue(format!("p{}", i));
        }

        // We should see progress after the panic (i.e., ticks recorded).
        let ok = wait_until(Duration::from_secs(1), || {
            PROGRESS.load(Ordering::Relaxed) >= 1
        });
        assert!(
            ok,
            "effect panic should be contained; expected subsequent progress but saw none"
        );

        q.stop();
    }

    /// `stop(&self)` is idempotent and safe to call multiple times.
    #[test]
    fn stop_is_idempotent() {
        let q = ReactiveQueue::<String>::start();

        // Drive some activity
        q.enqueue("a".into());
        wait_until(Duration::from_millis(300), || q.read_seq() >= 1);

        // Multiple stops shouldn't panic.
        q.stop();
        q.stop();
        q.stop();
    }

    /// Enqueue from many threads concurrently; we assert lower bounds (>= unique items),
    /// but do not assume any exact ordering or perfect coalescing.
    #[test]
    fn concurrent_enqueues_are_processed() {
        let q = Arc::new(ReactiveQueue::<String>::start());
        let (s0, _, _) = (q.read_seq(), q.read_last_item(), q.read_dirty());

        let threads = 8usize;
        let per_thread = 10usize;

        let mut handles = Vec::new();
        for tid in 0..threads {
            let qc = q.clone();
            handles.push(thread::spawn(move || {
                for i in 0..per_thread {
                    qc.enqueue(format!("t{}_i{}", tid, i));
                }
                // Also enqueue a few duplicates per thread.
                for _ in 0..5 {
                    qc.enqueue(format!("dup_t{}", tid));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // We enqueued `threads * per_thread` distinct items plus duplicates.
        // We assert we get at least the distinct count within a reasonable time.
        let distinct = (threads * per_thread) as u64;
        let ok = wait_until(Duration::from_secs(3), || {
            q.read_seq().saturating_sub(s0) >= distinct
        });
        assert!(
            ok,
            "expected at least {} dispatches; got {} (start seq: {})",
            distinct,
            q.read_seq(),
            s0
        );

        q.stop();
    }

    /// Reading APIs should be safe at any time, including immediately after start,
    /// between enqueues, and after stop. This test samples around the lifecycle.
    #[test]
    fn reads_are_safe_across_lifecycle() {
        let q = ReactiveQueue::<String>::start();

        // Immediately readable
        let _ = q.read_seq();
        let _ = q.read_last_item();
        let _ = q.read_dirty();

        // During activity
        q.enqueue("z1".into());
        wait_until(Duration::from_millis(400), || q.read_seq() >= 1);
        let _ = q.read_seq();
        let _ = q.read_last_item();
        let _ = q.read_dirty();

        // After stop, reads still return safely (Drop already joins too).
        q.stop();
        let _ = q.read_seq();
        let _ = q.read_last_item();
        let _ = q.read_dirty();
    }

    /// Effects created via `effect(...)` are intentionally tracked on (seq, dirty, last),
    /// so they re-run on dispatch. We assert that it runs at least once after enqueues.
    #[test]
    fn tracked_effect_re_runs_on_changes() {
        let q = ReactiveQueue::<String>::start();
        static HITS: AtomicUsize = AtomicUsize::new(0);

        // This effect body itself doesn't read signals, but the API tracks (seq, dirty, last)
        // before invoking the user closure, so it will re-run on queue dispatches.
        q.effect(|| {
            HITS.fetch_add(1, Ordering::Relaxed);
        });

        // Give time for the initial run
        thread::sleep(std::time::Duration::from_millis(50));
        let initial = HITS.load(Ordering::Relaxed);
        assert!(
            initial >= 1,
            "effect should run at least once on registration"
        );

        // Enqueue a few items; the effect should run again (tracked re-run)
        q.enqueue("a".into());
        q.enqueue("b".into());

        let ok = {
            let start = std::time::Instant::now();
            let timeout = std::time::Duration::from_millis(500);
            let mut ok = false;
            while start.elapsed() < timeout {
                if HITS.load(Ordering::Relaxed) > initial {
                    ok = true;
                    break;
                }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            ok
        };
        assert!(ok, "tracked effect should re-run after enqueues");

        q.stop();
    }
}
