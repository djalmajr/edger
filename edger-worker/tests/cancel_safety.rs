//! Regression: a dispatch cancelled mid-flight (e.g. the HTTP client
//! disconnected during a slow/streaming response) must NOT leave the pooled
//! worker wedged in `Active`. The next request has to get a dispatchable worker.

mod helpers;

use std::sync::Arc;
use std::time::Duration;

use helpers::{default_pool_config, pool_with_factory, serialized_get, temp_worker_dir};
use helpers::MockIsolateFactory;

// Mutation captured: removing the `DispatchCancelGuard` (or its Drop body) from
// `WorkerPool` leaves the cancelled instance stuck `Active`; the second dispatch
// then exhausts the resolve retries and fails with `worker not ready for
// dispatch`, so this test goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_dispatch_recycles_worker_instead_of_wedging_it() {
    let pool = pool_with_factory(
        Arc::new(MockIsolateFactory {
            slow_fetch_ms: 2000,
            spa_html: None,
        }),
        default_pool_config(),
    );
    let (dir, config, _manifest) = temp_worker_dir("name: slow\nttl: 60\n");

    // First dispatch is cancelled by the timeout while the isolate is still
    // working — the future is dropped mid-flight, exactly like a client hang-up.
    let cancelled = tokio::time::timeout(
        Duration::from_millis(200),
        pool.fetch(dir.path(), &config, serialized_get("/slow"), None),
    )
    .await;
    assert!(cancelled.is_err(), "first dispatch must be cancelled");

    // The wedged-worker symptom would be a NotReady error (or a hang) here.
    let second = tokio::time::timeout(
        Duration::from_secs(6),
        pool.fetch(dir.path(), &config, serialized_get("/slow"), None),
    )
    .await
    .expect("second dispatch must not hang on a wedged worker")
    .expect("second dispatch must get a fresh, dispatchable worker");
    assert_eq!(second.status, 200);
}
