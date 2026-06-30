//! State service binding dispatch tests (story 08.04).

use std::fs;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_ext_keyval::SqlKeyValueProvider;
use edger_ext_turso::LocalSqliteProvider;
use edger_ext_turso_remote::RemoteTursoProvider;
use edger_isolation::{DenoFacade, DenoIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, AuthGate, AuthGateConfig, ExtensionRegistry,
    OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::Value;
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
    state_with_workers_and_registry(root, registry_with_state_providers())
}

fn state_with_workers_and_registry(
    root: std::path::PathBuf,
    registry: ExtensionRegistry,
) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(RuntimeFactory));
    server.mark_ready(pool.clone());

    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        registry,
        auth: AuthGate::new(
            AuthGateConfig::default(),
            Arc::new(AuthExtension::new(
                Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
                Some("test-root".into()),
            )),
        ),
    }
}

fn registry_with_state_providers() -> ExtensionRegistry {
    let sql_provider = Arc::new(LocalSqliteProvider::in_memory());
    let keyval_provider = Arc::new(SqlKeyValueProvider::new(sql_provider.clone()));
    let mut registry = ExtensionRegistry::new();
    registry
        .register_durable_sql_provider(sql_provider)
        .unwrap();
    registry
        .register_key_value_provider(keyval_provider.clone())
        .unwrap();
    registry.register_queue_provider(keyval_provider).unwrap();
    registry
}

fn registry_with_external_state_providers(root: &Path) -> ExtensionRegistry {
    let sql_provider = Arc::new(
        RemoteTursoProvider::new_local_for_tests(vec![(
            "@team".to_string(),
            root.join("team-state.db"),
        )])
        .unwrap(),
    );
    let keyval_provider = Arc::new(SqlKeyValueProvider::new(sql_provider.clone()));
    let mut registry = ExtensionRegistry::new();
    registry
        .register_durable_sql_provider(sql_provider)
        .unwrap();
    registry
        .register_key_value_provider(keyval_provider.clone())
        .unwrap();
    registry.register_queue_provider(keyval_provider).unwrap();
    registry
}

async fn dispatch(app: Router, uri: &str, authenticated: bool) -> (StatusCode, bytes::Bytes) {
    let mut request = Request::builder().method("GET").uri(uri);
    if authenticated {
        request = request.header("authorization", "Bearer test-root");
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

fn write_header_echo_worker(root: &std::path::Path, dir: &str, manifest: &str) {
    let worker_dir = root.join(dir);
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(worker_dir.join("manifest.yaml"), manifest).unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r#"Deno.serve((req: Request) => {
  return new Response(req.headers.get("x-edger-bindings") ?? "null", {
    headers: { "content-type": "application/json" },
  });
});
"#,
    )
    .unwrap();
}

#[tokio::test]
async fn worker_receives_service_binding_descriptors() {
    let root = tempfile::tempdir().unwrap();
    write_header_echo_worker(
        root.path(),
        "team-state-demo",
        r#"name: "@team/state-demo"
version: "1.0.0"
entrypoint: index.ts
kind: fetch
bindings:
  - kind: durableSql
    name: db
    namespace: "@team"
    permissions:
      - sql:read
      - sql:write
  - kind: keyValue
    name: cache
  - kind: queue
    name: jobs
"#,
    );

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (status, body) = dispatch(app, "/@team/state-demo", true).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["worker"], "@team/state-demo");
    assert_eq!(value["bindings"].as_array().unwrap().len(), 3);
    assert_eq!(value["bindings"][0]["name"], "db");
    assert_eq!(value["bindings"][0]["namespace"], "@team");
    assert_eq!(value["bindings"][1]["kind"], "keyValue");
    assert_eq!(value["bindings"][1]["namespace"], "@team");
    assert_eq!(value["bindings"][2]["kind"], "queue");
}

#[test]
fn worker_receives_service_binding_descriptors_with_external_durable_sql_provider() {
    let workers = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    write_header_echo_worker(
        workers.path(),
        "team-state-demo",
        r#"name: "@team/state-demo"
version: "1.0.0"
entrypoint: index.ts
kind: fetch
bindings:
  - kind: durableSql
    name: db
    namespace: "@team"
    permissions:
      - sql:read
      - sql:write
  - kind: keyValue
    name: cache
  - kind: queue
    name: jobs
"#,
    );

    let app = build_pipeline(state_with_workers_and_registry(
        workers.path().to_path_buf(),
        registry_with_external_state_providers(state.path()),
    ));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (status, body) =
        runtime.block_on(async { dispatch(app.clone(), "/@team/state-demo", true).await });

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["worker"], "@team/state-demo");
    assert_eq!(value["bindings"].as_array().unwrap().len(), 3);
    assert_eq!(value["bindings"][0]["kind"], "durableSql");
    assert_eq!(value["bindings"][0]["name"], "db");
    assert_eq!(value["bindings"][0]["namespace"], "@team");
    assert_eq!(value["bindings"][1]["kind"], "keyValue");
    assert_eq!(value["bindings"][1]["namespace"], "@team");
    assert_eq!(value["bindings"][2]["kind"], "queue");
    assert_eq!(value["bindings"][2]["namespace"], "@team");
}

#[tokio::test]
async fn worker_without_bindings_receives_no_binding_header() {
    let root = tempfile::tempdir().unwrap();
    write_header_echo_worker(
        root.path(),
        "plain",
        r#"name: plain
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
    );

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (status, body) = dispatch(app, "/plain", true).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"null");
}

#[tokio::test]
async fn public_worker_with_bindings_is_forbidden_before_dispatch() {
    let root = tempfile::tempdir().unwrap();
    write_header_echo_worker(
        root.path(),
        "public-state",
        r#"name: public-state
version: "1.0.0"
entrypoint: index.ts
kind: fetch
visibility: public
bindings:
  - kind: queue
    name: jobs
"#,
    );

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (status, body) = dispatch(app, "/public-state", false).await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
}
