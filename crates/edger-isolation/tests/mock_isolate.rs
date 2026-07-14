//! Integration tests for MockIsolate (story 03.02) — written first (TDD red).

use edger_core::{Isolate, SerializedRequest, WorkerConfig};
use edger_isolation::{dispatch_execution, MockIsolate};

fn sample_req(uri: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: uri.into(),
        headers: vec![],
        body: None,
        request_id: "test-req".into(),
        base_href: None,
    }
}

fn default_config() -> WorkerConfig {
    edger_core::parse_worker_config(&edger_core::WorkerManifest {
        name: "mock-worker".into(),
        ..Default::default()
    })
}

#[tokio::test]
async fn mock_isolate_execute_fetch_returns_200() {
    let mut isolate = MockIsolate::new();
    let req = sample_req("/hello");
    let config = default_config();
    let res = isolate.execute_fetch(req, &config).await.unwrap();
    assert_eq!(res.status, 200);
    assert!(res.body.unwrap().starts_with(b"fetch:"));
}

#[tokio::test]
async fn mock_isolate_execute_routes_prefix() {
    let mut isolate = MockIsolate::new();
    let req = sample_req("/api/users");
    let config = default_config();
    let res = isolate.execute_routes(req, &config).await.unwrap();
    assert_eq!(res.status, 200);
    let body = String::from_utf8(res.body.unwrap().to_vec()).unwrap();
    assert!(body.contains("routes:GET /api/users"));
}

#[tokio::test]
async fn mock_isolate_static_spa_injects_base() {
    let mut isolate = MockIsolate::new().with_spa_html("<html><head></head><body>hi</body></html>");
    let config = default_config();
    let res = isolate
        .serve_static_spa("index.html", Some("/@app/"), &config)
        .await
        .unwrap();
    let body = String::from_utf8(res.body.unwrap().to_vec()).unwrap();
    assert!(body.contains(r#"<base href="/@app/""#));
}

#[tokio::test]
async fn mock_isolate_wasm_header() {
    let mut isolate = MockIsolate::new();
    let req = sample_req("/wasm");
    let config = default_config();
    let res = isolate.execute_wasm(req, &config).await.unwrap();
    assert_eq!(res.status, 200);
    assert!(res
        .headers
        .iter()
        .any(|(k, v)| k == "x-mock-wasm" && v == "1"));
}

#[tokio::test]
async fn dispatch_fullstack_uses_fetch_backend_with_restored_base() {
    let mut isolate = MockIsolate::new();
    let mut req = sample_req("/ssr");
    req.headers.push(("x-base".into(), "/mock-worker".into()));
    req.base_href = Some("/mock-worker/".into());
    let manifest = edger_core::WorkerManifest {
        name: "mock-worker".into(),
        adapter: Some("tanstack".into()),
        kind: Some("fullstack".into()),
        ssr_entrypoint: Some("server/server.js".into()),
        ..Default::default()
    };
    let config = edger_core::parse_worker_config(&manifest);
    let kind = config.kind.clone().unwrap();
    let res = dispatch_execution(&mut isolate, kind, req, &config)
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    let body = String::from_utf8(res.body.unwrap().to_vec()).unwrap();
    assert!(body.contains("fetch:GET /mock-worker/ssr"));
}

#[tokio::test]
async fn terminate_is_idempotent() {
    let mut isolate = MockIsolate::new();
    isolate.terminate().await.unwrap();
    isolate.terminate().await.unwrap();
    assert_eq!(isolate.terminate_count(), 2);
}

#[tokio::test]
async fn notify_idle_increments_counter() {
    let mut isolate = MockIsolate::new();
    isolate.notify_idle().await.unwrap();
    isolate.notify_idle().await.unwrap();
    assert_eq!(isolate.idle_count(), 2);
}
