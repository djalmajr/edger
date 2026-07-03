//! Story 16.D: passthrough streaming end-to-end — an SSE worker's events reach
//! the HTTP client INCREMENTALLY through the full pipeline (pool + axum body),
//! a client disconnect mid-stream recycles the worker instead of wedging it,
//! and buffered responses keep working identically.
//!
//! Requires `deno` on PATH. Ignored by default; run explicitly.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, ExtensionRegistry, OrchestratorState,
    ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use futures_util::StreamExt;
use tower::ServiceExt;

struct ProcessFactory;

impl IsolateFactory for ProcessFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(DenoProcessIsolate::new()),
        }
    }
}

fn state(root: std::path::PathBuf) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(ProcessFactory));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        registry: ExtensionRegistry::new(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

fn worker(root: &std::path::Path, name: &str, index: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!("name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n"),
    )
    .unwrap();
    fs::write(dir.join("index.ts"), index).unwrap();
}

const SSE_WORKER: &str = r#"Deno.serve(() => {
  let n = 0;
  let id;
  const stream = new ReadableStream({
    start(c) {
      id = setInterval(() => c.enqueue(new TextEncoder().encode(`data: tick-${n++}\n\n`)), 200);
    },
    cancel() { clearInterval(id); },
  });
  return new Response(stream, { headers: { "content-type": "text/event-stream" } });
});
"#;

async fn send(app: Router, uri: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(uri)
            .header("authorization", "Bearer test-root")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

// Mutation captured: reverting the pipeline to the buffered `fetch_worker`
// collects the (infinite) SSE body before responding — the first chunk never
// arrives and this test times out red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn sse_events_reach_the_http_client_incrementally() {
    let root = tempfile::tempdir().unwrap();
    worker(root.path(), "sse-app", SSE_WORKER);
    let app = build_pipeline(state(root.path().to_path_buf()));

    let res = send(app, "/sse-app").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "text/event-stream"
    );

    let mut body = res.into_body().into_data_stream();
    let started = Instant::now();
    let first = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("first SSE event within 5s")
        .expect("stream open")
        .expect("chunk ok");
    let first_at = started.elapsed();
    assert!(String::from_utf8_lossy(&first).contains("tick-"));

    let second = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("second SSE event within 5s")
        .expect("stream open")
        .expect("chunk ok");
    let second_at = started.elapsed();
    assert!(String::from_utf8_lossy(&second).contains("tick-"));

    assert!(
        second_at >= first_at + Duration::from_millis(100),
        "events must arrive incrementally: first {first_at:?}, second {second_at:?}"
    );
    // Dropping `body` here = client disconnect; covered by the next test.
}

// Mutation captured: removing `GuardedBody::drop`'s recycle leaves the instance
// Active after the client disconnects mid-stream, and the follow-up request
// fails with `worker not ready for dispatch`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn client_disconnect_mid_stream_recycles_the_worker() {
    let root = tempfile::tempdir().unwrap();
    worker(root.path(), "sse-app", SSE_WORKER);
    let app = build_pipeline(state(root.path().to_path_buf()));

    // First request: take one chunk, then DROP the body (client disconnect).
    let res = send(app.clone(), "/sse-app").await;
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body().into_data_stream();
    let _ = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("first chunk")
        .expect("stream open")
        .expect("chunk ok");
    drop(body);
    // Give the recycle task a beat.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Second request must get a FRESH worker, not a wedged/desynced one.
    let res = send(app, "/sse-app").await;
    assert_eq!(res.status(), StatusCode::OK, "recycled worker serves again");
    let mut body = res.into_body().into_data_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(10), body.next())
        .await
        .expect("chunk from fresh worker")
        .expect("stream open")
        .expect("chunk ok");
    assert!(String::from_utf8_lossy(&chunk).contains("tick-"));
}

// Buffered parity: a plain JSON worker responds identically through the
// streaming dispatch path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn buffered_responses_unchanged_through_streaming_path() {
    let root = tempfile::tempdir().unwrap();
    worker(
        root.path(),
        "plain-app",
        r#"Deno.serve(() => Response.json({ ok: true, n: 42 }));
"#,
    );
    let app = build_pipeline(state(root.path().to_path_buf()));

    let res = send(app, "/plain-app").await;
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&bytes);
    assert!(body.contains("\"ok\":true"), "{body}");
    assert!(body.contains("\"n\":42"), "{body}");
}
