//! Regression: a dispatch cancelled mid-flight (e.g. the HTTP client
//! disconnected during a slow/streaming response) must NOT leave the pooled
//! worker wedged in `Active`. The next request has to get a dispatchable worker.

mod helpers;

use std::sync::Arc;
use std::time::Duration;

use edger_core::{
    create_worker_ref, Isolate, IsolationError, SerializedRequest, SerializedResponse,
    WorkerConfig, WorkerRef,
};
use edger_worker::IsolateFactory;
use helpers::MockIsolateFactory;
use helpers::{default_pool_config, pool_with_factory, serialized_get, temp_worker_dir};

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

// EDG-6 (metrics amendment): a dispatch that obtained a slot and was dropped
// before a known result counts EXACTLY ONCE as `cancelled` on the group
// (ok=0, error=0) — the `DispatchCancelGuard` cancel path. Mutation
// captured: removing the `cancelled` count from the guard's `Drop` leaves
// `requests_cancelled_total` at 0 and this test goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_dispatch_counts_once_as_cancelled_on_the_group() {
    let pool = pool_with_factory(
        Arc::new(MockIsolateFactory {
            slow_fetch_ms: 2000,
            spa_html: None,
        }),
        default_pool_config(),
    );
    let (dir, _config, manifest) = temp_worker_dir("name: cancel-metrics\nttl: 60\n");
    let worker_ref = create_worker_ref(dir.path().to_path_buf(), manifest).expect("valid manifest");

    // The future is dropped while the isolate is still working — the
    // `DispatchCancelGuard` cancel path, exactly like a client hang-up.
    let cancelled = tokio::time::timeout(
        Duration::from_millis(200),
        pool.fetch_worker(&worker_ref, serialized_get("/cancel"), None),
    )
    .await;
    assert!(cancelled.is_err(), "the dispatch must be cancelled");

    let groups = pool.get_metrics().worker_groups;
    let group = groups
        .iter()
        .find(|group| group.name == "cancel-metrics")
        .unwrap_or_else(|| panic!("worker group missing from the metrics snapshot"));
    assert_eq!(
        group.requests_cancelled_total, 1,
        "one cancelled dispatch counted exactly once on the group"
    );
    assert_eq!(
        group.requests_ok_total, 0,
        "no ok counted for the cancelled dispatch"
    );
    assert_eq!(
        group.requests_error_total, 0,
        "no error counted for the cancelled dispatch"
    );
}

// Re-review P2 #2: a known `ok` result must be recorded BEFORE the
// supervisor cleanup await. Here the dispatch future is dropped while
// `on_request_complete` is still finishing the `max_requests` retirement
// (the mock's `terminate` is slow) — the known result must count as `ok`
// (never `cancelled`) and the instance must still follow its cleanup path
// (recycled by the guard's Drop protection, not wedged).
struct SlowTerminateIsolate;

#[async_trait::async_trait]
impl Isolate for SlowTerminateIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(bytes::Bytes::from_static(b"ok")),
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new("NOT_IMPLEMENTED", "spa"))
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new("NOT_IMPLEMENTED", "wasm"))
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        // Slow retirement: the dispatch future is deterministically dropped
        // inside `on_request_complete` -> retire_for_max_requests ->
        // terminate while the result is already known.
        tokio::time::sleep(Duration::from_millis(2000)).await;
        Ok(())
    }
}

struct SlowTerminateFactory;

impl IsolateFactory for SlowTerminateFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(SlowTerminateIsolate)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drop_during_on_request_complete_counts_ok_not_cancelled() {
    let pool = pool_with_factory(Arc::new(SlowTerminateFactory), default_pool_config());
    // maxRequests: 1 => the FIRST request's on_request_complete retires the
    // instance (slow terminate in the mock) => a deterministic point where
    // the dispatch future can be dropped AFTER the isolate already answered.
    // (The manifest is camelCase, hence `maxRequests`.)
    let (dir, _config, manifest) =
        temp_worker_dir("name: cancel-cleanup\nmaxRequests: 1\nttl: 60\n");
    let worker_ref = create_worker_ref(dir.path().to_path_buf(), manifest).expect("valid manifest");

    // The isolate answers immediately; the future is dropped while
    // on_request_complete is still finishing the max_requests retirement.
    let dropped = tokio::time::timeout(
        Duration::from_millis(500),
        pool.fetch_worker(&worker_ref, serialized_get("/cleanup"), None),
    )
    .await;
    assert!(
        dropped.is_err(),
        "the future must be dropped during the cleanup await"
    );

    let groups = pool.get_metrics().worker_groups;
    let group = groups
        .iter()
        .find(|group| group.name == "cancel-cleanup")
        .unwrap_or_else(|| panic!("worker group missing from the metrics snapshot"));
    assert_eq!(
        group.requests_ok_total, 1,
        "the known result counted as ok before the cleanup await"
    );
    assert_eq!(
        group.requests_cancelled_total, 0,
        "a drop during on_request_complete must not reclassify a known ok"
    );
    assert_eq!(group.requests_error_total, 0, "no error counted");

    // The instance followed the correct cleanup path: the guard's Drop
    // protection recycled it (not wedged, not left in the pool).
    for _ in 0..1_000 {
        if pool.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(pool.is_empty(), "the recycled instance must leave the pool");
}
