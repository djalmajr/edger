//! HTTP contract for root-controlled routing policies.

use std::fs;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use edger_core::{root_principal, CreateApiKeyRequest};
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, load_manifests_from_roots, ApiKeyService, ControlAuth, OrchestratorState,
    ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::Value;
use tower::ServiceExt;

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn build_app(root: &std::path::Path) -> axum::Router {
    build_app_with_routing_flags(root, false, false)
}

fn build_app_with_routing_flags(
    root: &std::path::Path,
    tenant_routing_enabled: bool,
    weighted_routing_enabled: bool,
) -> axum::Router {
    let dir = root.join("app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: '@acme/app'\nversion: '1.0.0'\nentrypoint: index.html\nkind: static\n",
    )
    .unwrap();
    fs::write(dir.join("index.html"), "test").unwrap();
    let index = load_manifests_from_roots(&[], None, &[root.to_path_buf()]).unwrap();
    let server = ServerState::new_unready();
    if tenant_routing_enabled {
        server.enable_tenant_routing();
    }
    if weighted_routing_enabled {
        server.enable_weighted_routing();
    }
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    build_pipeline(OrchestratorState {
        server,
        pool,
        index,
        auth: ControlAuth::with_static_key("test-root"),
    })
}

#[tokio::test]
async fn session_reports_effective_routing_flags() {
    let root = tempfile::tempdir().unwrap();
    let app = build_app(root.path());
    let (status, json) = send(app.clone(), "GET", "/api/admin/session", true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(json.get("principal").is_some());
    assert_eq!(json["tenantRoutingEnabled"], false);
    assert_eq!(json["weightedRoutingEnabled"], false);
    let (status, _) = send(app, "GET", "/api/admin/session", false, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let root = tempfile::tempdir().unwrap();
    let app = build_app_with_routing_flags(root.path(), true, false);
    let (status, json) = send(app, "GET", "/api/admin/session", true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["tenantRoutingEnabled"], true);
    assert_eq!(json["weightedRoutingEnabled"], false);

    let root = tempfile::tempdir().unwrap();
    let app = build_app_with_routing_flags(root.path(), false, true);
    let (status, json) = send(app, "GET", "/api/admin/session", true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["tenantRoutingEnabled"], false);
    assert_eq!(json["weightedRoutingEnabled"], true);
}

async fn send(
    app: axum::Router,
    method: &str,
    uri: &str,
    root: bool,
    body: &str,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if root {
        request = request.header("authorization", "Bearer test-root");
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

const URI: &str = "/api/admin/routing-policy?name=%40acme%2Fapp";
const OTHER_URI: &str = "/api/admin/routing-policy?name=other";
const POLICY: &str =
    r#"{"name":"@acme/app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#;

#[tokio::test]
async fn root_can_store_read_and_delete_a_namespaced_policy() {
    let root = tempfile::tempdir().unwrap();
    let app = build_app(root.path());
    let (status, json) = send(app.clone(), "GET", URI, true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(json["policy"].is_null());

    let (status, json) = send(app.clone(), "PUT", URI, true, POLICY).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["policy"]["tenantAccess"]["tenants"][0], "acme");

    let (status, json) = send(app.clone(), "GET", URI, true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["policy"]["name"], "@acme/app");

    let restarted = build_app(root.path());
    let (status, json) = send(restarted.clone(), "GET", URI, true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["policy"]["tenantAccess"]["tenants"][0], "acme");

    let (status, json) = send(restarted.clone(), "DELETE", URI, true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["deleted"], true);
    let (status, json) = send(restarted, "GET", URI, true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(json["policy"].is_null());
}

#[tokio::test]
async fn rejects_unauthorized_or_invalid_changes_without_losing_previous_policy() {
    let root = tempfile::tempdir().unwrap();
    let app = build_app(root.path());
    let (status, _) = send(app.clone(), "PUT", URI, false, POLICY).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(app.clone(), "GET", URI, false, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, json) = send(app.clone(), "PUT", URI, true, POLICY).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let mismatch = r#"{"name":"other","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#;
    let (status, json) = send(app.clone(), "PUT", URI, true, mismatch).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    let (status, json) = send(app.clone(), "PUT", URI, true, "{").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    let (status, json) = send(app, "GET", URI, true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["policy"]["tenantAccess"]["tenants"][0], "acme");
}

#[tokio::test]
async fn browser_cross_origin_cannot_mutate_a_routing_policy() {
    let root = tempfile::tempdir().unwrap();
    let app = build_app(root.path());
    let request = Request::builder()
        .method("PUT")
        .uri(URI)
        .header("authorization", "Bearer test-root")
        .header("host", "edger.example.com")
        .header("origin", "https://other.example.com")
        .body(Body::from(POLICY))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let (status, json) = send(app, "GET", URI, true, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(json["policy"].is_null());
}

fn build_app_with_worker_scoped_key(root: &std::path::Path) -> (axum::Router, String) {
    let dir = root.join("app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: '@acme/app'\nversion: '1.0.0'\nentrypoint: index.html\nkind: static\n",
    )
    .unwrap();
    fs::write(dir.join("index.html"), "test").unwrap();
    let other = root.join("other");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join("manifest.yaml"),
        "name: 'other'\nversion: '1.0.0'\nentrypoint: index.html\nkind: static\n",
    )
    .unwrap();
    fs::write(other.join("index.html"), "test").unwrap();
    let index = load_manifests_from_roots(&[], None, &[root.to_path_buf()]).unwrap();
    let keys = Arc::new(ApiKeyService::in_memory().unwrap());
    let created = keys
        .create(
            &root_principal(),
            CreateApiKeyRequest {
                name: "other-reader".into(),
                permissions: vec!["workers:read".into()],
                namespaces: vec!["*".into()],
                workers: vec!["other".into()],
                expires_at: None,
                role: None,
            },
        )
        .expect("scoped key");
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    let app = build_pipeline(OrchestratorState {
        server,
        pool,
        index,
        auth: ControlAuth::with_static_key("test-root").with_key_service(keys),
    });
    (app, created.raw_key)
}

async fn send_with_key(
    app: axum::Router,
    method: &str,
    uri: &str,
    key: &str,
    body: &str,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", format!("Bearer {key}"))
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn scoped_reader_cannot_see_routing_policy_of_another_app() {
    let root = tempfile::tempdir().unwrap();
    let (app, scoped_key) = build_app_with_worker_scoped_key(root.path());

    let (status, json) = send_with_key(app.clone(), "GET", OTHER_URI, &scoped_key, "").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(json["policy"].is_null());

    let (status, json) = send_with_key(app.clone(), "GET", URI, &scoped_key, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{json}");
    assert_eq!(json["code"], "NOT_FOUND");
    assert!(json.get("policy").is_none());

    let (status, json) = send(app.clone(), "PUT", URI, true, POLICY).await;
    assert_eq!(status, StatusCode::OK, "{json}");

    let (status, json) = send_with_key(app.clone(), "GET", URI, &scoped_key, "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{json}");
    assert_eq!(json["code"], "NOT_FOUND");
    assert!(json.get("policy").is_none());
}
