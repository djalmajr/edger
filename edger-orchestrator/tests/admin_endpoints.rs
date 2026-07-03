//! Admin endpoint contract coverage for surviving Epic 17 control-plane routes.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::WorkerManifest;
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, ControlAuth, ControlAuthConfig, ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::Value;
use tower::ServiceExt;

const ROOT_KEY: &str = "test-root";

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn state_with_auth(auth: ControlAuth) -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/hello"),
            WorkerManifest {
                name: "hello".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();

    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());

    OrchestratorState {
        server,
        pool,
        index,
        auth,
    }
}

fn root_state() -> OrchestratorState {
    state_with_auth(ControlAuth::with_static_key(ROOT_KEY))
}

fn open_state() -> OrchestratorState {
    state_with_auth(ControlAuth::new(ControlAuthConfig::default()))
}

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    api_key: Option<&str>,
    body: Body,
) -> (StatusCode, Value, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(key) = api_key {
        request = request.header("authorization", format!("Bearer {key}"));
    }

    let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

async fn send_with_origin(
    app: Router,
    method: &str,
    uri: &str,
    origin: &str,
) -> (StatusCode, Value, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {ROOT_KEY}"))
        .header("host", "edger.local")
        .header("origin", origin)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

// Mutation captured: treating missing/wrong credentials as root makes the
// 401 cases pass through, while making open mode require a key breaks the open
// 200 cases.
#[tokio::test]
async fn admin_auth_matrix_covers_read_and_mutation_routes() {
    for (method, uri) in [
        ("GET", "/api/admin/workers"),
        ("POST", "/api/admin/workers/hello/disable"),
    ] {
        let app = build_pipeline(root_state());

        let (status, json, _text) = send(app.clone(), method, uri, None, Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["code"], "UNAUTHORIZED");

        let (status, json, _text) =
            send(app.clone(), method, uri, Some("wrong"), Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["code"], "UNAUTHORIZED");

        let (status, _json, text) = send(app, method, uri, Some(ROOT_KEY), Body::empty()).await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {text}");

        let open_app = build_pipeline(open_state());
        let (status, _json, text) = send(open_app, method, uri, None, Body::empty()).await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    }
}

// Mutation captured: accidentally re-registering the removed keys API makes
// either route return a non-404 response.
#[tokio::test]
async fn removed_admin_keys_routes_stay_unregistered() {
    let app = build_pipeline(root_state());

    for method in ["GET", "POST"] {
        let (status, _json, text) = send(
            app.clone(),
            method,
            "/api/admin/keys",
            Some(ROOT_KEY),
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "unexpected body: {text}");
    }
}

// Mutation captured: changing open/static auth to return a non-root principal
// breaks the root role and wildcard namespace contract.
#[tokio::test]
async fn admin_session_returns_root_principal() {
    let app = build_pipeline(root_state());
    let (status, json, text) = send(
        app,
        "GET",
        "/api/admin/session",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["principal"]["name"], "root");
    assert_eq!(json["principal"]["role"], "admin");
    assert_eq!(json["principal"]["isRoot"], true);
    assert_eq!(json["principal"]["namespaces"], serde_json::json!(["*"]));
}

// Mutation captured: dropping worker catalog construction leaves the expected
// worker entry missing.
#[tokio::test]
async fn admin_catalog_returns_worker_entries() {
    let app = build_pipeline(root_state());

    let (status, catalog, text) = send(
        app.clone(),
        "GET",
        "/api/admin/catalog",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    let items = catalog["items"].as_array().expect("catalog items array");
    let worker = items
        .iter()
        .find(|item| item["id"] == "worker:hello")
        .expect("hello worker catalog entry");
    assert_eq!(worker["kind"], "worker");
    assert_eq!(worker["owner"], "hello");
    assert_eq!(worker["route"], "/hello");
    assert_eq!(worker["status"], "loaded");

    let (status, _json, _text) = send(
        app,
        "GET",
        "/api/admin/extensions",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// Mutation captured: toggling only the admin listing state without affecting
// route resolution would keep `/hello` serving after disable and this test
// goes red.
#[tokio::test]
async fn worker_disable_and_enable_controls_data_plane_route() {
    let app = build_pipeline(root_state());

    let (status, _json, text) = send(app.clone(), "GET", "/hello", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert!(text.contains("fetch:GET /"));

    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/hello/disable",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["status"], "disabled");

    let (status, _json, _text) = send(app.clone(), "GET", "/hello", None, Body::empty()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/hello/enable",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["status"], "loaded");

    let (status, _json, text) = send(app, "GET", "/hello", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert!(text.contains("fetch:GET /"));
}

// Mutation captured: removing the worker error log from either admin response
// leaves the per-worker errors array or summary object empty.
#[tokio::test]
async fn worker_error_endpoints_return_basic_shapes() {
    let state = root_state();
    state
        .server
        .worker_errors()
        .record("hello", "request-1", 502, "WORKER_ERROR", "boom");
    let app = build_pipeline(state);

    let (status, errors, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/hello/errors",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(errors["worker"], "hello");
    let entries = errors["errors"].as_array().expect("errors array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["code"], "WORKER_ERROR");
    assert_eq!(entries[0]["status"], 502);

    let (status, summary, text) = send(
        app,
        "GET",
        "/api/admin/workers/error-summary",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(summary["summary"]["hello"]["count"], 1);
    assert_eq!(
        summary["summary"]["hello"]["latest"]["code"],
        "WORKER_ERROR"
    );
}

// Mutation captured: skipping `validate_admin_mutation_security` allows the
// cross-site Origin mutation through as 200 instead of 403.
#[tokio::test]
async fn admin_mutation_rejects_cross_site_origin() {
    let app = build_pipeline(root_state());
    let (status, json, _text) = send_with_origin(
        app,
        "POST",
        "/api/admin/workers/hello/disable",
        "https://evil.local",
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "CSRF_DENIED");
}
