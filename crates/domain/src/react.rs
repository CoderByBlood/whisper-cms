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
