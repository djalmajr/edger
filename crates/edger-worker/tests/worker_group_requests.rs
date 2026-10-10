//! EDG-6 (metrics): the per-group request counters are incremented once per
//! dispatch that obtained a process slot, by worker-side result
//! (`ok` = the worker returned a response with any HTTP status; `error` = a
//! worker/isolate error; `cancelled` = the dispatch future was dropped before
//! a known result), and they live on the worker IDENTITY for the life of the
//! edger process: they must survive instance recycling and LRU
//! eviction/readmission (only a process restart resets them). Queue
//! rejections and queue timeouts are not counted here, and neither are
//! synthetic health checks (`x-edger-health-check`) — on both the buffered
//! and the streaming dispatch paths.

mod helpers;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    create_worker_ref, ExecutionKind, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, WorkerConfig, WorkerRef, WorkerResponse,
};
use edger_worker::{IsolateFactory, WorkerPool};
use futures_util::StreamExt;
use helpers::{default_pool_config, pool_with_factory, serialized_get, temp_worker_dir};

/// The (ok, error, cancelled) counters of the group named `name`, as exposed
/// through the pool's metrics snapshot.
fn group_requests(pool: &WorkerPool, name: &str) -> (u64, u64, u64) {
    let groups = pool.get_metrics().worker_groups;
    let group = groups
        .iter()
        .find(|group| group.name == name)
        .unwrap_or_else(|| panic!("worker group {name:?} missing from the metrics snapshot"));
    (
        group.requests_ok_total,
        group.requests_error_total,
        group.requests_cancelled_total,
    )
}

// A dispatch that the worker answers (any HTTP status) counts once as `ok`
// on the group.
#[tokio::test]
async fn dispatch_ok_increments_the_group_ok_counter() {
    let pool = pool_with_factory(
        Arc::new(helpers::MockIsolateFactory::default()),
        default_pool_config(),
    );
    let (dir, _config, manifest) = temp_worker_dir("name: metrics-ok\nttl: 60\n");
    let worker_ref = create_worker_ref(dir.path().to_path_buf(), manifest).expect("valid manifest");

    let res = pool
        .fetch_worker(&worker_ref, serialized_get("/metrics-ok"), None)
        .await
        .expect("the mock worker answers");
    assert_eq!(res.status, 200);

    let (ok, error, cancelled) = group_requests(&pool, "metrics-ok");
    assert_eq!(ok, 1, "one dispatched request counted as ok");
    assert_eq!(error, 0, "no error counted");
    assert_eq!(cancelled, 0, "no cancellation counted");
}

// The isolate error path counts once as `error` on the group, and the
// counter SURVIVES the instance recycle: the failed instance is evicted
// (cache empty) but the group counter is preserved, and the fresh instance
// accumulates on the SAME group.
struct FlakyIsolate {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Isolate for FlakyIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(IsolationError::new("EXEC_FAILED", "boom on first call"));
        }
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(bytes::Bytes::from_static(b"recovered")),
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
        Ok(())
    }
}

struct FlakyFactory {
    calls: Arc<AtomicUsize>,
}

impl IsolateFactory for FlakyFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(FlakyIsolate {
            calls: Arc::clone(&self.calls),
        })
    }
}

#[tokio::test]
async fn isolate_error_counts_as_error_and_the_counter_survives_the_recycle() {
    let calls = Arc::new(AtomicUsize::new(0));
    let pool = pool_with_factory(
        Arc::new(FlakyFactory {
            calls: Arc::clone(&calls),
        }),
        default_pool_config(),
    );
    let (dir, _config, manifest) = temp_worker_dir("name: metrics-err\nttl: 60\n");
    let worker_ref = create_worker_ref(dir.path().to_path_buf(), manifest).expect("valid manifest");

    let first = pool
        .fetch_worker(&worker_ref, serialized_get("/metrics-err"), None)
        .await;
    assert!(
        first.is_err(),
        "first dispatch must surface the isolate failure"
    );

    // The failed instance is evicted (recycled) ...
    assert!(pool.is_empty(), "failed worker must leave the cache");
    // ... but the group-level error counter survives the recycle.
    let (ok, error, cancelled) = group_requests(&pool, "metrics-err");
    assert_eq!(ok, 0, "no ok counted yet");
    assert_eq!(error, 1, "the isolate error counted once on the group");
    assert_eq!(cancelled, 0, "no cancellation counted");

    // A fresh instance serves the next dispatch and accumulates on the SAME
    // group: the instance's own count restarted at zero, the group's did not.
    let second = pool
        .fetch_worker(&worker_ref, serialized_get("/metrics-err"), None)
        .await
        .expect("second dispatch must get a fresh worker");
    assert_eq!(second.status, 200);

    let (ok, error, cancelled) = group_requests(&pool, "metrics-err");
    assert_eq!(
        ok, 1,
        "the fresh instance's dispatch accumulated on the group"
    );
    assert_eq!(error, 1, "the recycled instance's error is still counted");
    assert_eq!(cancelled, 0, "no cancellation counted");
}

fn health_check_request(path: &str) -> SerializedRequest {
    let mut req = serialized_get(path);
    req.headers
        .push(("x-edger-health-check".into(), "1".into()));
    req
}

// Synthetic health checks (`x-edger-health-check`) are excluded from the
// group counters on BOTH dispatch paths — the admin's health traffic is not
// real demand. Control: the SAME request without the header counts once.
#[tokio::test]
async fn health_checks_are_excluded_on_the_buffered_path() {
    let pool = pool_with_factory(
        Arc::new(helpers::MockIsolateFactory::default()),
        default_pool_config(),
    );
    let (dir, _config, manifest) = temp_worker_dir("name: metrics-hc-b\nttl: 60\n");
    let worker_ref = create_worker_ref(dir.path().to_path_buf(), manifest).expect("valid manifest");

    // With the real health-check header: the dispatch still obtains a slot
    // and runs the worker, but the counters stay at zero.
    let res = pool
        .fetch_worker(&worker_ref, health_check_request("/hc"), None)
        .await
        .expect("the health check reaches the worker");
    assert_eq!(res.status, 200);
    let (ok, error, cancelled) = group_requests(&pool, "metrics-hc-b");
    assert_eq!(ok, 0, "a synthetic health check is not counted as ok");
    assert_eq!(error, 0, "a synthetic health check is not counted as error");
    assert_eq!(
        cancelled, 0,
        "a synthetic health check is not counted as cancelled"
    );

    // Control without the header: the same dispatch counts once as ok.
    let res = pool
        .fetch_worker(&worker_ref, serialized_get("/hc"), None)
        .await
        .expect("the mock worker answers");
    assert_eq!(res.status, 200);
    let (ok, error, cancelled) = group_requests(&pool, "metrics-hc-b");
    assert_eq!(ok, 1, "without the header the dispatch counts once");
    assert_eq!(error, 0);
    assert_eq!(cancelled, 0);
}

#[tokio::test]
async fn health_checks_are_excluded_on_the_streaming_path() {
    let pool = pool_with_factory(
        Arc::new(helpers::MockIsolateFactory::default()),
        default_pool_config(),
    );
    let (dir, _config, manifest) = temp_worker_dir("name: metrics-hc-s\nttl: 60\n");
    let worker_ref = create_worker_ref(dir.path().to_path_buf(), manifest).expect("valid manifest");

    // Streaming entry, WITH the header: the mock isolate buffers (its
    // default `execute_fetch_stream`), but the streaming dispatch path must
    // still skip the counters for synthetic health checks.
    let res = pool
        .fetch_worker_stream(
            &worker_ref,
            health_check_request("/hc"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .expect("the health check reaches the worker through the streaming entry");
    assert!(matches!(res, WorkerResponse::Buffered(_)));
    let (ok, error, cancelled) = group_requests(&pool, "metrics-hc-s");
    assert_eq!(
        ok, 0,
        "a synthetic health check through the streaming entry is not counted"
    );
    assert_eq!(error, 0);
    assert_eq!(cancelled, 0);

    // Control without the header: counted once on the same group.
    let res = pool
        .fetch_worker_stream(
            &worker_ref,
            serialized_get("/hc"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .expect("the mock worker answers through the streaming entry");
    assert!(matches!(res, WorkerResponse::Buffered(_)));
    let (ok, error, cancelled) = group_requests(&pool, "metrics-hc-s");
    assert_eq!(
        ok, 1,
        "without the header the streaming dispatch counts once"
    );
    assert_eq!(error, 0);
    assert_eq!(cancelled, 0);
}

// Re-review P2 #1: the health-check exclusion must travel WITH the body to
// the stream terminals. A worker whose isolate really STREAMS (returns
// `WorkerResponse::Streamed`) must count nothing with the header — on a
// clean end AND on a mid-stream error — and exactly once without it (ok on
// the clean end, error on the error/drop path).

/// Body: `chunks` then either `None` (clean end) or the given `stream_error`
/// (mid-stream error). Always ready; no Pending.
struct ScriptedBody {
    chunks: std::collections::VecDeque<Bytes>,
    stream_error: Option<String>,
}

impl futures_core::Stream for ScriptedBody {
    type Item = Result<Bytes, IsolationError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.chunks.pop_front() {
            Some(chunk) => std::task::Poll::Ready(Some(Ok(chunk))),
            None => match self.stream_error.take() {
                Some(code) => std::task::Poll::Ready(Some(Err(IsolationError::new(
                    &code,
                    "body failed mid-stream",
                )))),
                None => std::task::Poll::Ready(None),
            },
        }
    }
}

struct StreamedIsolate {
    fail_mid_stream: bool,
}

#[async_trait]
impl Isolate for StreamedIsolate {
    async fn execute_fetch_stream(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        // Pre-EDG-9 style backend: no completion signal / production-complete
        // flag — the end of the body (or its error/drop) is the lifecycle
        // trigger, so the pool's stream terminals decide ok vs error.
        Ok(WorkerResponse::Streamed(edger_core::StreamedResponse {
            status: 200,
            headers: vec![],
            body: Box::pin(ScriptedBody {
                chunks: std::collections::VecDeque::from([
                    Bytes::from_static(b"chunk-1"),
                    Bytes::from_static(b"chunk-2"),
                ]),
                stream_error: self
                    .fail_mid_stream
                    .then_some("BODY_STREAM_ERROR".to_string()),
            }),
            completed: None,
            production_complete: None,
            max_duration_elapsed_ms: None,
        }))
    }

    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch_stream(req, config)
            .await
            .map(|response| match response {
                WorkerResponse::Buffered(res) => res,
                WorkerResponse::Streamed(streamed) => SerializedResponse {
                    status: streamed.status,
                    headers: streamed.headers,
                    body: None,
                },
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
        Ok(())
    }
}

struct StreamedFactory {
    fail_mid_stream: bool,
}

impl IsolateFactory for StreamedFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(StreamedIsolate {
            fail_mid_stream: self.fail_mid_stream,
        })
    }
}

/// Drive a streamed body to its terminal state (clean end or mid-stream
/// error), consuming the chunks.
async fn drain_streamed_body(response: WorkerResponse) {
    let WorkerResponse::Streamed(streamed) = response else {
        panic!("the mock isolate must return a Streamed response");
    };
    let mut body = streamed.body;
    while let Some(result) = body.next().await {
        // Mid-stream error ends the loop: the pool's drop/error path has
        // already decided the lifecycle (recycle).
        let _ = result;
    }
}

async fn wait_for_group(pool: &WorkerPool, name: &str, expected: (u64, u64, u64)) {
    for _ in 0..1_000 {
        let current = group_requests(pool, name);
        if current == expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!(
        "group {name:?} counters did not settle to {expected:?}: {:?}",
        group_requests(pool, name)
    );
}

// Clean end through a REAL stream: the header keeps every counter at zero;
// without it the same clean end counts exactly once as `ok`.
#[tokio::test]
async fn streamed_health_check_is_excluded_on_a_clean_end() {
    let pool = pool_with_factory(
        Arc::new(StreamedFactory {
            fail_mid_stream: false,
        }),
        default_pool_config(),
    );
    let (dir, _config, manifest) = temp_worker_dir("name: metrics-hc-stream\nttl: 60\n");
    let worker_ref = create_worker_ref(dir.path().to_path_buf(), manifest).expect("valid manifest");

    // WITH the header: the body is really streamed to the end, but the group
    // counters must stay at zero.
    let res = pool
        .fetch_worker_stream(
            &worker_ref,
            health_check_request("/hc"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .expect("the health check reaches the streaming worker");
    drain_streamed_body(res).await;
    wait_for_group(&pool, "metrics-hc-stream", (0, 0, 0)).await;
    let (ok, error, cancelled) = group_requests(&pool, "metrics-hc-stream");
    assert_eq!(
        (ok, error, cancelled),
        (0, 0, 0),
        "a streamed health check counts nothing even on a clean end"
    );

    // Control WITHOUT the header: the same clean end counts once as `ok`.
    let res = pool
        .fetch_worker_stream(
            &worker_ref,
            serialized_get("/hc"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .expect("the streaming worker answers");
    drain_streamed_body(res).await;
    wait_for_group(&pool, "metrics-hc-stream", (1, 0, 0)).await;
}

// Mid-stream error through a REAL stream: the header keeps every counter at
// zero; without it the same error counts exactly once as `error` (the
// recycle path), never as `ok` or `cancelled`.
#[tokio::test]
async fn streamed_health_check_is_excluded_on_a_mid_stream_error() {
    let pool = pool_with_factory(
        Arc::new(StreamedFactory {
            fail_mid_stream: true,
        }),
        default_pool_config(),
    );
    let (dir, _config, manifest) = temp_worker_dir("name: metrics-hc-stream-err\nttl: 60\n");
    let worker_ref = create_worker_ref(dir.path().to_path_buf(), manifest).expect("valid manifest");

    // WITH the header: the body errors mid-stream (recycle), counters stay 0.
    let res = pool
        .fetch_worker_stream(
            &worker_ref,
            health_check_request("/err"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .expect("the health check reaches the streaming worker");
    drain_streamed_body(res).await;
    wait_for_group(&pool, "metrics-hc-stream-err", (0, 0, 0)).await;
    let (ok, error, cancelled) = group_requests(&pool, "metrics-hc-stream-err");
    assert_eq!(
        (ok, error, cancelled),
        (0, 0, 0),
        "a streamed health check counts nothing even on a mid-stream error"
    );

    // Control WITHOUT the header: the same error counts once as `error`
    // (the failed instance was recycled; a fresh one serves this dispatch).
    let res = pool
        .fetch_worker_stream(
            &worker_ref,
            serialized_get("/err"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .expect("the streaming worker accepts the retry");
    drain_streamed_body(res).await;
    wait_for_group(&pool, "metrics-hc-stream-err", (0, 1, 0)).await;
}
