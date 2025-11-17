use futures::StreamExt;
use std::future::Future;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const MAX_IN_FLIGHT: usize = 16;

/// Generic driver:
///   - takes `rx_in: Receiver<A>`
///   - uses `chain: Fn(A) -> Fut<Output = Result<B,E>>`
///   - sends all Ok(B) to `tx_ok`
///   - sends all Err(E) to `tx_err`
///   - returns when:
///       • the input channel is closed, and
///       • all in-flight items have completed.
pub fn pipeline_task<A, B, E, F, Fut>(
    rx_in: mpsc::Receiver<A>,
    tx_ok: mpsc::Sender<B>,
    tx_err: mpsc::Sender<E>,
    chain: F,
) -> impl Future<Output = ()> + Send + 'static
where
    A: Send + 'static,
    B: Send + 'static,
    E: Send + 'static,
    F: Fn(A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<B, E>> + Send + 'static,
{
    ReceiverStream::new(rx_in)
        // Turn each input into its processing future.
        .map(move |a| chain(a))
        // Run up to MAX_IN_FLIGHT futures concurrently.
        .buffer_unordered(MAX_IN_FLIGHT)
        // Route results to the appropriate output channel.
        .for_each(move |res| {
            let tx_ok = tx_ok.clone();
            let tx_err = tx_err.clone();
            async move {
                match res {
                    Ok(v) => {
                        let _ = tx_ok.send(v).await;
                    }
                    Err(e) => {
                        let _ = tx_err.send(e).await;
                    }
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    // Simple error type for tests.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestError(&'static str);

    const TEST_TIMEOUT: Duration = Duration::from_secs(3);

    // ─────────────────────────────────────────────
    // 1. Happy path: all inputs succeed, all outputs land in tx_ok.
    // ─────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_task_routes_all_ok_results() {
        let (tx_in, rx_in) = mpsc::channel::<i32>(8);
        let (tx_ok, mut rx_ok) = mpsc::channel::<i32>(8);
        let (tx_err, mut rx_err) = mpsc::channel::<TestError>(8);

        // Chain: multiply by 2, always Ok
        let chain = |n: i32| async move { Ok::<_, TestError>(n * 2) };

        let fut = pipeline_task(rx_in, tx_ok.clone(), tx_err.clone(), chain);

        // Send 0..5 then close input.
        for n in 0..5 {
            tx_in.send(n).await.unwrap();
        }
        drop(tx_in);

        // Wait for pipeline to drain (defensively bounded).
        timeout(TEST_TIMEOUT, fut)
            .await
            .expect("pipeline_task_routes_all_ok_results timed out");

        // Drain all OK outputs.
        let mut got = Vec::new();
        while let Some(v) = rx_ok.recv().await {
            got.push(v);
        }
        got.sort();
        assert_eq!(got, vec![0, 2, 4, 6, 8]);

        // No errors should have been produced.
        assert!(timeout(Duration::from_millis(100), rx_err.recv())
            .await
            .ok()
            .flatten()
            .is_none());
    }

    // ─────────────────────────────────────────────
    // 2. Mixed Ok/Err: even numbers succeed, odd numbers fail.
    //    Verify routing to tx_ok / tx_err.
    // ─────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_task_routes_errors_and_successes() {
        let (tx_in, rx_in) = mpsc::channel::<i32>(16);
        let (tx_ok, mut rx_ok) = mpsc::channel::<i32>(16);
        let (tx_err, mut rx_err) = mpsc::channel::<TestError>(16);

        // Even -> Ok, Odd -> Err
        let chain = |n: i32| async move {
            if n % 2 == 0 {
                Ok::<_, TestError>(n)
            } else {
                Err(TestError("odd"))
            }
        };

        let fut = pipeline_task(rx_in, tx_ok.clone(), tx_err.clone(), chain);

        // Send 0..9 then close input.
        for n in 0..10 {
            tx_in.send(n).await.unwrap();
        }
        drop(tx_in);

        timeout(TEST_TIMEOUT, fut)
            .await
            .expect("pipeline_task_routes_errors_and_successes timed out");

        // Drain oks.
        let mut oks = Vec::new();
        while let Some(v) = rx_ok.recv().await {
            oks.push(v);
        }
        oks.sort();
        assert_eq!(oks, vec![0, 2, 4, 6, 8]);

        // Drain errs.
        let mut errs = Vec::new();
        while let Some(e) = rx_err.recv().await {
            errs.push(e);
        }
        assert_eq!(errs.len(), 5);
        assert!(errs.iter().all(|e| e.0 == "odd"));
    }

    // ─────────────────────────────────────────────
    // 3. Concurrency bound: at most MAX_IN_FLIGHT futures in flight.
    //    We simulate slow work and track max simultaneous in-flight tasks.
    // ─────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_task_respects_max_in_flight_bound() {
        let (tx_in, rx_in) = mpsc::channel::<usize>(64);
        let (tx_ok, mut rx_ok) = mpsc::channel::<()>(64);
        let (tx_err, mut rx_err) = mpsc::channel::<TestError>(64);

        let inflight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let inflight_cloned = inflight.clone();
        let max_seen_cloned = max_seen.clone();

        // Chain: increment `inflight`, sleep a bit, decrement.
        let chain = move |_n: usize| {
            let inflight = inflight_cloned.clone();
            let max_seen = max_seen_cloned.clone();
            async move {
                let cur = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                // update max_seen
                loop {
                    let prev = max_seen.load(Ordering::SeqCst);
                    if cur <= prev {
                        break;
                    }
                    if max_seen
                        .compare_exchange(prev, cur, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        break;
                    }
                }

                // Simulate work
                sleep(Duration::from_millis(20)).await;

                inflight.fetch_sub(1, Ordering::SeqCst);
                Ok::<(), TestError>(())
            }
        };

        let fut = pipeline_task(rx_in, tx_ok.clone(), tx_err.clone(), chain);

        // Enqueue more items than MAX_IN_FLIGHT.
        let total = super::MAX_IN_FLIGHT * 2;
        for n in 0..total {
            tx_in.send(n).await.unwrap();
        }
        drop(tx_in);

        timeout(TEST_TIMEOUT, fut)
            .await
            .expect("pipeline_task_respects_max_in_flight_bound timed out");

        // Drain and ignore oks / errs; we just care that nothing panicked or hung.
        while rx_ok.recv().await.is_some() {}
        assert!(timeout(Duration::from_millis(100), rx_err.recv())
            .await
            .ok()
            .flatten()
            .is_none());

        let max_inflight = max_seen.load(Ordering::SeqCst);
        assert!(
            max_inflight <= super::MAX_IN_FLIGHT,
            "observed max in-flight {} exceeds bound {}",
            max_inflight,
            super::MAX_IN_FLIGHT
        );
        // Should see some parallelism (not strictly required, but nice sanity check).
        assert!(
            max_inflight >= 2,
            "expected at least some concurrency, saw {}",
            max_inflight
        );
    }

    // ─────────────────────────────────────────────
    // 4. Empty input: no items sent, input channel closed.
    //    The task should complete and produce no output.
    // ─────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_task_handles_empty_input() {
        // Create channel, but drop the sender immediately so the receiver
        // sees "closed + empty" from the very beginning.
        let (tx_in, rx_in) = mpsc::channel::<i32>(4);
        drop(tx_in);

        let (tx_ok, mut rx_ok) = mpsc::channel::<()>(4);
        let (tx_err, mut rx_err) = mpsc::channel::<TestError>(4);

        // Chain is never actually called.
        let chain = |_n: i32| async move { Ok::<(), TestError>(()) };

        let fut = pipeline_task(rx_in, tx_ok.clone(), tx_err.clone(), chain);

        let result = timeout(TEST_TIMEOUT, fut).await;
        assert!(
            result.is_ok(),
            "pipeline_task should finish on empty, already-closed input"
        );

        // There should be no ok values.
        let ok = timeout(Duration::from_millis(200), rx_ok.recv())
            .await
            .ok()
            .flatten();
        assert!(
            ok.is_none(),
            "expected no OK outputs for empty input, got: {:?}",
            ok
        );

        // There should be no error values either.
        let err = timeout(Duration::from_millis(200), rx_err.recv())
            .await
            .ok()
            .flatten();
        assert!(
            err.is_none(),
            "expected no Err outputs for empty input, got: {:?}",
            err
        );
    }

    // ─────────────────────────────────────────────
    // 5. Closed output channels: dropping receivers early should not
    //    cause the driver to panic or hang; sends simply fail and are ignored.
    // ─────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_task_survives_closed_output_channels() {
        let (tx_in, rx_in) = mpsc::channel::<i32>(8);
        let (tx_ok, rx_ok) = mpsc::channel::<i32>(1);
        let (tx_err, rx_err) = mpsc::channel::<TestError>(1);

        // Drop receivers immediately so all sends will fail.
        drop(rx_ok);
        drop(rx_err);

        let chain = |n: i32| async move {
            if n >= 0 {
                Ok::<_, TestError>(n)
            } else {
                Err(TestError("negative"))
            }
        };

        let fut = pipeline_task(rx_in, tx_ok.clone(), tx_err.clone(), chain);

        // Send some items (both Ok and Err) then close input.
        for n in -2..=2 {
            tx_in.send(n).await.unwrap();
        }
        drop(tx_in);

        // The pipeline should still complete; send failures are ignored.
        let result = timeout(TEST_TIMEOUT, fut).await;
        assert!(
            result.is_ok(),
            "pipeline_task should not hang when outputs are closed"
        );
    }
}
