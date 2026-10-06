//! EDG-6 (metrics): the per-group dispatch counters and the process-wide
//! stream-detach counters must reach `/metrics` and `/metrics/stats`:
//!
//! - 3 requests to a worker ->
//!   `edger_worker_requests_total{...,outcome="ok"} 3` (and the group
//!   `requestsTotal` in `/metrics/stats`);
//! - a streamed body ABANDONED mid-production whose abandon drain finishes
//!   cleanly -> `edger_stream_abandoned_total{outcome="drained"} >= 1`.
//!
//! Requires `deno` on PATH. Ignored by default; run explicitly.

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
    IsolateFactory, PoolConfig, WorkerLifecycleEvent, WorkerLifecycleEventKind, WorkerPool,
};
use futures_util::StreamExt;
use tower::ServiceExt;

/// 16 KiB chunks: small enough to keep the corpus in the kilobytes.
const CHUNK_BYTES: usize = 16 * 1024;

/// Factory whose persistent-process isolates use a detach pipeline AND the
/// EDG-9 abandon drain (generous limits so an early disconnect drains
/// cleanly -> the `drained` outcome).
#[derive(Clone)]
struct MetricsFactory {
    detach_max_bytes: u64,
    budget: Arc<StreamDetachBudget>,
    drain_max_bytes: u64,
    drain_max_ms: u64,
}

impl MetricsFactory {
    fn new() -> Self {
        Self {
            // Far above the response sizes here: no backpressure.
            detach_max_bytes: 8 * 1024 * 1024,
            budget: Arc::new(StreamDetachBudget::new(1024 * 1024)),
            drain_max_bytes: 512 * 1024,
            drain_max_ms: 2_000,
        }
    }
}

impl IsolateFactory for MetricsFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(
                DenoProcessIsolate::new()
                    .with_stream_detach(self.detach_max_bytes, Arc::clone(&self.budget))
                    .with_abandon_drain_limits(self.drain_max_bytes, self.drain_max_ms),
            ),
        }
    }
}

/// Pipeline + pool wired exactly like the multiproc backend of the `edger`
/// binary: the shared detach budget is registered on the server state (the
/// `/metrics` block) and the pool's completion wait mirrors the isolates'
/// drain limits. The lifecycle receiver lets the test await the real
/// `DrainCompleted` event before asserting on the counters.
fn state(
    root: std::path::PathBuf,
    factory: MetricsFactory,
) -> (
    OrchestratorState,
    Arc<StreamDetachBudget>,
    tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
) {
    let server = ServerState::new_unready();
    let budget = Arc::clone(&factory.budget);
    server.set_stream_detach_budget(budget.clone());
    let (lifecycle_tx, lifecycle_rx) = tokio::sync::mpsc::channel(64);
    let drain_max_bytes = factory.drain_max_bytes;
    let drain_max_ms = factory.drain_max_ms;
    let pool = WorkerPool::with_factory_and_lifecycle_abandon_drain(
        PoolConfig::default(),
        Arc::new(factory),
        Some(lifecycle_tx),
        edger_worker::AbandonDrainLimits {
            max_bytes: drain_max_bytes,
            max_ms: drain_max_ms,
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
        budget,
        lifecycle_rx,
    )
}

/// A buffered fetch worker: every request is a complete 200 response.
fn write_buffered_worker(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmax_processes: 1\nqueue_timeout: 1s\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"Deno.serve(() => new Response("hello-metrics", {
  headers: { "content-type": "text/plain" },
}));
"#,
    )
    .unwrap();
}

/// Streaming worker: `chunks` x 16 KiB with a `pause_ms` gap between chunks
/// (the stream does NOT close in the meantime), so an early disconnect
/// happens MID-production. 8 x 100 ms ~= 800 ms of production.
fn write_stream_worker(root: &std::path::Path, name: &str, chunks: usize, pause_ms: u64) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmax_processes: 1\nqueue_timeout: 1s\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        format!(
            r#"let seq = 0;
const CHUNKS = {chunks};
const CHUNK_BYTES = {chunk_bytes};
const PAUSE_MS = {pause_ms};
Deno.serve(() => {{
  seq += 1;
  const stream = new ReadableStream({{
    async start(c) {{
      for (let i = 0; i < CHUNKS; i++) {{
        const chunk = new Uint8Array(CHUNK_BYTES).fill(0x78);
        chunk[0] = i;
        c.enqueue(chunk);
        if (PAUSE_MS > 0) await new Promise((r) => setTimeout(r, PAUSE_MS));
      }}
      c.close();
    }},
  }});
  return new Response(stream, {{
    headers: {{ "content-type": "text/plain", "x-seq": String(seq) }},
  }});
}});
"#,
            chunks = chunks,
            chunk_bytes = CHUNK_BYTES,
            pause_ms = pause_ms,
        ),
    )
    .unwrap();
}

async fn send(app: Router, uri: &str) -> axum::http::Response<Body> {
    tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("the request completes within 30s (cold start included)")
    .expect("request ok")
}

/// The numeric value of the FIRST `/metrics` line starting with `prefix`
/// (renderer layout: `name{labels} value`), or `None` when the line is
/// absent.
fn metric_value(body: &str, prefix: &str) -> Option<u64> {
    body.lines().find_map(|line| {
        line.strip_prefix(prefix)
            .and_then(|rest| rest.trim_start().parse::<u64>().ok())
    })
}

/// Return the first lifecycle event of `kind` within `ms`, skipping the
/// others (DrainStarted, ...).
async fn lifecycle_event_of(
    rx: &mut tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
    kind: WorkerLifecycleEventKind,
    ms: u64,
) -> Option<WorkerLifecycleEvent> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if let Ok(event) = rx.try_recv() {
            if event.kind == kind {
                return Some(event);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// 3 requests to one worker -> `edger_worker_requests_total{...,outcome="ok"} 3`
// on /metrics and `requestsTotal: 3` on /metrics/stats (the group-level
// counter sits next to the existing group counters).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn three_requests_count_as_ok_in_prometheus_and_stats() {
    let root = tempfile::tempdir().unwrap();
    write_buffered_worker(root.path(), "metrics-app");
    let factory = MetricsFactory::new();
    let (state, _budget, _lifecycle_rx) = state(root.path().to_path_buf(), factory);
    let app = build_pipeline(state);

    for _ in 0..3 {
        let res = send(app.clone(), "/metrics-app").await;
        assert_eq!(res.status(), StatusCode::OK);
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
    }

    let metrics_body = send(app.clone(), "/metrics").await;
    let metrics_bytes = axum::body::to_bytes(metrics_body.into_body(), usize::MAX)
        .await
        .unwrap();
    let metrics = String::from_utf8(metrics_bytes.to_vec()).unwrap();

    assert_eq!(
        metric_value(
            &metrics,
            "edger_worker_requests_total{worker=\"metrics-app\",version=\"1.0.0\",namespace=\"\",outcome=\"ok\"} "
        ),
        Some(3),
        "three dispatched requests counted as ok; /metrics excerpt:\n{metrics}"
    );
    assert_eq!(
        metric_value(
            &metrics,
            "edger_worker_requests_total{worker=\"metrics-app\",version=\"1.0.0\",namespace=\"\",outcome=\"error\"} "
        ),
        Some(0),
        "no worker/isolate error happened; /metrics excerpt:\n{metrics}"
    );

    // The group JSON on /metrics/stats carries the same total (ok + error).
    let stats_body = send(app.clone(), "/metrics/stats").await;
    let stats_bytes = axum::body::to_bytes(stats_body.into_body(), usize::MAX)
        .await
        .unwrap();
    let stats: serde_json::Value = serde_json::from_slice(&stats_bytes).unwrap();
    let group = stats
        .pointer("/workers")
        .and_then(|workers| workers.as_array())
        .and_then(|workers| {
            workers.iter().find(|worker| {
                worker.get("name").and_then(|name| name.as_str()) == Some("metrics-app")
            })
        })
        .expect("the metrics-app group is present in /metrics/stats");
    assert_eq!(
        group.get("requestsTotal").and_then(|value| value.as_u64()),
        Some(3),
        "requestsTotal next to the existing group counters: {group}"
    );
    assert_eq!(
        group.get("requestTotal").and_then(|value| value.as_u64()),
        Some(3),
        "the existing requestTotal is untouched: {group}"
    );
}

// An abandoned stream body mid-production: the drain sends the harness a
// cancel frame (EDG-9 slice 2), the harness ends with `E {"cancelled":true}`
// and the process is reused -> `edger_stream_abandoned_total{outcome="cancelled"} >= 1`
// (process-wide counter, no worker label) and the abandoned dispatch counts
// exactly one `ok` for the worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn abandoned_stream_cancel_counts_in_the_process_wide_counter() {
    let root = tempfile::tempdir().unwrap();
    write_stream_worker(root.path(), "drain-metrics-app", 8, 100);
    let factory = MetricsFactory::new();
    let (state, _budget, mut lifecycle_rx) = state(root.path().to_path_buf(), factory);
    let app = build_pipeline(state);

    let res = send(app.clone(), "/drain-metrics-app").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("x-seq").unwrap(), "1");
    let mut body_stream = res.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_stream.next())
        .await
        .expect("the first chunk arrives within 15s (includes the deno spawn)")
        .expect("stream open")
        .expect("the first chunk decodes");
    assert!(!first.is_empty(), "the first chunk is non-empty");
    // Disconnect MID-production: dropping the stream abandons the response.
    drop(body_stream);

    // Wait for the REAL drain completion (process reused): the reader
    // increments the `cancelled` counter before the completion signal
    // reaches the pool, so the event makes the counter observable.
    let event = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::DrainCompleted,
        10_000,
    )
    .await
    .expect("the abandon drain completes within 10s (8 x 100 ms of production)");
    assert_eq!(event.reason, "stream_abandoned_drained");
    assert_eq!(
        event.detail,
        Some("cancelled"),
        "the harness honoured the cancel frame"
    );

    let metrics_body = send(app.clone(), "/metrics").await;
    let metrics_bytes = axum::body::to_bytes(metrics_body.into_body(), usize::MAX)
        .await
        .unwrap();
    let metrics = String::from_utf8(metrics_bytes.to_vec()).unwrap();

    let cancelled = metric_value(
        &metrics,
        "edger_stream_abandoned_total{outcome=\"cancelled\"} ",
    )
    .expect("the abandoned cancelled counter is emitted; /metrics excerpt:\n{metrics}");
    assert!(
        cancelled >= 1,
        "the cancelled abandon counted at least once: {cancelled}"
    );
    let ok = metric_value(
        &metrics,
        "edger_worker_requests_total{worker=\"drain-metrics-app\",version=\"1.0.0\",namespace=\"\",outcome=\"ok\"} ",
    )
    .expect("the per-worker ok counter is emitted");
    assert_eq!(ok, 1, "the abandoned dispatch counts exactly one ok");
}
