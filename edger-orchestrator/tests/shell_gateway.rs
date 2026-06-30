//! Shell/gateway integration tests (story 08.05).

use std::fs;
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
        auth: AuthGate::new(
            AuthGateConfig::default(),
            Arc::new(AuthExtension::new(
                Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
                Some("test-root".into()),
            )),
        ),
        index: load_manifests_from_dirs(&[root]).unwrap(),
        pool,
        registry: ExtensionRegistry::new(),
        server,
    }
}

async fn dispatch(
    app: Router,
    uri: &str,
    fetch_dest: Option<&str>,
    authenticated: bool,
) -> (StatusCode, bytes::Bytes) {
    let mut request = Request::builder().method("GET").uri(uri);
    if authenticated {
        request = request.header("authorization", "Bearer test-root");
    }
    if let Some(fetch_dest) = fetch_dest {
        request = request.header("sec-fetch-dest", fetch_dest);
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body)
}

fn write_shell_fixture(root: &std::path::Path) {
    let shell_dir = root.join("shell-demo");
    fs::create_dir_all(&shell_dir).unwrap();
    fs::write(
        shell_dir.join("manifest.yaml"),
        r#"name: shell-demo
version: "1.0.0"
entrypoint: index.html
base: "/"
injectBase: true
shellExcludes:
  - todos-shell-demo
"#,
    )
    .unwrap();
    fs::write(
        shell_dir.join("index.html"),
        r#"<!doctype html><html><head><script src="/shell.js"></script></head><body><main>shell-demo</main><iframe src="/todos-shell-demo" title="todos-shell-demo"></iframe></body></html>"#,
    )
    .unwrap();
    fs::write(shell_dir.join("shell.js"), "globalThis.shellLoaded = true;").unwrap();
}

fn write_todos_fixture(root: &std::path::Path) {
    let worker_dir = root.join("todos-shell-demo");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: todos-shell-demo
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r#"Deno.serve((req: Request) => {
  const url = new URL(req.url);
  return new Response(`${url.pathname} base=${req.headers.get("x-base")}`);
});
"#,
    )
    .unwrap();
}

fn app_with_shell() -> (Router, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    write_shell_fixture(root.path());
    write_todos_fixture(root.path());
    let state = state_with_workers(root.path().to_path_buf());
    (build_pipeline(state), root)
}

#[tokio::test]
async fn document_navigation_serves_shell_with_root_base() {
    let (app, _root) = app_with_shell();
    let (status, body) = dispatch(app, "/reports/list", Some("document"), true).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("shell-demo"));
    assert!(html.contains(r#"<base href="/" />"#));
    assert!(html.contains(r#"<iframe src="/todos-shell-demo""#));
}

#[tokio::test]
async fn shell_single_segment_asset_is_served_by_shell_worker() {
    let (app, _root) = app_with_shell();
    let (status, body) = dispatch(app, "/shell.js", None, true).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"globalThis.shellLoaded = true;");
}

#[tokio::test]
async fn iframe_app_bypasses_shell_and_receives_worker_base() {
    let (app, _root) = app_with_shell();
    let (status, body) = dispatch(app, "/todos-shell-demo/list", Some("iframe"), true).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"/list base=/todos-shell-demo");
}

#[tokio::test]
async fn protected_shell_requires_authentication() {
    let (app, _root) = app_with_shell();
    let (status, body) = dispatch(app, "/reports/list", Some("document"), false).await;

    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
}

#[tokio::test]
async fn reserved_admin_path_is_not_intercepted_by_shell() {
    let (app, _root) = app_with_shell();
    let (status, body) = dispatch(app, "/api/admin/session", Some("document"), true).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(String::from_utf8_lossy(&body).contains(r#""isRoot":true"#));
}
