//! Story 15.C: real JS frameworks (Express via node:http, Hono via Deno.serve)
//! run on the persistent Deno process backend — full Deno compat, no reimpl.
//!
//! Requires `deno` on PATH and network access to resolve `npm:` on a cold
//! cache. Ignored by default to keep the core suite hermetic; run explicitly.

use std::fs;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_isolation::{DenoProcessIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, AuthGate, AuthGateConfig, ExtensionRegistry,
    OrchestratorState, ServerState,
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
        auth: AuthGate::new(
            AuthGateConfig::default(),
            Arc::new(AuthExtension::new(
                Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
                Some("test-root".into()),
            )),
        ),
    }
}

async fn get(app: Router, uri: &str) -> (StatusCode, String) {
    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
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

// Mutation captured: dropping the node:http listener capture in the harness
// leaves Express with no handler — spawn fails / route 500s and this goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno + npm network (cold cache); run explicitly"]
async fn express_and_hono_run_on_the_process_backend() {
    let root = tempfile::tempdir().unwrap();
    worker(
        root.path(),
        "express-app",
        r#"import express from "npm:express@5";
const app = express();
app.get("/", (_req, res) => res.json({ framework: "express" }));
app.get("/users/:id", (req, res) => res.json({ user: req.params.id }));
app.listen(3000);
"#,
    );
    worker(
        root.path(),
        "hono-app",
        r#"import { Hono } from "npm:hono@4";
const app = new Hono();
app.get("/", (c) => c.json({ framework: "hono" }));
app.get("/users/:id", (c) => c.json({ user: c.req.param("id") }));
Deno.serve(app.fetch);
"#,
    );

    let app = build_pipeline(state(root.path().to_path_buf()));

    // Express via node:http listener capture
    let (status, body) = get(app.clone(), "/express-app").await;
    assert_eq!(status, StatusCode::OK, "express root: {body}");
    assert!(
        body.contains("\"framework\":\"express\""),
        "express: {body}"
    );
    let (status, body) = get(app.clone(), "/express-app/users/7").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"user\":\"7\""), "express param: {body}");

    // Hono via Deno.serve capture (warm process reused)
    let (status, body) = get(app.clone(), "/hono-app").await;
    assert_eq!(status, StatusCode::OK, "hono root: {body}");
    assert!(body.contains("\"framework\":\"hono\""), "hono: {body}");
    let (status, body) = get(app, "/hono-app/users/9").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"user\":\"9\""), "hono param: {body}");
}
