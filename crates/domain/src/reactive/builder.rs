//! Linking helpers + reference-based PipelineBuilder + `pipeline!` macro.
//!
//! - `link_sync` / `link_async` / `link_async_with` wire queues stage-by-stage.
//! - Errors are forwarded as boxed trait objects (`DynErr`) to a single error queue.
//! - To satisfy `'static` captures, we clone the queue handles (they're cheap) and
//!   wrap async stage functions in `Arc<dyn Fn(...)>`.

use super::queue::ReactiveQueue;
use std::future::Future;
use std::sync::LazyLock;
use std::{error::Error as StdError, sync::Arc};
use tokio::sync::Semaphore;

// A global Tokio runtime to run async steps when we're not already inside a Tokio runtime.
static ASYNC_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build async runtime for reactive::builder")
});

fn spawn_on_rt<Fut>(fut: Fut)
where
    Fut: std::future::Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(fut);
    } else {
        ASYNC_RT.spawn(fut);
    }
}

/// Common alias for the error stream (boxed trait object).
pub type DynErr = Box<dyn StdError + Send + Sync + 'static>;

/// SYNC stage: A -> Result<B, E>
pub fn link_sync<A, B, E>(
    q_in: &ReactiveQueue<A>,
    q_out: &ReactiveQueue<B>,
    q_err: &ReactiveQueue<DynErr>,
    f: impl Fn(A) -> Result<B, E> + Send + 'static,
) where
    A: Send + 'static,
    B: Send + 'static,
    E: StdError + Send + Sync + 'static,
{
    // Capture OWNED handles so the closure is 'static.
    let q_out = q_out.clone();
    let q_err = q_err.clone();

    q_in.for_each_owned(move |a| match f(a) {
        Ok(b) => {
            let _ = q_out.enqueue(b);
        }
        Err(e) => {
            let _ = q_err.enqueue(Box::new(e));
        }
    });
}

/// Link an input queue to an output queue with an **async** step.
/// Spawns each item on a Tokio runtime (current if present, otherwise a shared global).
pub fn link_async<A, B, E, Fut>(
    q_in: &ReactiveQueue<A>,
    q_out: &ReactiveQueue<B>,
    q_err: &ReactiveQueue<DynErr>,
    f: impl Fn(A) -> Fut + Send + Sync + 'static,
) where
    A: Send + 'static,
    B: Send + 'static,
    E: StdError + Send + Sync + 'static,
    Fut: Future<Output = Result<B, E>> + Send + 'static,
{
    // Clone queue handles so they can be moved into 'static tasks
    let q_out = q_out.clone();
    let q_err = q_err.clone();

    // Put the step closure behind Arc so each task can clone it (avoids FnOnce move issues)
    let f = Arc::new(f);

    q_in.for_each_owned(move |a| {
        let q_out = q_out.clone();
        let q_err = q_err.clone();
        let f = f.clone();

        spawn_on_rt(async move {
            match f(a).await {
                Ok(b) => {
                    let _ = q_out.enqueue(b);
                }
                Err(e) => {
                    let _ = q_err.enqueue(DynErr::from(e));
                }
            }
        });
    });
}

/// Link an input queue to an output queue with an **async** step and a **max in-flight** bound.
/// Concurrency is limited via a Tokio semaphore; each task acquires a permit.
pub fn link_async_with<A, B, E, Fut>(
    q_in: &ReactiveQueue<A>,
    q_out: &ReactiveQueue<B>,
    q_err: &ReactiveQueue<DynErr>,
    max_in_flight: usize,
    f: impl Fn(A) -> Fut + Send + Sync + 'static,
) where
    A: Send + 'static,
    B: Send + 'static,
    E: StdError + Send + Sync + 'static,
    Fut: Future<Output = Result<B, E>> + Send + 'static,
{
    // Clone queue handles for 'static tasks
    let q_out = q_out.clone();
    let q_err = q_err.clone();

    // Shared semaphore guarding concurrency
    let gate = Arc::new(Semaphore::new(max_in_flight));

    // Arc the step closure so each task can call it
    let f = Arc::new(f);

    q_in.for_each_owned(move |a| {
        let q_out = q_out.clone();
        let q_err = q_err.clone();
        let gate = gate.clone();
        let f = f.clone();

        spawn_on_rt(async move {
            // Acquire inside the task so the permit lifetime matches the task lifetime
            let _permit = gate.acquire().await.expect("semaphore closed");

            match f(a).await {
                Ok(b) => {
                    let _ = q_out.enqueue(b);
                }
                Err(e) => {
                    let _ = q_err.enqueue(DynErr::from(e));
                }
            }
        });
    });
}

/// Reference-based builder (no cloning of queues required at callsite).
pub struct PipelineBuilder<'a, T: Send + 'static> {
    q_cur: &'a ReactiveQueue<T>,
    q_err: &'a ReactiveQueue<DynErr>,
}

impl<'a, T: Send + 'static> PipelineBuilder<'a, T> {
    pub fn new(q_start: &'a ReactiveQueue<T>, q_err: &'a ReactiveQueue<DynErr>) -> Self {
        Self {
            q_cur: q_start,
            q_err,
        }
    }

    pub fn map_sync<U, E, F>(self, next: &'a ReactiveQueue<U>, f: F) -> PipelineBuilder<'a, U>
    where
        U: Send + 'static,
        E: StdError + Send + Sync + 'static,
        F: Fn(T) -> Result<U, E> + Send + 'static,
    {
        link_sync(self.q_cur, next, self.q_err, f);
        PipelineBuilder {
            q_cur: next,
            q_err: self.q_err,
        }
    }

    pub fn map_async<U, E, Fut, F>(self, next: &'a ReactiveQueue<U>, f: F) -> PipelineBuilder<'a, U>
    where
        U: Send + 'static,
        E: StdError + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<U, E>> + Send + 'static,
        F: Fn(T) -> Fut + Send + Sync + 'static,
    {
        link_async(self.q_cur, next, self.q_err, f);
        PipelineBuilder {
            q_cur: next,
            q_err: self.q_err,
        }
    }

    pub fn map_async_with<U, E, Fut, F>(
        self,
        next: &'a ReactiveQueue<U>,
        max_in_flight: usize,
        f: F,
    ) -> PipelineBuilder<'a, U>
    where
        U: Send + 'static,
        E: StdError + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<U, E>> + Send + 'static,
        F: Fn(T) -> Fut + Send + Sync + 'static,
    {
        link_async_with(self.q_cur, next, self.q_err, max_in_flight, f);
        PipelineBuilder {
            q_cur: next,
            q_err: self.q_err,
        }
    }

    pub fn finish(self) -> (&'a ReactiveQueue<T>, &'a ReactiveQueue<DynErr>) {
        (self.q_cur, self.q_err)
    }
}

// ---------- internal engine ----------
#[doc(hidden)]
#[macro_export]
macro_rules! __pipeline {
    // ----- Phase 1: collect normalized routes -----

    // End: errors = qerr (optional trailing comma)
    (@collect ($($acc:tt)*); errors = $qerr:ident $(,)?) => {
        $crate::__pipeline!(@emit $qerr; $($acc)*);
    };

    // Route: ASYNC (bounded)  qin => async(step, max = N [, args...]?) => qout, ...
    (@collect ($($acc:tt)*);
        $qin:ident => async ( $step:ident , max = $max:expr $(, $($args:expr),* )? ) => $qout:ident , $($rest:tt)*
    ) => {
        $crate::__pipeline!(
            @collect
            ($($acc)* $qin => __ASYNC_PAREN__ $step , $max $(, $($args),* )? => $qout ,);
            $($rest)*
        );
    };

    // Route: ASYNC (simple)  qin => async step [(args...)?] => qout, ...
    (@collect ($($acc:tt)*);
        $qin:ident => async $step:ident $( ( $($args:expr),* $(,)? ) )? => $qout:ident , $($rest:tt)*
    ) => {
        $crate::__pipeline!(
            @collect
            ($($acc)* $qin => __ASYNC__ $step $( ( $($args),* ) )? => $qout ,);
            $($rest)*
        );
    };

    // Route: SYNC  qin => step [(args...)?] => qout, ...
    (@collect ($($acc:tt)*);
        $qin:ident => $step:ident $( ( $($args:expr),* $(,)? ) )? => $qout:ident , $($rest:tt)*
    ) => {
        $crate::__pipeline!(
            @collect
            ($($acc)* $qin => __SYNC__ $step $( ( $($args),* ) )? => $qout ,);
            $($rest)*
        );
    };

    // Allow stray commas between routes
    (@collect ($($acc:tt)*); , $($rest:tt)*) => {
        $crate::__pipeline!(@collect ($($acc)*); $($rest)*);
    };

    // ----- Phase 2: emit link_* calls -----

    // Done
    (@emit $qerr:ident; ) => {};

    // Emit: ASYNC (bounded)
    (@emit $qerr:ident;
        $qin:ident => __ASYNC_PAREN__ $step:ident , $max:expr $(, $($args:expr),* )? => $qout:ident , $($rest:tt)*
    ) => {
        $crate::reactive::builder::link_async_with(
            &$qin, &$qout, &$qerr, $max,
            move |x| $step(x $(, $($args),* )? )
        );
        $crate::__pipeline!(@emit $qerr; $($rest)*);
    };

    // Emit: ASYNC (simple)
    (@emit $qerr:ident;
        $qin:ident => __ASYNC__ $step:ident $( ( $($args:expr),* ) )? => $qout:ident , $($rest:tt)*
    ) => {
        $crate::reactive::builder::link_async(
            &$qin, &$qout, &$qerr,
            move |x| $step(x $(, $($args),* )? )
        );
        $crate::__pipeline!(@emit $qerr; $($rest)*);
    };

    // Emit: SYNC
    (@emit $qerr:ident;
        $qin:ident => __SYNC__ $step:ident $( ( $($args:expr),* ) )? => $qout:ident , $($rest:tt)*
    ) => {
        $crate::reactive::builder::link_sync(
            &$qin, &$qout, &$qerr,
            move |x| $step(x $(, $($args),* )? )
        );
        $crate::__pipeline!(@emit $qerr; $($rest)*);
    };
}

// ---------- public wrapper (single hop; no recursion here) ----------
#[macro_export]
macro_rules! pipeline {
    ( $($body:tt)* ) => {
        $crate::__pipeline!(@collect (); $($body)*);
    };
}
pub use pipeline;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactive::queue::ReactiveQueue;
    use leptos_reactive::SignalGet; // <- needed for `.get()` on RwSignal
    use std::path::PathBuf;
    use std::sync::{
        atomic::{AtomicIsize, AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};

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

    // Small async helper
    async fn short_sleep_ms(ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    // ---------- sync link tests ----------

    #[test]
    fn link_sync_routes_ok_and_err() {
        let q_in = ReactiveQueue::<i32>::start();
        let q_out = ReactiveQueue::<String>::start();
        let q_err = ReactiveQueue::<DynErr>::start();

        let (ok_tx, ok_rx) = mpsc::channel::<String>();
        let (er_tx, er_rx) = mpsc::channel::<String>();

        q_out.for_each_owned(move |s| {
            ok_tx.send(s).unwrap();
        });
        q_err.for_each_owned(move |e| {
            er_tx.send(e.to_string()).unwrap();
        });

        // even -> Ok, odd -> Err
        link_sync(&q_in, &q_out, &q_err, |n: i32| {
            if n % 2 == 0 {
                Ok(format!("ok-{n}"))
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("bad-{n}"),
                ))
            }
        });

        for n in 0..6 {
            q_in.enqueue(n).unwrap();
        }

        // expect 3 oks
        let oks: Vec<_> = (0..3)
            .map(|_| ok_rx.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect();
        // expect 3 errs
        let ers: Vec<_> = (0..3)
            .map(|_| er_rx.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect();

        assert!(oks.contains(&"ok-0".into()));
        assert!(oks.contains(&"ok-2".into()));
        assert!(oks.contains(&"ok-4".into()));

        assert!(ers.iter().any(|s| s.contains("bad-1")));
        assert!(ers.iter().any(|s| s.contains("bad-3")));
        assert!(ers.iter().any(|s| s.contains("bad-5")));

        q_in.stop();
        q_out.stop();
        q_err.stop();
    }

    #[test]
    fn link_sync_consumer_panic_is_contained() {
        let q_in = ReactiveQueue::<u32>::start();
        let q_out = ReactiveQueue::<u32>::start();
        let q_err = ReactiveQueue::<DynErr>::start();

        // first item panics, then Ok
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        link_sync(&q_in, &q_out, &q_err, |_n| {
            if SEEN.fetch_add(1, Ordering::Relaxed) == 0 {
                panic!("boom in mapper");
            }
            Ok::<u32, std::io::Error>(42)
        });

        let (tx, rx) = mpsc::channel::<u32>();
        q_out.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        q_in.enqueue(1).unwrap(); // panics inside consumer; should be contained
        q_in.enqueue(2).unwrap(); // should process to 42

        let got = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(got, 42);

        q_in.stop();
        q_out.stop();
        q_err.stop();
    }

    // ---------- async link tests ----------

    #[tokio::test(flavor = "current_thread")]
    async fn link_async_routes_ok_and_err() {
        let q_in = ReactiveQueue::<i32>::start();
        let q_out = ReactiveQueue::<String>::start();
        let q_err = ReactiveQueue::<DynErr>::start();

        let (ok_tx, ok_rx) = mpsc::channel::<String>();
        let (er_tx, er_rx) = mpsc::channel::<String>();

        q_out.for_each_owned(move |s| {
            ok_tx.send(s).unwrap();
        });
        q_err.for_each_owned(move |e| {
            er_tx.send(e.to_string()).unwrap();
        });

        link_async(&q_in, &q_out, &q_err, |n: i32| async move {
            short_sleep_ms((n.abs() as u64) % 5).await;
            if n >= 0 {
                Ok(format!("ok-{n}"))
            } else {
                Err(std::io::Error::new(std::io::ErrorKind::Other, "neg"))
            }
        });

        for n in [-2, -1, 0, 1, 2, 3] {
            q_in.enqueue(n).unwrap();
        }

        // We don’t assert order for async; just presence and counts.
        let oks: Vec<_> = (0..4)
            .map(|_| ok_rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect();
        let ers: Vec<_> = (0..2)
            .map(|_| er_rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect();

        assert!(oks.iter().any(|s| s == "ok-0"));
        assert!(oks.iter().any(|s| s == "ok-1"));
        assert!(oks.iter().any(|s| s == "ok-2"));
        assert!(oks.iter().any(|s| s == "ok-3"));
        assert_eq!(ers.len(), 2);

        q_in.stop();
        q_out.stop();
        q_err.stop();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn link_async_with_respects_in_flight_bound() {
        use std::sync::atomic::{AtomicIsize, Ordering};
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };

        let qa = ReactiveQueue::<u32>::start();
        let qb = ReactiveQueue::<u32>::start();
        let qerr = ReactiveQueue::<DynErr>::start();

        // Collect qb outputs
        let (tx_ok, rx_ok) = mpsc::channel::<u32>();
        qb.for_each_owned(move |x| {
            tx_ok.send(x).unwrap();
        });

        // Collect errors (should be none)
        let err_count: std::sync::Arc<std::sync::atomic::AtomicUsize> =
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let err_count = err_count.clone();
            qerr.for_each_owned(move |_e| {
                err_count.fetch_add(1, Ordering::Relaxed);
            });
        }

        // Concurrency tracking
        static CUR: AtomicIsize = AtomicIsize::new(0);
        static PEAK: AtomicIsize = AtomicIsize::new(0);

        async fn bump(n: u32) -> Result<u32, std::io::Error> {
            // record concurrent in-flight
            let now = CUR.fetch_add(1, Ordering::SeqCst) + 1;
            loop {
                let old = PEAK.load(Ordering::SeqCst);
                if now > old {
                    if PEAK
                        .compare_exchange(old, now, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        break;
                    }
                } else {
                    break;
                }
            }
            // simulate work
            tokio::time::sleep(Duration::from_millis(30)).await;

            CUR.fetch_sub(1, Ordering::SeqCst);
            Ok::<u32, std::io::Error>(n + 1) // map 0..=10 → 1..=11
        }

        // Wire: qa → qb with max_in_flight = 2
        link_async_with::<u32, u32, std::io::Error, _>(&qa, &qb, &qerr, 2, |x| bump(x));

        // Enqueue 11 items
        for n in 0..=10 {
            qa.enqueue(n).unwrap();
        }

        // Wait until we've collected all 11 outputs (deadline-based loop)
        let expected = 11usize;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut got = Vec::with_capacity(expected);
        while got.len() < expected {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Ok(x) = rx_ok.recv_timeout(remaining) {
                got.push(x);
            }
        }

        // Assert we really got 11 mapped values 1..=11
        got.sort();
        assert_eq!(got, (1u32..=11u32).collect::<Vec<_>>());

        // Assert concurrency bound held
        assert!(PEAK.load(Ordering::SeqCst) <= 2);

        // Assert no errors routed
        assert_eq!(err_count.load(Ordering::Relaxed), 0);

        qa.stop();
        qb.stop();
        qerr.stop();
    }

    // ---------- PipelineBuilder tests ----------

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_builder_chains_sync_then_async() {
        let q_a = ReactiveQueue::<i32>::start();
        let q_b = ReactiveQueue::<String>::start();
        let q_c = ReactiveQueue::<usize>::start();
        let q_err = ReactiveQueue::<DynErr>::start();

        // Collect C
        let (tx, rx) = mpsc::channel::<usize>();
        q_c.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        // A(i32) -> B(String) sync, then B -> C(usize) async
        PipelineBuilder::new(&q_a, &q_err)
            .map_sync(&q_b, |n| Ok::<_, std::io::Error>(format!("N={n}")))
            .map_async(&q_c, |s: String| async move {
                short_sleep_ms(10).await;
                Ok::<_, std::io::Error>(s.len())
            })
            .finish();

        for n in 0..5 {
            q_a.enqueue(n).unwrap();
        }

        let mut got = Vec::new();
        for _ in 0..5 {
            got.push(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        got.sort();
        assert_eq!(got, vec![3, 3, 3, 3, 3]);

        q_a.stop();
        q_b.stop();
        q_c.stop();
        q_err.stop();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_builder_routes_errors() {
        let q_a = ReactiveQueue::<i32>::start();
        let q_b = ReactiveQueue::<i32>::start();
        let q_err = ReactiveQueue::<DynErr>::start();

        let (ok_tx, ok_rx) = mpsc::channel::<i32>();
        let (er_tx, er_rx) = mpsc::channel::<String>();
        q_b.for_each_owned(move |x| ok_tx.send(x).unwrap());
        q_err.for_each_owned(move |e| er_tx.send(e.to_string()).unwrap());

        PipelineBuilder::new(&q_a, &q_err)
            .map_async(&q_b, |n| async move {
                if n >= 0 {
                    Ok::<_, std::io::Error>(n * 2)
                } else {
                    Err(std::io::Error::new(std::io::ErrorKind::Other, "neg"))
                }
            })
            .finish();

        for n in [-2, -1, 0, 1, 2] {
            q_a.enqueue(n).unwrap();
        }

        // expect 3 oks and 2 errs
        let oks: Vec<_> = (0..3)
            .map(|_| ok_rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect();
        let ers: Vec<_> = (0..2)
            .map(|_| er_rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect();

        assert!(oks.contains(&0));
        assert!(oks.contains(&2));
        assert!(oks.contains(&4));
        assert_eq!(ers.len(), 2);

        q_a.stop();
        q_b.stop();
        q_err.stop();
    }

    // ---------- Macro tests ----------

    // Simple sync and async functions used by the macro:
    fn f_sync_to_string(n: i32) -> Result<String, std::io::Error> {
        Ok(format!("S{n}"))
    }
    async fn f_async_len(s: String) -> Result<usize, std::io::Error> {
        Ok(s.len())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn macro_pipeline_sync_then_async() {
        let qa = ReactiveQueue::<i32>::start();
        let qb = ReactiveQueue::<String>::start();
        let qc = ReactiveQueue::<usize>::start();
        let qerr = ReactiveQueue::<DynErr>::start();

        let (tx, rx) = mpsc::channel::<usize>();
        qc.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        crate::pipeline! {
            qa => f_sync_to_string => qb,
            qb => async f_async_len => qc,
            errors = qerr
        }

        for n in 1..=3 {
            qa.enqueue(n).unwrap();
        }
        let mut got = Vec::new();
        for _ in 0..3 {
            got.push(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        got.sort();
        // "S1","S2","S3" → lengths 2,2,2
        assert_eq!(got, vec![2, 2, 2]);

        qa.stop();
        qb.stop();
        qc.stop();
        qerr.stop();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn macro_pipeline_async_with_max() {
        let qa = ReactiveQueue::<u32>::start();
        let qb = ReactiveQueue::<u32>::start();
        let qerr = ReactiveQueue::<DynErr>::start();

        let (tx, rx) = mpsc::channel::<u32>();
        qb.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        static CUR: AtomicIsize = AtomicIsize::new(0);
        static PEAK: AtomicIsize = AtomicIsize::new(0);

        async fn bump(n: u32) -> Result<u32, std::io::Error> {
            let now = CUR.fetch_add(1, Ordering::SeqCst) + 1;
            loop {
                let old = PEAK.load(Ordering::SeqCst);
                if now > old {
                    if PEAK
                        .compare_exchange(old, now, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        break;
                    }
                } else {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            CUR.fetch_sub(1, Ordering::SeqCst);
            Ok(n + 10)
        }

        pipeline! {
            qa => async(bump, max = 2) => qb,
            errors = qerr
        }

        for n in 0..8 {
            qa.enqueue(n).unwrap();
        }
        let mut out = Vec::new();
        for _ in 0..8 {
            out.push(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        out.sort();
        assert_eq!(out, (10..18).collect::<Vec<_>>());
        assert!(PEAK.load(Ordering::SeqCst) <= 2);

        qa.stop();
        qb.stop();
        qerr.stop();
    }

    // ---------- Signals inside tests (ensure trait import works) ----------

    #[test]
    fn effect_trait_import_is_in_scope_here() {
        // This test only checks that we can read signals inside a test-owned effect,
        // proving that `SignalGet` is in scope.
        let q = ReactiveQueue::<i32>::start();
        let seq = q.seq; // copy handles
        let dirty = q.dirty;

        q.effect(move || {
            let _ = seq.get();
            let _ = dirty.get();
        });

        q.enqueue(1).unwrap();
        assert!(wait_until(Duration::from_millis(400), || q.read_seq() >= 1));
        q.stop();
    }

    // ---------- Edge cases ----------

    #[test]
    fn enqueue_after_stop_is_error_and_links_are_harmless() {
        let q_in = ReactiveQueue::<i32>::start();
        let q_out = ReactiveQueue::<i32>::start();
        let q_err = ReactiveQueue::<DynErr>::start();

        // link, then stop input
        link_sync(&q_in, &q_out, &q_err, |x| Ok::<_, std::io::Error>(x + 1));
        q_in.stop();

        // Enqueue now should fail
        assert!(q_in.enqueue(5).is_err());

        q_out.stop();
        q_err.stop();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn builder_with_pathbuf_and_realistic_steps() {
        // A realistic small chain: PathBuf -> String (filename) -> usize (len)
        let qa = ReactiveQueue::<PathBuf>::start();
        let qb = ReactiveQueue::<String>::start();
        let qc = ReactiveQueue::<usize>::start();
        let qerr = ReactiveQueue::<DynErr>::start();

        let (tx, rx) = mpsc::channel::<usize>();
        qc.for_each_owned(move |x| {
            tx.send(x).unwrap();
        });

        PipelineBuilder::new(&qa, &qerr)
            .map_sync(&qb, |p: PathBuf| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no filename"))
            })
            .map_async(&qc, |s: String| async move {
                short_sleep_ms(5).await;
                Ok::<_, std::io::Error>(s.len())
            })
            .finish();

        qa.enqueue(PathBuf::from("/tmp/a.txt")).unwrap();
        qa.enqueue(PathBuf::from("/tmp/β")).unwrap(); // non-ASCII okay
        qa.enqueue(PathBuf::from("/tmp/.")).unwrap(); // filename "." -> Some(".")

        let mut out = Vec::new();
        for _ in 0..3 {
            out.push(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        }
        out.sort();
        assert!(out.contains(&5)); // "a.txt".len() = 5
        assert!(out.iter().all(|n| *n >= 1));

        qa.stop();
        qb.stop();
        qc.stop();
        qerr.stop();
    }
}
