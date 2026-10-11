//! EDG-20: per-version bundle cache, end-to-end with a real Deno process.
//!
//! A multi-file worker (relative import => needs bundling) with
//! `minProcesses: 2`. The startup prewarm spawns the first process, which
//! bundles and publishes the per-version cache entry (`miss`); the second
//! prewarmed process reuses the cache (`hit` — `deno bundle` did not run a
//! second time). Editing a source file IN PLACE and recycling the version
//! changes the content key: the replenished processes must be a fresh
//! `miss`, and the responses reflect the edited file.
//!
//! The bundle counters are read from the data-plane `/metrics` (root key),
//! next to the other multiproc counters; the multiproc block is rendered
//! when the stream-detach budget is registered, as in the `edger` binary for
//! the multiproc backend.
//!
//! Requires `deno` on PATH. Ignored by default; run explicitly:
//! `cargo test -p edger-orchestrator --test bundle_cache_e2e -- --ignored`

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, StreamDetachBudget, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, prewarm_min_process_workers, ControlAuth,
    OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

/// Factory that answers with real Deno processes for process-kind workers.
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
    // The multiproc /metrics block (including the bundle cache counters) is
    // rendered only when the detach budget is registered — mirror the
    // `edger` binary wiring for the multiproc backend.
    server.set_stream_detach_budget(Arc::new(StreamDetachBudget::new(64 << 20)));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

/// Multi-file worker (relative import => bundling required), with a
/// module-scope `x-seq` counter and a `greeting.ts` whose content is part of
/// the response body (the edit-in-place step is observable).
fn write_bundle_worker(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmaxProcesses: 2\nminProcesses: 2\nttl: \"60s\"\n"
        ),
    )
    .unwrap();
    fs::write(dir.join("greeting.ts"), "export const greeting = 'v1';\n").unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"import { greeting } from "./greeting.ts";
let seq = 0;
Deno.serve(() => {
  seq += 1;
  return new Response(`ok-${greeting}`, {
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
    .expect("the request completed (30s budget includes the deno spawns)")
    .unwrap()
}

async fn post(app: Router, uri: &str) -> axum::http::Response<Body> {
    tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("the admin call completed")
    .unwrap()
}

/// The data-plane `/metrics` body (root key has `observability:read`).
async fn fetch_metrics(app: &Router) -> String {
    let res = send(app.clone(), "/metrics").await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "/metrics answers the root key"
    );
    String::from_utf8(
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// `edger_worker_bundle_total{outcome="<outcome>"}` from a /metrics body.
fn bundle_counter(body: &str, outcome: &str) -> u64 {
    let prefix = format!("edger_worker_bundle_total{{outcome=\"{outcome}\"}}");
    body.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|rest| rest.trim().parse::<u64>().ok())
        .unwrap_or_else(|| panic!("missing {prefix} in /metrics body: {body}"))
}

/// `/metrics/stats` worker entries (control-plane read — it does NOT
/// dispatch to the worker app).
#[derive(serde::Deserialize)]
struct StatsResponse {
    #[serde(default)]
    workers: Vec<StatsWorker>,
}

#[derive(serde::Deserialize)]
struct StatsWorker {
    name: String,
    #[serde(default, rename = "idleProcesses")]
    idle_processes: usize,
}

/// Poll `/metrics/stats` until the named app has at least `count` IDLE
/// processes (no data-plane request in between), within `timeout`.
async fn stats_has_idle_processes(
    app: &Router,
    name: &str,
    count: usize,
    timeout: Duration,
) -> bool {
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
                .any(|worker| worker.name == name && worker.idle_processes >= count)
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

// The full EDG-20 scenario: prewarm of `minProcesses: 2` bundles ONCE
// (miss=1, hit=1), an in-place edit + recycle invalidates the key (a fresh
// miss) and the responses stay correct with the edited file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn second_process_reuses_the_cached_bundle_and_edit_invalidates_it() {
    let root = tempfile::tempdir().unwrap();
    write_bundle_worker(root.path(), "bundle-app");
    let st = state(root.path().to_path_buf());

    // The `edger` binary prewarms the minProcesses floor at startup and
    // waits for it: two processes of the same version spawn back to back.
    prewarm_min_process_workers(&st.index, &st.pool)
        .await
        .unwrap();
    let app = build_pipeline(st);

    // Both prewarmed processes are idle before we assert anything.
    assert!(
        stats_has_idle_processes(&app, "bundle-app", 2, Duration::from_secs(90)).await,
        "the prewarm must leave two idle processes"
    );

    // The first spawn bundled and published (miss); the second spawn reused
    // the per-version cache (hit) — the bundler ran exactly once.
    let metrics = fetch_metrics(&app).await;
    assert_eq!(
        bundle_counter(&metrics, "miss"),
        1,
        "one build for the prewarm"
    );
    assert_eq!(
        bundle_counter(&metrics, "hit"),
        1,
        "the second process is a cache hit (no deno bundle)"
    );
    assert_eq!(bundle_counter(&metrics, "error"), 0);
    assert_eq!(bundle_counter(&metrics, "uncached"), 0);

    // The response is correct on a prewarmed (bundled) process.
    let res = send(app.clone(), "/bundle-app").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("x-seq").unwrap(),
        "1",
        "the prewarmed process has not served a user request yet"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body, axum::body::Bytes::from_static(b"ok-v1"));

    // Edit a source file IN PLACE (dev dirs and the WebIDE do exactly this),
    // then recycle the version: the content key changes, so the replenished
    // processes must build a fresh bundle (a new miss) — and the cache keeps
    // only the new key.
    fs::write(
        root.path().join("bundle-app").join("greeting.ts"),
        "export const greeting = 'v2';\n",
    )
    .unwrap();
    let recycled = post(
        app.clone(),
        "/api/admin/workers/bundle-app/recycle?version=1.0.0",
    )
    .await;
    assert_eq!(recycled.status(), StatusCode::OK);
    let recycled_body = axum::body::to_bytes(recycled.into_body(), usize::MAX)
        .await
        .unwrap();
    let recycled_json: serde_json::Value =
        serde_json::from_slice(&recycled_body).expect("the recycle route returns JSON");
    assert_eq!(
        recycled_json["recycled"].as_u64(),
        Some(2),
        "both version processes were recycled"
    );

    // Wait for the floor replenishment to bring the (rebuilt) processes back
    // WITHOUT a data-plane request in between.
    assert!(
        stats_has_idle_processes(&app, "bundle-app", 2, Duration::from_secs(90)).await,
        "the floor replenishment must respawn the two processes after the recycle"
    );

    // The edited key invalidated the cache: a fresh miss (total 2) and the
    // second replenished process hit the new key (total 2).
    let metrics = fetch_metrics(&app).await;
    assert_eq!(
        bundle_counter(&metrics, "miss"),
        2,
        "the edited tree is a new key: a fresh bundle build"
    );
    assert_eq!(
        bundle_counter(&metrics, "hit"),
        2,
        "the second replenished process reused the new cache entry"
    );
    assert_eq!(bundle_counter(&metrics, "error"), 0);
    assert_eq!(bundle_counter(&metrics, "uncached"), 0);

    // Responses stay correct with the edited file (the new bundle was used).
    let res = send(app.clone(), "/bundle-app").await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        body,
        axum::body::Bytes::from_static(b"ok-v2"),
        "the edited file is served by the rebundled processes"
    );
}
