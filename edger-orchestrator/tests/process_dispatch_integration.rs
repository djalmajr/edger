//! Story 15.B: the persistent Deno process backend serves the ExecutionKind
//! matrix (fetch/routes/SPA) through the real HTTP pipeline, no `deno eval`.
//!
//! `DenoProcessIsolate` is available because `edger-isolation` is a dev-dep with
//! the `multiproc` feature enabled.

use std::fs;
use std::sync::Arc;

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

struct ProcessFactory;

impl IsolateFactory for ProcessFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            // fetch / routes / spa all go through the persistent process isolate
            // (SPA serving stays pure-Rust inside it).
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

async fn dispatch(app: Router, method: &str, uri: &str, body: &str) -> (StatusCode, String) {
    let res = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn worker(root: &std::path::Path, name: &str, manifest: &str, files: &[(&str, &str)]) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.yaml"), manifest).unwrap();
    for (file, contents) in files {
        fs::write(dir.join(file), contents).unwrap();
    }
}

// Mutation captured: routing fetch/routes to the v1 bridge (or failing to reuse
// the persistent process) breaks the parity assertions below — the process
// backend must serve the whole matrix.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn process_backend_serves_fetch_routes_and_spa() {
    let root = tempfile::tempdir().unwrap();

    worker(
        root.path(),
        "hello",
        "name: hello\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
        &[(
            "index.ts",
            r#"Deno.serve(async (req: Request) => {
  const payload = req.body ? await req.json() : { name: "world" };
  return Response.json({ hello: payload.name });
});"#,
        )],
    );
    worker(
        root.path(),
        "routes",
        "name: routes\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: routes\n",
        &[(
            "index.ts",
            r#"export default {
  routes: {
    "/api/status": () => Response.json({ ok: true }),
    "/users/:id": (req: Request & { params: Record<string,string> }) =>
      Response.json({ user: req.params.id }),
  },
  fetch: () => new Response("fallback"),
};"#,
        )],
    );
    worker(
        root.path(),
        "panel",
        "name: panel\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: spa\n",
        &[(
            "index.html",
            "<!doctype html><html><head></head><body>panel</body></html>",
        )],
    );

    let app = build_pipeline(state(root.path().to_path_buf()));

    // fetch — with body, persistent process
    let (status, body) = dispatch(app.clone(), "POST", "/hello", r#"{"name":"edger"}"#).await;
    assert_eq!(status, StatusCode::OK, "fetch body: {body}");
    assert_eq!(body, r#"{"hello":"edger"}"#);

    // fetch again reuses the SAME warm process (module loaded once)
    let (status, body) = dispatch(app.clone(), "POST", "/hello", r#"{"name":"again"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"hello":"again"}"#);

    // routes — exact + :param via the process
    let (status, body) = dispatch(app.clone(), "GET", "/routes/api/status", "").await;
    assert_eq!(status, StatusCode::OK, "routes: {body}");
    assert_eq!(body, r#"{"ok":true}"#);
    let (status, body) = dispatch(app.clone(), "GET", "/routes/users/42", "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"user":"42"}"#);

    // static SPA — served pure-Rust with base injection, no process
    let (status, body) = dispatch(app, "GET", "/panel", "").await;
    assert_eq!(status, StatusCode::OK, "spa: {body}");
    assert!(
        body.contains("<base href=\"/panel/\" />"),
        "spa body: {body}"
    );
    assert!(body.contains("panel"));
}
