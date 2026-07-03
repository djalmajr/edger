//! End-to-end pipeline tests (story 05.03 / 06.02).

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use edger_core::WorkerManifest;
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, ControlAuth, ExtensionRegistry, ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn orchestrator_with_worker() -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/demo"),
            WorkerManifest {
                name: "demo".into(),
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
        registry: ExtensionRegistry::new(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

#[tokio::test]
async fn pipeline_worker_fetch_returns_mock_body() {
    let app = build_pipeline(orchestrator_with_worker());
    let res = app
        .oneshot(
            Request::builder()
                .uri("/demo")
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8(body.to_vec())
        .unwrap()
        .contains("fetch:GET /"));
}

#[tokio::test]
async fn pipeline_api_reserved_does_not_invoke_worker() {
    let app = build_pipeline(orchestrator_with_worker());
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/keys")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["code"], "API_STUB");
}
