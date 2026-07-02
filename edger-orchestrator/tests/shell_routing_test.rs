//! Story 07.02 — SPA base injection under namespaced paths.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_isolation::{DenoFacade, DenoIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, AuthGate, AuthGateConfig, ExtensionRegistry,
    OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct RuntimeFactory;

impl IsolateFactory for RuntimeFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(DenoIsolate::new(DenoFacade::new())),
        }
    }
}

fn state_with_workers(root: std::path::PathBuf) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(RuntimeFactory));
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

async fn dispatch(app: Router, uri: &str) -> (StatusCode, String, Option<String>) {
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
    let content_type = res
        .headers()
        .get("content-type")
        .map(|value| value.to_str().unwrap().to_string());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        String::from_utf8_lossy(&body).into_owned(),
        content_type,
    )
}

fn write_spa_fixture(dir: &Path, manifest: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("manifest.yaml"), manifest).unwrap();
    fs::write(
        dir.join("index.html"),
        r#"<!doctype html><html><head><title>panel</title></head><body><script src="./app.js"></script></body></html>"#,
    )
    .unwrap();
    fs::write(dir.join("app.js"), "console.log('panel');").unwrap();
}

// Mutation captured: dropping the `base_href` computation in the dispatch
// pipeline (or the `<base>` injection in `serve_static_spa`) serves the
// namespaced SPA without a `<base href="/@team/panel/">` tag and this test
// goes red.
#[tokio::test]
async fn namespaced_spa_receives_injected_base_href() {
    let root = tempfile::tempdir().unwrap();
    write_spa_fixture(
        &root.path().join("panel"),
        r#"name: "@team/panel"
version: "1.0.0"
entrypoint: index.html
kind: spa
"#,
    );

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));

    let (status, body, _) = dispatch(app.clone(), "/@team/panel").await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert!(
        body.contains(r#"<base href="/@team/panel/" />"#),
        "missing injected base href: {body}"
    );

    // Relative asset referenced by the HTML resolves through the same route.
    let (status, body, content_type) = dispatch(app, "/@team/panel/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "console.log('panel');");
    assert_eq!(
        content_type.as_deref(),
        Some("application/javascript; charset=utf-8")
    );
}

// Mutation captured: forcing base injection regardless of the manifest flag
// (ignoring `inject_base: false`) adds a `<base>` tag to this SPA and the
// negative assertion goes red.
#[tokio::test]
async fn spa_with_inject_base_false_serves_untouched_html() {
    let root = tempfile::tempdir().unwrap();
    write_spa_fixture(
        &root.path().join("rawspa"),
        r#"name: rawspa
version: "1.0.0"
entrypoint: index.html
kind: spa
injectBase: false
"#,
    );

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));

    let (status, body, _) = dispatch(app, "/rawspa").await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {body}");
    assert!(
        !body.contains("<base"),
        "HTML must not be modified when injectBase is false: {body}"
    );
}
