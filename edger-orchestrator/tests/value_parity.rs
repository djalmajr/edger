//! Value parity proofs for Epic 08.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use bytes::Bytes;
use edger_core::ExecutionKind;
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_ext_gateway::GatewayExtension;
use edger_ext_keyval::SqlKeyValueProvider;
use edger_ext_turso::LocalSqliteProvider;
use edger_isolation::{DenoFacade, DenoIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, collect_extensions, load_manifests_from_dirs, AuthGate, AuthGateConfig,
    ExtensionRegistry, OrchestratorState, ServerState,
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

struct ResponseParts {
    body: Bytes,
    headers: HeaderMap,
    status: StatusCode,
}

fn state_with_worker_dirs(
    worker_dirs: Vec<PathBuf>,
    registry: ExtensionRegistry,
) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(RuntimeFactory));
    server.mark_ready(pool.clone());

    let auth = Arc::new(AuthExtension::new(
        Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
        Some("test-root".into()),
    ));

    OrchestratorState {
        auth: AuthGate::new(AuthGateConfig::default(), auth),
        index: load_manifests_from_dirs(&worker_dirs).unwrap(),
        pool,
        registry,
        server,
    }
}

fn registry_with_gateway_and_state() -> ExtensionRegistry {
    let sql_provider = Arc::new(LocalSqliteProvider::in_memory());
    let keyval_provider = Arc::new(SqlKeyValueProvider::new(sql_provider.clone()));
    let mut registry = collect_extensions(vec![GatewayExtension::middleware()]).unwrap();
    registry
        .register_durable_sql_provider(sql_provider)
        .unwrap();
    registry
        .register_key_value_provider(keyval_provider.clone())
        .unwrap();
    registry.register_queue_provider(keyval_provider).unwrap();
    registry
}

fn app_with_worker_dirs(worker_dirs: Vec<PathBuf>, registry: ExtensionRegistry) -> Router {
    build_pipeline(state_with_worker_dirs(worker_dirs, registry))
}

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    authenticated: bool,
    headers: &[(&str, &str)],
    body: impl Into<Body>,
) -> ResponseParts {
    let mut request = Request::builder().method(method).uri(uri);
    if authenticated {
        request = request.header("authorization", "Bearer test-root");
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }

    let response = app
        .oneshot(request.body(body.into()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    ResponseParts {
        body,
        headers,
        status,
    }
}

fn write_fetch_worker(root: &Path, dir: &str, manifest: &str, source: &str) {
    let worker_dir = root.join(dir);
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(worker_dir.join("manifest.yaml"), manifest).unwrap();
    fs::write(worker_dir.join("index.ts"), source).unwrap();
}

fn write_shell_fixture(root: &Path) {
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
        r#"<!doctype html><html><body><main>shell-demo</main><iframe src="/todos-shell-demo" title="todos-shell-demo"></iframe></body></html>"#,
    )
    .unwrap();
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[tokio::test]
async fn todo_spa_fixture_serves_document_asset_and_fallback() {
    // Mutation captured: treating a Static SPA as a plain fetch worker breaks
    // deep links and asset serving for migrated TodoMVC-style apps.
    let todos_dir = repo_root().join("workers/value-parity/todos");
    let app = app_with_worker_dirs(vec![todos_dir], registry_with_gateway_and_state());

    let document = send(app.clone(), "GET", "/todos", false, &[], Body::empty()).await;
    assert_eq!(
        document.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&document.body)
    );
    let html = String::from_utf8_lossy(&document.body);
    assert!(html.contains("edger-value-parity-todos"));
    assert!(html.contains(r#"<base href="/todos/" />"#));

    let asset = send(
        app.clone(),
        "GET",
        "/todos/app.js",
        false,
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(
        asset.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&asset.body)
    );
    assert!(String::from_utf8_lossy(&asset.body).contains("valueParityTodosReady"));

    let fallback = send(
        app,
        "GET",
        "/todos/filter/active",
        false,
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(
        fallback.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&fallback.body)
    );
    assert!(String::from_utf8_lossy(&fallback.body).contains("edger-value-parity-todos"));
}

#[tokio::test]
async fn protected_worker_denies_without_auth_and_runs_with_root_key() {
    // Mutation captured: accidentally marking protected workers as public would
    // make the unauthorized request succeed.
    let root = tempfile::tempdir().unwrap();
    write_fetch_worker(
        root.path(),
        "protected-api",
        r#"name: protected-api
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
        r#"Deno.serve((req: Request) => {
  const url = new URL(req.url);
  return new Response(JSON.stringify({
    base: req.headers.get("x-base"),
    path: url.pathname,
    requestId: req.headers.get("x-request-id")
  }), { headers: { "content-type": "application/json" } });
});
"#,
    );

    let app = app_with_worker_dirs(vec![root.path().to_path_buf()], ExtensionRegistry::new());
    let denied = send(
        app.clone(),
        "GET",
        "/protected-api/check",
        false,
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);

    let allowed = send(
        app,
        "GET",
        "/protected-api/check",
        true,
        &[("x-request-id", "vp-auth")],
        Body::empty(),
    )
    .await;
    assert_eq!(
        allowed.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&allowed.body)
    );
    let body: Value = serde_json::from_slice(&allowed.body).unwrap();
    assert_eq!(body["path"], "/check");
    assert_eq!(body["base"], "/protected-api");
    assert_eq!(body["requestId"], "vp-auth");
}

#[tokio::test]
async fn vhost_routes_host_to_namespaced_worker_without_hijacking_reserved_paths() {
    // Mutation captured: running shell/path routing before vhost resolution would
    // send host-bound document navigations to the wrong app.
    let root = tempfile::tempdir().unwrap();
    write_fetch_worker(
        root.path(),
        "hosted-app",
        r#"name: "@acme/hosted-app"
version: "1.0.0"
entrypoint: index.ts
kind: fetch
hosts:
  - App.Example.test
"#,
        r#"Deno.serve((req: Request) => {
  const url = new URL(req.url);
  return new Response(JSON.stringify({
    host: req.headers.get("host"),
    path: url.pathname,
    base: req.headers.get("x-base")
  }), { headers: { "content-type": "application/json" } });
});
"#,
    );

    let app = app_with_worker_dirs(vec![root.path().to_path_buf()], ExtensionRegistry::new());
    let hosted = send(
        app.clone(),
        "GET",
        "/dashboard",
        true,
        &[("host", "app.example.test:19080")],
        Body::empty(),
    )
    .await;
    assert_eq!(
        hosted.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&hosted.body)
    );
    let body: Value = serde_json::from_slice(&hosted.body).unwrap();
    assert_eq!(body["host"], "app.example.test:19080");
    assert_eq!(body["path"], "/dashboard");
    assert_eq!(body["base"], "/");

    let unknown = send(
        app.clone(),
        "GET",
        "/",
        true,
        &[("host", "unknown.example.test")],
        Body::empty(),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    let reserved = send(
        app,
        "GET",
        "/api/not-configured",
        true,
        &[("host", "app.example.test")],
        Body::empty(),
    )
    .await;
    assert_eq!(reserved.status, StatusCode::NOT_FOUND);
    let reserved_body = String::from_utf8_lossy(&reserved.body);
    assert!(reserved_body.contains("API_STUB"));
}

#[tokio::test]
async fn stateful_app_receives_sql_kv_and_queue_bindings() {
    // Mutation captured: resolving bindings from a static registry instead of
    // the authenticated worker context would drop namespace-scoped descriptors.
    let root = tempfile::tempdir().unwrap();
    write_fetch_worker(
        root.path(),
        "state-app",
        r#"name: "@acme/state-app"
version: "1.0.0"
entrypoint: index.ts
kind: fetch
bindings:
  - kind: durableSql
    name: db
    namespace: "@acme"
    permissions:
      - sql:read
      - sql:write
  - kind: keyValue
    name: cache
  - kind: queue
    name: jobs
"#,
        r#"Deno.serve((req: Request) => {
  return new Response(req.headers.get("x-edger-bindings") ?? "null", {
    headers: { "content-type": "application/json" },
  });
});
"#,
    );

    let app = app_with_worker_dirs(
        vec![root.path().to_path_buf()],
        registry_with_gateway_and_state(),
    );
    let response = send(app, "GET", "/@acme/state-app", true, &[], Body::empty()).await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&response.body)
    );
    let value: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(value["worker"], "@acme/state-app");
    assert_eq!(value["bindings"].as_array().unwrap().len(), 3);
    assert_eq!(value["bindings"][0]["kind"], "durableSql");
    assert_eq!(value["bindings"][0]["namespace"], "@acme");
    assert_eq!(value["bindings"][1]["kind"], "keyValue");
    assert_eq!(value["bindings"][1]["namespace"], "@acme");
    assert_eq!(value["bindings"][2]["kind"], "queue");
}

#[tokio::test]
async fn shell_gateway_composes_document_iframe_and_admin_paths() {
    // Mutation captured: routing every document request to the shell would break
    // iframe app dispatch and reserved admin paths.
    let root = tempfile::tempdir().unwrap();
    write_shell_fixture(root.path());
    write_fetch_worker(
        root.path(),
        "todos-shell-demo",
        r#"name: todos-shell-demo
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
        r#"Deno.serve((req: Request) => {
  const url = new URL(req.url);
  return new Response(`${url.pathname} base=${req.headers.get("x-base")}`);
});
"#,
    );

    let app = app_with_worker_dirs(
        vec![root.path().to_path_buf()],
        registry_with_gateway_and_state(),
    );
    let shell = send(
        app.clone(),
        "GET",
        "/reports/list",
        true,
        &[("sec-fetch-dest", "document")],
        Body::empty(),
    )
    .await;
    assert_eq!(
        shell.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&shell.body)
    );
    let shell_html = String::from_utf8_lossy(&shell.body);
    assert!(shell_html.contains("shell-demo"));
    assert!(shell_html.contains(r#"<base href="/" />"#));

    let iframe = send(
        app.clone(),
        "GET",
        "/todos-shell-demo/list",
        true,
        &[("sec-fetch-dest", "iframe")],
        Body::empty(),
    )
    .await;
    assert_eq!(
        iframe.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&iframe.body)
    );
    assert_eq!(iframe.body.as_ref(), b"/list base=/todos-shell-demo");
    assert_eq!(
        iframe
            .headers
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("*")
    );

    let admin = send(
        app,
        "GET",
        "/api/admin/session",
        true,
        &[("sec-fetch-dest", "document")],
        Body::empty(),
    )
    .await;
    assert_eq!(
        admin.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&admin.body)
    );
    assert!(String::from_utf8_lossy(&admin.body).contains(r#""isRoot":true"#));
}

#[tokio::test]
async fn gateway_cors_preflight_does_not_bypass_auth() {
    // Mutation captured: running gateway hooks before auth would let a protected
    // worker's preflight become reachable without the runtime credential.
    let root = tempfile::tempdir().unwrap();
    write_fetch_worker(
        root.path(),
        "cors-api",
        r#"name: cors-api
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
        r#"Deno.serve(() => new Response("cors-api"));
"#,
    );

    let app = app_with_worker_dirs(
        vec![root.path().to_path_buf()],
        registry_with_gateway_and_state(),
    );
    let denied = send(
        app.clone(),
        "OPTIONS",
        "/cors-api",
        false,
        &[
            ("origin", "https://app.example.com"),
            ("access-control-request-headers", "x-demo"),
        ],
        Body::empty(),
    )
    .await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);

    let allowed = send(
        app,
        "OPTIONS",
        "/cors-api",
        true,
        &[
            ("origin", "https://app.example.com"),
            ("access-control-request-headers", "x-demo"),
        ],
        Body::empty(),
    )
    .await;
    assert_eq!(
        allowed.status,
        StatusCode::NO_CONTENT,
        "unexpected body: {}",
        String::from_utf8_lossy(&allowed.body)
    );
    assert_eq!(
        allowed
            .headers
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("*")
    );
    assert_eq!(
        allowed
            .headers
            .get("access-control-allow-headers")
            .and_then(|value| value.to_str().ok()),
        Some("x-demo")
    );
}

#[tokio::test]
async fn empty_plugin_base_does_not_become_a_shell_or_shadow_workers() {
    // Mutation captured: normalizing `base: ""` to `/` repeats the Buntime
    // pure-plugin bug where document navigations are served by the wrong app.
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("pure-plugin")).unwrap();
    fs::write(
        root.path().join("pure-plugin/manifest.yaml"),
        r#"name: pure-plugin
version: "1.0.0"
entrypoint: index.html
base: ""
"#,
    )
    .unwrap();
    fs::write(
        root.path().join("pure-plugin/index.html"),
        "<!doctype html><p>wrong shell</p>",
    )
    .unwrap();
    write_fetch_worker(
        root.path(),
        "target",
        r#"name: target
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
        r#"Deno.serve((req: Request) => {
  const url = new URL(req.url);
  return new Response(`target ${url.pathname}`);
});
"#,
    );

    let app = app_with_worker_dirs(vec![root.path().to_path_buf()], ExtensionRegistry::new());
    let response = send(
        app,
        "GET",
        "/target/deep-link",
        true,
        &[("sec-fetch-dest", "document")],
        Body::empty(),
    )
    .await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(response.body.as_ref(), b"target /deep-link");
}
