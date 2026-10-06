//! EDG-10: `minProcesses` is a MAINTAINED floor, not a one-shot prewarm.
//!
//! End-to-end proof with a real Deno process: a worker with
//! `minProcesses: 1` and `ttl: "1s"` serves one request, then stays idle for
//! 3 s. Its TTL timer expires TWICE while idle; without the floor the
//! process would be terminated on the first expiry (cold start on the next
//! request). With the floor, every expiry that would drop the group below
//! `minProcesses` keeps the instance `Idle` and re-arms the timer — so the
//! next request is served by the SAME process.
//!
//! The process identity is proven with a module-scope `x-seq` counter (the
//! same technique as EDG-8/9): the counter only resets when the Deno process
//! is respawned. Request #1 returns `x-seq: 1`; request #2 — sent 3 s later
//! after the two expired TTLs — must return `x-seq: 2`, not `1`.
//!
//! Requires `deno` on PATH. Ignored by default; run explicitly:
//! `cargo test -p edger-orchestrator --test min_processes_e2e -- --ignored`

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

/// Factory that answers with real Deno processes for process-kind workers
/// (the floor only matters for persistent-process backends).
#[derive(Clone)]
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
        auth: ControlAuth::with_static_key("test-root"),
    }
}

/// Worker with a module-scope request counter exposed as the `x-seq` header:
/// it increments per request and only resets when the Deno process is
/// respawned, so a reset to `1` on the second request would prove a recycle.
fn write_floor_worker(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nminProcesses: 1\nttl: \"1s\"\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"let seq = 0;
Deno.serve(() => {
  seq += 1;
  return new Response("ok", {
    headers: { "content-type": "text/plain", "x-seq": String(seq) },
  });
});
"#,
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
    .expect("the request completed (30s budget includes the deno spawn)")
    .unwrap()
}

// The core EDG-10 scenario: the floor instance survives the idle TTL
// expirations and the SAME process serves the next request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn floor_instance_survives_idle_ttl_and_serves_the_next_request() {
    let root = tempfile::tempdir().unwrap();
    write_floor_worker(root.path(), "floor-app");
    let app = build_pipeline(state(root.path().to_path_buf()));

    // Request #1: the process serves it and then goes Idle; its TTL timer
    // (1 s) is armed from here.
    let res_a = send(app.clone(), "/floor-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(
        res_a.headers().get("x-seq").unwrap(),
        "1",
        "first request on a fresh process"
    );
    let body_a = axum::body::to_bytes(res_a.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body_a, axum::body::Bytes::from_static(b"ok"));

    // 3 s idle: the 1 s TTL expires twice. Every expiry that would drop the
    // group below minProcesses (1) must KEEP the instance Idle and re-arm
    // the timer — no termination, no respawn.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Request #2: the SAME process must serve it — the module-scope counter
    // continues (2, not a reset to 1).
    let res_b = send(app, "/floor-app").await;
    assert_eq!(res_b.status(), StatusCode::OK);
    assert_eq!(
        res_b.headers().get("x-seq").unwrap(),
        "2",
        "x-seq must not reset: the floor process was not recycled while idle"
    );
}
