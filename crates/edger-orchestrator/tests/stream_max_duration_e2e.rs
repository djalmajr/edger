//! EDG-16: total response-stream duration cuts an endless SSE stream through
//! the EDG-9 cancel/drain path, records its outcome and exposes active work.
//! Requires deno on PATH; ignored by default.

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

const DEFAULT_STREAM_MAX_DURATION_MS: u64 = 300_000;
static STREAM_DURATION_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
struct MaxDurationFactory {
    budget: Arc<StreamDetachBudget>,
    default_duration_ms: u64,
    drain_max_bytes: u64,
    drain_max_ms: u64,
}

impl MaxDurationFactory {
    fn from_env() -> Self {
        let default_duration_ms = std::env::var("EDGER_STREAM_MAX_DURATION_MS")
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(DEFAULT_STREAM_MAX_DURATION_MS);
        Self {
            budget: Arc::new(StreamDetachBudget::new(8 * 1024 * 1024)),
            default_duration_ms,
            drain_max_bytes: 8 * 1024 * 1024,
            drain_max_ms: 2_000,
        }
    }
}

impl IsolateFactory for MaxDurationFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(
                DenoProcessIsolate::new()
                    .with_stream_detach(8 * 1024 * 1024, Arc::clone(&self.budget))
                    .with_abandon_drain_limits(self.drain_max_bytes, self.drain_max_ms)
                    .with_stream_max_duration_default_ms(self.default_duration_ms),
            ),
        }
    }
}

fn state_with_lifecycle(
    root: std::path::PathBuf,
    factory: MaxDurationFactory,
) -> (
    OrchestratorState,
    tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
) {
    let server = ServerState::new_unready();
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

fn write_sse_worker(root: &std::path::Path, name: &str, stream_timeout: Option<&str>) {
    write_sse_worker_with_pool(root, name, stream_timeout, 1, 0);
}

fn write_sse_worker_with_pool(
    root: &std::path::Path,
    name: &str,
    stream_timeout: Option<&str>,
    max_processes: usize,
    min_processes: usize,
) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let timeout_field = stream_timeout
        .map(|value| format!("streamTimeout: {value}\n"))
        .unwrap_or_default();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmaxProcesses: {max_processes}\nminProcesses: {min_processes}\nqueueTimeout: 1s\n{timeout_field}"
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
  let tick = 0;
  let timer;
  const stream = new ReadableStream({
    start(controller) {
      timer = setInterval(() => {
        controller.enqueue(enc.encode("data: tick-" + tick++ + "\n\n"));
      }, 100);
    },
    cancel() { clearInterval(timer); },
  });
  return new Response(stream, {
    headers: { "content-type": "text/event-stream", "x-seq": String(seq) },
  });
});
"#,
    )
    .unwrap();
}

fn worker_stats_optional<'a>(
    stats: &'a serde_json::Value,
    name: &str,
) -> Option<&'a serde_json::Value> {
    stats["workers"]
        .as_array()?
        .iter()
        .find(|worker| worker["name"] == name)
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

async fn max_duration_event_for(
    rx: &mut tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
    request_id: &str,
    timeout: Duration,
) -> Option<WorkerLifecycleEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        while let Ok(event) = rx.try_recv() {
            if event.kind == WorkerLifecycleEventKind::StreamMaxDuration
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
async fn stream_max_duration_precedence_visibility_reuse_and_event() {
    let _env_lock = STREAM_DURATION_ENV_LOCK.lock().await;
    let previous_env = std::env::var("EDGER_STREAM_MAX_DURATION_MS").ok();
    unsafe { std::env::set_var("EDGER_STREAM_MAX_DURATION_MS", "1000") };

    let root = tempfile::tempdir().unwrap();
    write_sse_worker(root.path(), "duration-manifest", Some("1s"));
    write_sse_worker(root.path(), "duration-env", None);
    write_sse_worker(root.path(), "duration-zero", Some("0"));
    write_sse_worker_with_pool(root.path(), "duration-identity", Some("1s"), 2, 2);
    let factory = MaxDurationFactory::from_env();
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state.clone());

    // Manifest value wins over the 1 s process default. A live stats read
    // must expose this stream's request ID, positive age and streaming state.
    let request_id = "edg16-manifest-stream";
    let started = Instant::now();
    let response = send_with_id(&app, "/duration-manifest", request_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-seq").unwrap(), "1");
    let mut body = response.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(3), body.next())
        .await
        .expect("first heartbeat arrives")
        .expect("stream stays open")
        .expect("heartbeat body is valid");
    assert!(String::from_utf8_lossy(&first).contains("tick-"));

    let live_stats = stats_json(&app).await;
    let live_worker = worker_stats(&live_stats, "duration-manifest");
    let active = &live_worker["processes"][0]["activeRequest"];
    assert_eq!(active["requestId"], request_id);
    assert!(active["ageMs"].as_u64().unwrap() > 0);
    assert_eq!(active["streaming"], true);
    let instance_id = live_worker["id"].as_str().unwrap().to_string();

    let mut heartbeats = 1;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = body.next().await {
            chunk.expect("duration cut ends the body cleanly");
            heartbeats += 1;
        }
    })
    .await
    .expect("the stream ends before ten seconds");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(1),
        "cut too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "cut too late: {elapsed:?}"
    );
    assert!(heartbeats > 1, "heartbeats must arrive before cutoff");
    wait_for_active_request_null(&app, "duration-manifest").await;

    let event = max_duration_event_for(&mut lifecycle_rx, request_id, Duration::from_secs(5))
        .await
        .expect("pool lifecycle publishes the duration cut");
    assert_eq!(event.process_id.as_deref(), Some(instance_id.as_str()));
    assert_eq!(event.detail, Some("cancelled"));
    assert!(
        event
            .duration_ms
            .is_some_and(|duration| (1_000..10_000).contains(&duration)),
        "event duration must reflect the stream age at cutoff: {:?}",
        event.duration_ms
    );

    let metric = metrics_text(&app).await;
    assert!(
        metric.contains("edger_stream_max_duration_total{outcome=\"cancelled\"} 1"),
        "max-duration metric missing from /metrics:\n{metric}"
    );
    assert!(
        metric.contains("edger_stream_abandoned_total{outcome=\"cancelled\"} 0"),
        "abandoned counter changed for duration cut:\n{metric}"
    );
    let next = send_with_id(&app, "/duration-manifest/seq", "edg16-manifest-next").await;
    assert_eq!(next.headers().get("x-seq").unwrap(), "2");

    // With no manifest field, the process default from the env value applies.
    let env_id = "edg16-env-stream";
    let env_started = Instant::now();
    let response = send_with_id(&app, "/duration-env", env_id).await;
    assert_eq!(response.headers().get("x-seq").unwrap(), "1");
    let mut body = response.into_body().into_data_stream();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = body.next().await {
            chunk.expect("env default also ends the body cleanly");
        }
    })
    .await
    .expect("the env duration cuts the stream");
    assert!(env_started.elapsed() >= Duration::from_secs(1));
    let env_metric = metrics_text(&app).await;
    assert!(env_metric.contains("edger_stream_max_duration_total{outcome=\"cancelled\"} 2"));
    assert!(env_metric.contains("edger_stream_abandoned_total{outcome=\"cancelled\"} 0"));

    // An explicit manifest zero overrides the 1 s environment default. The
    // body remains live for three seconds; then this test drops the client.
    let zero_id = "edg16-zero-stream";
    let response = send_with_id(&app, "/duration-zero", zero_id).await;
    assert_eq!(response.headers().get("x-seq").unwrap(), "1");
    let mut body = response.into_body().into_data_stream();
    let zero_started = Instant::now();
    let first_chunk = body.next().await.unwrap().unwrap();
    assert!(!first_chunk.is_empty());
    while zero_started.elapsed() < Duration::from_secs(3) {
        if let Some(chunk) = tokio::time::timeout(Duration::from_millis(250), body.next())
            .await
            .expect("explicit zero continues to produce heartbeats")
        {
            chunk.unwrap();
        }
    }
    assert!(zero_started.elapsed() >= Duration::from_secs(3));
    drop(body);
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let zero_metric = metrics_text(&app).await;
    assert!(zero_metric.contains("edger_stream_max_duration_total{outcome=\"cancelled\"} 2"));
    assert!(zero_metric.contains("edger_stream_abandoned_total{outcome=\"cancelled\"} 1"));
    assert!(
        max_duration_event_for(&mut lifecycle_rx, zero_id, Duration::from_millis(100))
            .await
            .is_none(),
        "a zero override must not emit a max-duration event"
    );

    // With two live processes, the lifecycle process ID must identify the
    // process whose activeRequest carries this stream's request ID.
    let identity_worker = state
        .index
        .worker_refs()
        .into_iter()
        .find(|worker| worker.name == "duration-identity")
        .expect("identity worker is loaded");
    assert_eq!(
        state.pool.prewarm_worker(&identity_worker).await.unwrap(),
        2
    );
    let identity_request_id = "edg16-process-identity";
    let response = send_with_id(&app, "/duration-identity", identity_request_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    tokio::time::timeout(Duration::from_secs(3), body.next())
        .await
        .expect("identity stream heartbeat arrives")
        .expect("identity stream stays open")
        .expect("identity stream body is valid");
    let stats = stats_json(&app).await;
    let identity_worker_stats = worker_stats(&stats, "duration-identity");
    let processes = identity_worker_stats["processes"]
        .as_array()
        .expect("process array");
    assert_eq!(processes.len(), 2, "both prewarmed processes are visible");
    let active_process = processes
        .iter()
        .find(|process| process["activeRequest"]["requestId"] == identity_request_id)
        .expect("one process carries the active stream request");
    let active_process_id = active_process["id"].as_str().unwrap().to_string();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = body.next().await {
            chunk.expect("identity stream ends cleanly at max duration");
        }
    })
    .await
    .expect("identity stream ends before ten seconds");
    let identity_event = max_duration_event_for(
        &mut lifecycle_rx,
        identity_request_id,
        Duration::from_secs(5),
    )
    .await
    .expect("pool lifecycle publishes the identity stream cut");
    assert_eq!(
        identity_event.process_id.as_deref(),
        Some(active_process_id.as_str())
    );

    unsafe {
        match previous_env {
            Some(value) => std::env::set_var("EDGER_STREAM_MAX_DURATION_MS", value),
            None => std::env::remove_var("EDGER_STREAM_MAX_DURATION_MS"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn recycle_endpoint_releases_an_unbounded_stream_and_next_request_gets_new_process() {
    let _env_lock = STREAM_DURATION_ENV_LOCK.lock().await;
    let previous_env = std::env::var("EDGER_STREAM_MAX_DURATION_MS").ok();
    unsafe { std::env::set_var("EDGER_STREAM_MAX_DURATION_MS", "1000") };

    let root = tempfile::tempdir().unwrap();
    write_sse_worker(root.path(), "duration-recycle", Some("0"));
    let factory = MaxDurationFactory::from_env();
    let (state, _lifecycle_rx) = state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state.clone());

    let request_id = "edg16-recycle-active-stream";
    let response = send_with_id(&app, "/duration-recycle", request_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    tokio::time::timeout(Duration::from_secs(3), body.next())
        .await
        .expect("SSE heartbeat arrives")
        .expect("stream remains open")
        .expect("heartbeat is valid");

    let before = stats_json(&app).await;
    let process = worker_stats(&before, "duration-recycle")["processes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|process| process["activeRequest"]["requestId"] == request_id)
        .expect("active SSE process is visible");
    let old_process_id = process["id"].as_str().unwrap().to_string();

    let started = Instant::now();
    let recycle_response = tokio::time::timeout(
        Duration::from_secs(15),
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/workers/duration-recycle/recycle?version=1.0.0")
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("recycle response completes within 15 seconds")
    .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(recycle_response.status(), StatusCode::OK);
    assert!(
        elapsed < Duration::from_secs(15),
        "recycle took {elapsed:?}"
    );
    let recycle_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(recycle_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(recycle_json["version"], "1.0.0");
    assert!(recycle_json["recycled"].as_u64().unwrap() >= 1);
    assert_eq!(recycle_json["prewarm"], "not_configured");

    let after_recycle = stats_json(&app).await;
    if let Some(worker) = worker_stats_optional(&after_recycle, "duration-recycle") {
        assert_eq!(worker["totalProcesses"], 0, "old group still has a process");
        assert!(worker["processes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|process| { process["activeRequest"].is_null() }));
    }

    drop(body);
    let next = send_with_id(&app, "/duration-recycle/seq", "edg16-recycle-next-request").await;
    assert_eq!(next.status(), StatusCode::OK);
    assert_eq!(next.headers().get("x-seq").unwrap(), "1");
    let after_next = stats_json(&app).await;
    let new_worker = worker_stats(&after_next, "duration-recycle");
    let new_process_id = new_worker["processes"][0]["id"].as_str().unwrap();
    assert_ne!(
        new_process_id, old_process_id,
        "recycle must replace the process"
    );

    unsafe {
        match previous_env {
            Some(value) => std::env::set_var("EDGER_STREAM_MAX_DURATION_MS", value),
            None => std::env::remove_var("EDGER_STREAM_MAX_DURATION_MS"),
        }
    }
}
