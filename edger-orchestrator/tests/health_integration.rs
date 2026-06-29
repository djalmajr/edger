//! Integration tests for HTTP health/readiness (story 05.01).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use edger_isolation::MockIsolate;
use edger_orchestrator::server::{router, ServerState};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct StubIsolateFactory;

impl IsolateFactory for StubIsolateFactory {
    fn create_isolate(&self) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn ready_pool() -> WorkerPool {
    WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubIsolateFactory))
}

#[tokio::test]
async fn health_returns_200_ok_json() {
    let app = router(ServerState::new_unready());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn ready_returns_503_before_init() {
    let app = router(ServerState::new_unready());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "not_ready");
}

#[tokio::test]
async fn ready_returns_200_after_mark_ready() {
    let state = ServerState::new_unready();
    state.mark_ready(ready_pool());
    let app = router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ready");
}

#[tokio::test]
async fn propagates_incoming_x_request_id() {
    let app = router(ServerState::new_unready());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("x-request-id", "trace-abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok()),
        Some("trace-abc")
    );
}

#[tokio::test]
async fn generates_x_request_id_when_missing() {
    let app = router(ServerState::new_unready());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(!id.is_empty());
}
