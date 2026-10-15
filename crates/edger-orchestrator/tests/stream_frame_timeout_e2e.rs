//! EDG-17: the per-frame read timeout cuts a stalled streamed body through
//! the SAME cancel/drain path as the EDG-16 max-duration cut (origin
//! FrameTimeout), reusing the process on a clean end. Requires deno on PATH;
//! ignored by default.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, StreamDetachBudget, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{
    AbandonDrainLimits, IsolateFactory, PoolConfig, WorkerLifecycleEvent, WorkerLifecycleEventKind,
    WorkerPool,
};
use futures_util::StreamExt;
use tower::ServiceExt;

static FRAME_TIMEOUT_WORKER_TIMEOUT_MS: u64 = 2_000;

#[derive(Clone)]
struct FrameTimeoutFactory {
    budget: Arc<StreamDetachBudget>,
    drain_max_bytes: u64,
    drain_max_ms: u64,
    /// EDG-32: per-worker Deno cache root inside the test's own tempdir.
    deno_cache_root: std::path::PathBuf,
}

impl FrameTimeoutFactory {
    fn new() -> Self {
        Self {
            budget: Arc::new(StreamDetachBudget::new(8 * 1024 * 1024)),
            drain_max_bytes: 8 * 1024 * 1024,
            drain_max_ms: 2_000,
            // The `state_with_lifecycle` helper always replaces this with the
            // test's own tempdir root before the pool is built.
            deno_cache_root: std::env::temp_dir().join("edger-test-deno-cache"),
        }
    }
}

impl IsolateFactory for FrameTimeoutFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(
                DenoProcessIsolate::new()
                    .with_per_worker_deno_cache_root(self.deno_cache_root.clone())
                    .with_stream_detach(8 * 1024 * 1024, Arc::clone(&self.budget))
                    .with_abandon_drain_limits(self.drain_max_bytes, self.drain_max_ms)
                    // A generous process default: the manifest `streamTimeout: 0`
                    // disables the max-duration cut for the worker under test.
                    .with_stream_max_duration_default_ms(300_000),
            ),
        }
    }
}

fn state_with_lifecycle(
    root: std::path::PathBuf,
    factory: FrameTimeoutFactory,
) -> (
    OrchestratorState,
    tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
) {
    let server = ServerState::new_unready();
    // EDG-32: keep the per-worker Deno cache inside the test's own tempdir.
    let factory = FrameTimeoutFactory {
        deno_cache_root: root.join(".edger-test-deno-cache"),
        ..factory
    };
    server.set_stream_detach_budget(Arc::clone(&factory.budget));
    let (lifecycle_tx, lifecycle_rx) = tokio::sync::mpsc::channel(64);
    let pool = WorkerPool::with_factory_and_lifecycle_abandon_drain(
        PoolConfig::default(),
        Arc::new(factory.clone()),
        Some(lifecycle_tx),
        AbandonDrainLimits {
            max_bytes: factory.drain_max_bytes,
            max_ms: factory.drain_max_ms,
        },
    );
    server.mark_ready(pool.clone());
    (
        OrchestratorState {
            server,
            pool,
            index: load_manifests_from_dirs(&[root]).unwrap(),
            auth: ControlAuth::with_static_key("test-root"),
        },
        lifecycle_rx,
    )
}

/// An SSE worker that sends ONE event and then stops producing (no further
/// events, no close) — the stall that trips the per-frame read timeout.
fn write_stall_worker(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\ntimeout: 2s\nstreamTimeout: 0\nmaxProcesses: 1\nminProcesses: 1\nqueueTimeout: 1s\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"let seq = 0;
Deno.serve((req) => {
  const url = new URL(req.url);
  if (url.pathname === "/seq") {
    seq += 1;
    return new Response("sequence", { headers: { "x-seq": String(seq) } });
  }
  seq += 1;
  const enc = new TextEncoder();
  const stream = new ReadableStream({
    start(controller) {
      controller.enqueue(enc.encode("data: hello\n\n"));
      // Then stop producing: the next frame read on the Rust side
      // times out and cuts the stream (EDG-17).
    },
  });
  return new Response(stream, {
    headers: { "content-type": "text/event-stream", "x-seq": String(seq) },
  });
});
"#,
    )
    .unwrap();
}

async fn send_with_id(app: &Router, uri: &str, request_id: &str) -> axum::http::Response<Body> {
    tokio::time::timeout(
        Duration::from_secs(30),
        app.clone().oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .header("x-request-id", request_id)
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("request headers arrive, including a cold start")
    .expect("request succeeds")
}

async fn metrics_text(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    String::from_utf8(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

async fn stats_json(app: &Router) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics/stats")
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn worker_stats<'a>(stats: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    stats["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|worker| worker["name"] == name)
        .unwrap_or_else(|| panic!("worker {name} missing from /metrics/stats: {stats}"))
}

async fn wait_for_active_request_null(app: &Router, worker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let stats = stats_json(app).await;
        if worker_stats(&stats, worker)["processes"][0]["activeRequest"].is_null() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "activeRequest did not clear: {stats}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn frame_timeout_event_for(
    rx: &mut tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
    request_id: &str,
    timeout: Duration,
) -> Option<WorkerLifecycleEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        while let Ok(event) = rx.try_recv() {
            if event.kind == WorkerLifecycleEventKind::StreamFrameTimeout
                && event.request_id.as_deref() == Some(request_id)
            {
                return Some(event);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn frame_timeout_cancels_drains_and_reuses_the_same_process() {
    let root = tempfile::tempdir().unwrap();
    write_stall_worker(root.path(), "frame-stall");
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), FrameTimeoutFactory::new());
    let app = build_pipeline(state.clone());

    // The worker's per-frame read timeout is 2 s (manifest `timeout: 2s`);
    // the max-duration cut is disabled (`streamTimeout: 0`).
    let request_id = "edg17-frame-stall";
    let started = Instant::now();
    let response = send_with_id(&app, "/frame-stall", request_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-seq").unwrap(), "1");
    let mut body = response.into_body().into_data_stream();

    // The first event arrives; then the worker stalls and the per-frame
    // timeout cuts the stream. The body must end CLEANLY (no error item)
    // after the chunk already queued.
    let first = tokio::time::timeout(Duration::from_secs(30), body.next())
        .await
        .expect("the first event arrives")
        .expect("stream stays open")
        .expect("first event is valid");
    assert!(
        String::from_utf8_lossy(&first).contains("data: hello"),
        "the worker's single event reaches the client"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = body.next().await {
            chunk.expect("the frame-timeout cut ends the body cleanly");
        }
    })
    .await
    .expect("the stream ends before ten seconds");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(FRAME_TIMEOUT_WORKER_TIMEOUT_MS),
        "cut too early: {elapsed:?}"
    );
    wait_for_active_request_null(&app, "frame-stall").await;

    // The cut keeps the stream identity: request id, the process id shown in
    // /metrics/stats, the per-frame timeout and the drain outcome.
    let event = frame_timeout_event_for(&mut lifecycle_rx, request_id, Duration::from_secs(5))
        .await
        .expect("pool lifecycle publishes the frame-timeout cut");
    assert_eq!(event.detail, Some("cancelled"));
    assert_eq!(
        event.timeout_ms,
        Some(FRAME_TIMEOUT_WORKER_TIMEOUT_MS),
        "the event carries the per-frame timeout"
    );
    assert!(
        event
            .duration_ms
            .is_some_and(|duration| (2_000..10_000).contains(&duration)),
        "event duration must reflect the stream age at cutoff: {:?}",
        event.duration_ms
    );
    let instance_id = event.process_id.clone().expect("process id on the event");

    // The counter family: only edger_stream_frame_timeout_total moves — the
    // client-abandonment and max-duration families stay untouched.
    let metric = metrics_text(&app).await;
    assert!(
        metric.contains("edger_stream_frame_timeout_total{outcome=\"cancelled\"} 1"),
        "frame-timeout metric missing from /metrics:\n{metric}"
    );
    assert!(
        metric.contains("edger_stream_abandoned_total{outcome=\"cancelled\"} 0"),
        "abandoned counter changed for the frame-timeout cut:\n{metric}"
    );
    assert!(
        metric.contains("edger_stream_max_duration_total{outcome=\"cancelled\"} 0"),
        "max-duration counter changed for the frame-timeout cut:\n{metric}"
    );

    // The NEXT request is served by the SAME process: x-seq increments and
    // the stats process id is unchanged (no cold start).
    let after_stats = stats_json(&app).await;
    assert_eq!(
        worker_stats(&after_stats, "frame-stall")["processes"][0]["id"],
        instance_id,
        "the drain restored the socket: the process is unchanged"
    );
    let next = send_with_id(&app, "/frame-stall/seq", "edg17-frame-stall-next").await;
    assert_eq!(next.headers().get("x-seq").unwrap(), "2");
}
