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
//!
//! EDG-13 adds the EMPTIED-floor scenario: with `minProcesses: 1` and
//! `maxRequests: 3`, the third request retires the only instance and the
//! group is emptied; without any request the pool must refill the emptied
//! (floored) generation in the background — a new Idle process appears in
//! `/metrics/stats` — and the fourth request is served by that already-
//! alive process (module counter reset, no cold start on the request path).

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, prewarm_min_process_workers, ControlAuth,
    OrchestratorState, ServerState,
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

/// Worker whose ONLY instance retires after `maxRequests` requests
/// (`maxRequests: 3`): the same module-scope `x-seq` counter as
/// `write_floor_worker`, plus a 60 s TTL (well outside the test window —
/// the floor, not the TTL, keeps anything alive).
fn write_max_requests_worker(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nminProcesses: 1\nmaxRequests: 3\nttl: \"60s\"\n"
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

/// `/metrics/stats` worker entries, as the orchestrator exposes them
/// (control-plane read — it does NOT dispatch to the worker app).
#[derive(serde::Deserialize)]
struct StatsResponse {
    #[serde(default)]
    workers: Vec<StatsWorker>,
}

#[derive(serde::Deserialize)]
struct StatsWorker {
    name: String,
    // /metrics/stats serializes camelCase (`MetricsWorkerStats`).
    #[serde(default, rename = "idleProcesses")]
    idle_processes: usize,
}

/// Poll `/metrics/stats` (control plane only — no data-plane request is
/// sent to the worker) until the named app has at least one IDLE process
/// reported, within a short timeout.
async fn stats_has_idle_process(app: &Router, name: &str, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let res = match tokio::time::timeout(
            Duration::from_secs(10),
            app.clone().oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/metrics/stats")
                    .header("authorization", "Bearer test-root")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        {
            Ok(Ok(res)) => res,
            _ => {
                // A stats read that failed/timed out is not evidence:
                // retry until the deadline.
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if res.status() == StatusCode::OK {
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            let stats: StatsResponse =
                serde_json::from_slice(&body).expect("the stats endpoint returns JSON");
            if stats
                .workers
                .iter()
                .any(|worker| worker.name == name && worker.idle_processes >= 1)
            {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// EDG-15: opt-in process warmup. With `minProcesses: 1`, `maxRequests: 3`
// and `warmup: {path: /}`, the `edger` startup prewarm spawns the process
// and sends ONE synthetic GET to it BEFORE it goes Idle (x-seq 1 is the
// warmup). The first user request must therefore see `x-seq: 2` on the
// same process — without the warmup it would cold-start and see `1`. The
// third user request retires the process (maxRequests) and the background
// replenishment refills the floor with a NEW, ALSO-WARMED process: the
// fourth user request sees `x-seq: 2` again (its seq 1 was the warmup),
// with no cold start on the request path (the process was observed Idle in
// /metrics/stats BEFORE the fourth request).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn warmup_process_answers_the_first_request_with_executed_code() {
    let root = tempfile::tempdir().unwrap();
    write_warmup_worker(root.path(), "warm-app");
    let st = state(root.path().to_path_buf());
    // The `edger` binary prewarms the minProcesses floor at startup and
    // waits for it: the warmup GET runs on the freshly spawned process
    // before it goes Idle.
    prewarm_min_process_workers(&st.index, &st.pool)
        .await
        .unwrap();
    let app = build_pipeline(st);

    // User request #1: the SAME warmed process serves it — the module-scope
    // counter continues at 2 (a cold start, i.e. no warmup, would answer 1).
    let res = send(app.clone(), "/warm-app").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-seq").unwrap(),
        "2",
        "the warmup ran on this process: the first user request is seq 2"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body, axum::body::Bytes::from_static(b"ok"));

    // User requests #2/#3: same process (seq 3, 4); on #3 the process
    // reaches maxRequests and retires — the group is emptied and the
    // background replenishment spawns a new, warmed process (its seq 1 is
    // the warmup).
    for expected in ["3", "4"] {
        let res = send(app.clone(), "/warm-app").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get("x-seq").unwrap(),
            expected,
            "requests #2/#3 run on the first (warmed) process"
        );
        let _ = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
    }

    // Wait WITHOUT a data-plane request until the replenished process is
    // Idle in /metrics/stats: the fourth request must land on an already
    // alive (and warmed) process, not cold-start it.
    let seen_idle = stats_has_idle_process(&app, "warm-app", Duration::from_secs(15)).await;
    assert!(
        seen_idle,
        "the replenished process must be idle before the fourth request"
    );

    // User request #4: the NEW, warmed process — x-seq 2 (its seq 1 was
    // the warmup), no cold start on the request path.
    let res = send(app, "/warm-app").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-seq").unwrap(),
        "2",
        "the replenished process was warmed too: the fourth user request is seq 2"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body, axum::body::Bytes::from_static(b"ok"));
}

/// Worker with the same module-scope `x-seq` counter as
/// `write_max_requests_worker`, plus `minProcesses: 1`, `maxRequests: 3`
/// and the opt-in `warmup` manifest field.
fn write_warmup_worker(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nminProcesses: 1\nmaxRequests: 3\nttl: \"60s\"\nwarmup:\n  path: /\n"
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

// EDG-13: the floor is re-established when the LAST instance retires. With
// `minProcesses: 1` and `maxRequests: 3` the third request retires the only
// instance and EMPTIES the group; without any request the background
// replenishment refills the emptied (floored) generation — a new Idle
// process appears in `/metrics/stats` — and the fourth request is served by
// that already-alive process: the module counter resets to 1 (new process,
// the retired one is not reused) and NO cold start ran on the request path
// (the process was observed Idle in the stats BEFORE the fourth request —
// in the old behavior the group left the cache, the stats never showed a
// process, and the fourth request would have had to cold-start it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn emptied_floor_group_is_replenished_without_a_request() {
    let root = tempfile::tempdir().unwrap();
    write_max_requests_worker(root.path(), "floor-maxreq");
    let app = build_pipeline(state(root.path().to_path_buf()));

    // Requests #1-#3: the only instance serves all three; on the third it
    // reaches maxRequests and retires — the group is emptied.
    for expected in ["1", "2", "3"] {
        let res = send(app.clone(), "/floor-maxreq").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get("x-seq").unwrap(),
            expected,
            "requests #1-#3 run on the first process"
        );
        let _ = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
    }

    // Wait WITHOUT a data-plane request until a new Idle process appears in
    // /metrics/stats (short timeout): the background replenishment must
    // refill the emptied floored generation. In the old behavior the group
    // left the cache, so no process ever appears and this times out.
    let seen_idle = stats_has_idle_process(&app, "floor-maxreq", Duration::from_secs(15)).await;
    assert!(
        seen_idle,
        "a new idle process must appear in /metrics/stats without any request"
    );

    // Request #4: the NEW process serves it — the module-scope counter
    // resets to 1 (the retired process is not reused). The process was
    // already alive (observed Idle above), so no cold start ran on the
    // request path.
    let res = send(app, "/floor-maxreq").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-seq").unwrap(),
        "1",
        "x-seq must reset: a fresh (replenished) process serves the fourth request"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body, axum::body::Bytes::from_static(b"ok"));
}
