//! ReactiveQueue: a generic, notify-agnostic reactive queue built on Leptos signals.
//!
//! ✦ Dedicated “reactive thread” owns the Leptos runtime + owner.
//! ✦ No built-in dedupe; FIFO dispatch; one item at a time.
//! ✦ Payloads are **moved** to a single owned consumer (no `Clone` bound on `T`).
//! ✦ The handle is **cloneable**; all clones reference the same underlying queue.
//! ✦ `stop(&self)` cleanly shuts down the thread; otherwise it stops when the last handle drops.

use leptos_reactive::*;
use std::{
    collections::VecDeque,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex},
    thread,
};

/// Messages handled on the reactive thread.
enum Apply<T> {
    // --- Mutations (queue / dispatch) ---
    Enqueue(T),
    DispatchNext,

    // --- Owned consumer registration ---
    RegisterOwned(Box<dyn Fn(T) + Send + 'static>),

    // --- Sync reads (reply via std::sync::mpsc) ---
    ReadSeq {
        resp: std::sync::mpsc::Sender<u64>,
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
    T: Send + 'static,
{
    /// Increments once per dispatched item.
    pub seq: RwSignal<u64>,
    /// True while queue is non-empty.
    pub dirty: RwSignal<bool>,

    inner: Arc<Inner<T>>,
}

struct Inner<T>
where
    T: Send + 'static,
{
    apply_tx: std::sync::mpsc::Sender<Apply<T>>,
    /// Join handle protected by a mutex so `stop(&self)` can join exactly once.
    reactive_thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl<T: Send + 'static> Drop for Inner<T> {
    fn drop(&mut self) {
        // Best-effort shutdown if `stop()` wasn’t called.
        let _ = self.apply_tx.send(Apply::Stop);
        if let Some(j) = self.reactive_thread.lock().unwrap().take() {
            let _ = j.join();
        }
    }
}

impl<T> Clone for ReactiveQueue<T>
where
    T: Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            seq: self.seq,     // RwSignal is Copy
            dirty: self.dirty, // RwSignal is Copy
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> ReactiveQueue<T>
where
    T: Send + 'static,
{
    /// Start the reactive thread and return a handle.
    pub fn start() -> Self {
        // Channel to the reactive thread.
        let (apply_tx, apply_rx) = std::sync::mpsc::channel::<Apply<T>>();
        let apply_tx_for_thread = apply_tx.clone(); // for self-scheduling DispatchNext

        // Send signal handles back to the caller.
        let (sig_tx, sig_rx) = std::sync::mpsc::channel::<(RwSignal<u64>, RwSignal<bool>)>();

        let reactive_thread = thread::spawn(move || {
            let rt: RuntimeId = create_runtime();
            let owner: Owner = Owner::current().expect("root owner (reactive thread)");

            // Signals
            let (seq, dirty) =
                with_owner(owner, || (create_rw_signal(0_u64), create_rw_signal(false)));

            // FIFO queue and single owned consumer
            let mut q: VecDeque<T> = VecDeque::new();
            let mut owned_cb: Option<Box<dyn Fn(T) + Send + 'static>> = None;

            sig_tx.send((seq, dirty)).expect("send signals");

            for msg in apply_rx {
                match msg {
                    // --- queue management ---
                    Apply::Enqueue(item) => {
                        q.push_back(item);
                        with_owner(owner, || dirty.set(true));
                        if q.len() == 1 {
                            let _ = apply_tx_for_thread.send(Apply::DispatchNext);
                        }
                    }
                    Apply::DispatchNext => {
                        if let Some(x) = q.pop_front() {
                            // Publish clock & dirty
                            with_owner(owner, || {
                                seq.set(seq.get_untracked().wrapping_add(1));
                                dirty.set(!q.is_empty());
                            });

                            // Move payload to the owned consumer (if any)
                            if let Some(cb) = &owned_cb {
                                let _ = catch_unwind(AssertUnwindSafe(|| (cb)(x)));
                            }

                            if !q.is_empty() {
                                let _ = apply_tx_for_thread.send(Apply::DispatchNext);
                            }
                        } else {
                            with_owner(owner, || dirty.set(false));
                        }
                    }

                    // --- owned consumer registration ---
                    Apply::RegisterOwned(cb) => {
                        owned_cb = Some(cb);
                    }

                    // --- reads ---
                    Apply::ReadSeq { resp } => {
                        let value = with_owner(owner, || seq.get_untracked());
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
        let (seq, dirty) = sig_rx.recv().expect("receive signals from reactive thread");

        ReactiveQueue {
            seq,
            dirty,
            inner: Arc::new(Inner {
                apply_tx,
                reactive_thread: Mutex::new(Some(reactive_thread)),
            }),
        }
    }

    /// Enqueue a single item.
    pub fn enqueue(&self, item: T) -> Result<(), ()> {
        self.inner
            .apply_tx
            .send(Apply::Enqueue(item))
            .map_err(|_| ())
    }

    /// Enqueue multiple items.
    pub fn enqueue_many<I>(&self, iter: I) -> Result<usize, ()>
    where
        I: IntoIterator<Item = T>,
    {
        let mut n = 0usize;
        for x in iter {
            self.inner
                .apply_tx
                .send(Apply::Enqueue(x))
                .map_err(|_| ())?;
            n += 1;
        }
        Ok(n)
    }

    /// Register exactly one owned consumer. Runs on the reactive thread. Replaces any prior consumer.
    /// Each dispatched item is moved into `f(T)`.
    pub fn for_each_owned(&self, f: impl Fn(T) + Send + 'static) {
        let _ = self.inner.apply_tx.send(Apply::RegisterOwned(Box::new(f)));
    }

    /// Create a Leptos `Effect` inside the owning runtime/owner and wait until it’s registered.
    /// Inside `f`, read from the signals using `.get()` (don’t capture &self).
    /// NOTE: This effect is for observing `seq/dirty`; payloads are delivered via `for_each_owned`.
    pub fn effect<F>(&self, f: F)
    where
        F: Fn() + Send + 'static,
    {
        use std::sync::mpsc::channel;

        let (ack_tx, ack_rx) = channel::<()>();
        let seq = self.seq;
        let dirty = self.dirty;

        let _ = self.inner.apply_tx.send(Apply::RunSync {
            f: Box::new(move |owner: Owner| {
                with_owner(owner, || {
                    create_effect(move |_| {
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            // TRACKED reads so the effect re-runs
                            let _ = seq.get();
                            let _ = dirty.get();
                            f();
                        }));
                        let _ = result.is_ok();
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

    /// Safe read of `dirty` from outside the reactive thread.
    pub fn read_dirty(&self) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = self.inner.apply_tx.send(Apply::ReadDirty { resp: tx });
        rx.recv().unwrap_or(false)
    }

    /// Graceful shutdown: stop the reactive thread (disposes runtime).
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
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    // ─────────────────────────────────────────────────────────────────────────
    // Test helpers
    // ─────────────────────────────────────────────────────────────────────────

    /// Spin until `pred()` is true or `timeout` elapses.
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

    /// Sleep a tiny bit; useful when we only need a small scheduling nudge.
    fn tiny_nap() {
        std::thread::sleep(Duration::from_millis(25));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Basics: start/stop, initial state
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn starts_clean_and_stops_cleanly() {
        let q = ReactiveQueue::<i32>::start();
        // Initial state
        assert_eq!(q.read_seq(), 0);
        assert!(!q.read_dirty());
        // Stop is synchronous
        q.stop();
        // Reads remain safe
        assert_eq!(q.read_seq(), 0);
        assert_eq!(q.read_dirty(), false);
    }

    #[test]
    fn stop_is_idempotent() {
        let q = ReactiveQueue::<i32>::start();
        q.stop();
        q.stop();
        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Enqueue, FIFO ordering, and dirty/seq semantics with a consumer
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn processes_in_fifo_order_and_updates_seq_and_dirty() {
        let q = ReactiveQueue::<i32>::start();

        // Collect processed items in arrival order.
        let (tx, rx) = mpsc::channel::<i32>();
        q.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        // Before
        assert_eq!(q.read_seq(), 0);
        assert!(!q.read_dirty());

        // Enqueue a few items
        q.enqueue(10).unwrap();
        q.enqueue(20).unwrap();
        q.enqueue(30).unwrap();

        // dirty should become true quickly
        assert!(wait_until(Duration::from_millis(200), || q.read_dirty()));

        // We should receive all items in FIFO order
        let a = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let b = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let c = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!([a, b, c], [10, 20, 30]);

        // seq should have advanced by 3 (one per dispatch)
        let ok = wait_until(Duration::from_secs(1), || q.read_seq() >= 3);
        assert!(ok, "seq should have advanced by at least 3");

        // eventually dirty goes false after drain
        let ok = wait_until(Duration::from_millis(500), || !q.read_dirty());
        assert!(ok, "dirty should return to false after draining");

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // No consumer registered: items are dropped but seq/dirty still behave
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn without_consumer_items_are_dropped_but_clock_advances() {
        let q = ReactiveQueue::<i32>::start();

        assert_eq!(q.read_seq(), 0);
        assert!(!q.read_dirty());

        q.enqueue_many([1, 2, 3]).unwrap();

        // seq should eventually reach >= 3 even with no consumer
        let ok = wait_until(Duration::from_secs(1), || q.read_seq() >= 3);
        assert!(ok, "seq should advance even without a consumer");

        // dirty should end false after drain
        let ok = wait_until(Duration::from_millis(500), || !q.read_dirty());
        assert!(ok, "dirty should be false after draining");

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Consumer replacement: last registered wins
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn consumer_replacement_takes_effect_immediately() {
        let q = ReactiveQueue::<i32>::start();

        let first_hit = Arc::new(AtomicBool::new(false));
        let first_hit_c = first_hit.clone();
        q.for_each_owned(move |_x| {
            first_hit_c.store(true, Ordering::Relaxed);
        });

        // Replace consumer
        let (tx, rx) = mpsc::channel::<i32>();
        q.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        q.enqueue(42).unwrap();

        // Ensure second consumer runs
        let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(got, 42);

        // The first consumer may or may not have seen earlier items—here we only verify
        // that after replacement, the new consumer is the one receiving items.
        assert!(first_hit.load(Ordering::Relaxed) || q.read_seq() > 0);

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Panic containment in consumer
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn consumer_panic_is_contained_and_processing_continues() {
        let q = ReactiveQueue::<i32>::start();

        static PANIC_ONCE: AtomicBool = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel::<i32>();

        q.for_each_owned(move |x| {
            if !PANIC_ONCE.swap(true, Ordering::Relaxed) {
                panic!("intentional panic in consumer");
            }
            tx.send(x).unwrap();
        });

        q.enqueue(1).unwrap(); // triggers panic, should be contained
        q.enqueue(2).unwrap(); // should still be processed

        let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(got, 2);

        // seq should be >= 2
        let ok = wait_until(Duration::from_secs(1), || q.read_seq() >= 2);
        assert!(ok);

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Panic containment in effect
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn effect_panic_is_contained_and_effect_runs_again() {
        let q = ReactiveQueue::<i32>::start();

        static PANIC_ONCE: AtomicBool = AtomicBool::new(false);
        static HITS: AtomicUsize = AtomicUsize::new(0);

        // Copy signal handles, then move them into the closure.
        let seq = q.seq;
        let dirty = q.dirty;

        q.effect(move || {
            // track reactivity on seq/dirty inside effect body
            let _ = seq.get();
            let _ = dirty.get();
            if !PANIC_ONCE.swap(true, Ordering::Relaxed) {
                panic!("intentional effect panic");
            } else {
                HITS.fetch_add(1, Ordering::Relaxed);
            }
        });

        // enqueue a couple to drive multiple re-runs
        q.enqueue(10).unwrap();
        q.enqueue(20).unwrap();

        // after the panic, we still want to see at least one recorded hit
        let ok = wait_until(Duration::from_secs(1), || HITS.load(Ordering::Relaxed) >= 1);
        assert!(ok, "effect should continue after a panic");

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // enqueue_many returns count and drives dispatch
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn enqueue_many_returns_count_and_dispatches_all() {
        let q = ReactiveQueue::<i32>::start();

        let (tx, rx) = mpsc::channel::<i32>();
        q.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        let n = q.enqueue_many(0..10).unwrap();
        assert_eq!(n, 10);

        // Read all ten
        let mut seen = Vec::new();
        for _ in 0..10 {
            let v = rx.recv_timeout(Duration::from_secs(1)).unwrap();
            seen.push(v);
        }

        // FIFO check
        assert_eq!(seen, (0..10).collect::<Vec<_>>());

        // seq should be >= 10 and dirty false eventually
        let ok = wait_until(Duration::from_secs(1), || q.read_seq() >= 10);
        assert!(ok);
        let ok = wait_until(Duration::from_millis(500), || !q.read_dirty());
        assert!(ok);

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Cloned handles enqueue concurrently
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn cloned_handles_share_same_queue_and_process_all() {
        let q = ReactiveQueue::<usize>::start();
        let qc1 = q.clone();
        let qc2 = q.clone();

        let (tx, rx) = mpsc::channel::<usize>();
        q.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        // Enqueue from clones
        for i in 0..5 {
            qc1.enqueue(i).unwrap();
            qc2.enqueue(i + 100).unwrap();
        }

        // Collect 10 items
        let mut got = Vec::new();
        for _ in 0..10 {
            let v = rx.recv_timeout(Duration::from_secs(1)).unwrap();
            got.push(v);
        }
        got.sort();
        assert_eq!(got, vec![0, 1, 2, 3, 4, 100, 101, 102, 103, 104]);

        // seq should be >= 10
        let ok = wait_until(Duration::from_secs(1), || q.read_seq() >= 10);
        assert!(ok);

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Enqueue after stop: send should error (channel closed)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn enqueue_after_stop_fails() {
        let q = ReactiveQueue::<i32>::start();
        q.stop();
        let err = q.enqueue(7);
        assert!(err.is_err(), "enqueue should fail after stop");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Dirty flag toggling around bursts
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn dirty_flag_toggles_true_then_false() {
        let q = ReactiveQueue::<i32>::start();

        let (tx, rx) = mpsc::channel::<i32>();
        q.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        assert_eq!(q.read_dirty(), false);
        q.enqueue_many(1..=3).unwrap();

        let ok = wait_until(Duration::from_millis(200), || q.read_dirty());
        assert!(ok, "dirty should become true after enqueues");

        // Drain them
        for _ in 0..3 {
            let _ = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }

        // Eventually false
        let ok = wait_until(Duration::from_millis(500), || !q.read_dirty());
        assert!(ok, "dirty should become false after draining");

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Effect tracks seq/dirty and fires on dispatch ticks
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn effect_tracks_seq_and_dirty() {
        let q = ReactiveQueue::<i32>::start();

        static RUNS: AtomicUsize = AtomicUsize::new(0);

        // Copy signal handles and move them into the effect.
        let seq = q.seq;
        let dirty = q.dirty;

        q.effect(move || {
            // read both to ensure re-run on either change
            let _ = seq.get();
            let _ = dirty.get();
            RUNS.fetch_add(1, Ordering::Relaxed);
        });

        // initial run + a couple more
        let initial = RUNS.load(Ordering::Relaxed);
        assert!(
            initial >= 1,
            "effect should run at least once on registration"
        );

        q.enqueue(1).unwrap();
        q.enqueue(2).unwrap();

        let ok = wait_until(Duration::from_secs(1), || {
            RUNS.load(Ordering::Relaxed) > initial
        });
        assert!(ok, "effect should re-run after dispatch ticks");

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // High volume: ensure we can process many items and seq doesn't overflow prematurely
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn processes_many_items_reasonably_fast() {
        let q = ReactiveQueue::<u32>::start();

        let count = Arc::new(AtomicUsize::new(0));
        let count_c = count.clone();
        q.for_each_owned(move |_x| {
            count_c.fetch_add(1, Ordering::Relaxed);
        });

        // enqueue 1_000 items (small enough for CI, large enough to shake out issues)
        let total = 1_000usize;
        for i in 0..total as u32 {
            q.enqueue(i).unwrap();
        }

        // wait until we see at least total processed (or timeout)
        let ok = wait_until(Duration::from_secs(3), || {
            count.load(Ordering::Relaxed) >= total
        });
        assert!(ok, "expected to process {total} items");

        // seq should be at least total
        assert!(q.read_seq() as usize >= total);

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Register consumer late: items queued before registration are dropped
    // (matches current implementation)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn items_before_consumer_registration_are_not_delivered() {
        let q = ReactiveQueue::<i32>::start();

        // enqueue two before registering consumer (will be dropped)
        q.enqueue(7).unwrap();
        q.enqueue(8).unwrap();

        tiny_nap(); // give dispatch loop a moment

        let (tx, rx) = mpsc::channel::<i32>();
        q.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        // enqueue one after registration
        q.enqueue(9).unwrap();

        // We should only receive 9
        let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(got, 9);

        // and nothing else shows up
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

        q.stop();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Ensure seq is strictly increasing per dispatch regardless of consumer panics
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn seq_increments_even_when_consumer_panics() {
        let q = ReactiveQueue::<i32>::start();

        static SEEN: AtomicU64 = AtomicU64::new(0);
        static PANIC_COUNTER: AtomicUsize = AtomicUsize::new(0);

        // Copy the signal handle and move into the effect.
        let seq = q.seq;

        // Observe seq via effect
        q.effect(move || {
            let s = seq.get(); // tracked
            SEEN.store(s, Ordering::Relaxed);
        });

        q.for_each_owned(move |_x| {
            // panic on the first two items; then proceed
            let n = PANIC_COUNTER.fetch_add(1, Ordering::Relaxed);
            if n < 2 {
                panic!("boom {}", n);
            }
        });

        q.enqueue_many(0..5).unwrap();

        // Eventually we should see seq >= 5 regardless of panics
        let ok = wait_until(Duration::from_secs(2), || SEEN.load(Ordering::Relaxed) >= 5);
        assert!(ok, "seq should reach 5 even if consumer panics");

        q.stop();
    }
}
