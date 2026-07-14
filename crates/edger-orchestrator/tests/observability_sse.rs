use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use edger_core::WorkerManifest;
use edger_isolation::MockIsolate;
use edger_orchestrator::observability::{
    OperationalEventInput, OperationalEventLevel, OperationalEventSource,
};
use edger_orchestrator::{
    build_pipeline, ControlAuth, ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use futures_util::StreamExt;
use tower::ServiceExt;

const ROOT_KEY: &str = "test-root";

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn state() -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/alpha"),
            WorkerManifest {
                name: "alpha".into(),
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
        auth: ControlAuth::with_static_key(ROOT_KEY),
    }
}

fn record(state: &OrchestratorState, request_id: &str) {
    state
        .server
        .operational_events()
        .record(OperationalEventInput {
            source: OperationalEventSource::Runtime,
            kind: "dispatch".into(),
            level: OperationalEventLevel::Info,
            namespace: None,
            worker: Some("alpha".into()),
            version: Some("1.0.0".into()),
            process_id: None,
            request_id: Some(request_id.into()),
            trace_id: None,
            outcome: Some("ok".into()),
            status: Some(200),
            duration_ms: Some(1),
            code: None,
            message: None,
            truncated: None,
            dropped_count: None,
        });
}

#[tokio::test]
async fn stream_is_root_only_resumes_after_cursor_and_keeps_filters() {
    let state = state();
    record(&state, "request-1");
    record(&state, "request-2");
    let app = build_pipeline(state);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/observability/events/stream?worker=alpha")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/observability/events/stream?worker=alpha&version=1.0.0&cursor=1")
                .header("authorization", format!("Bearer {ROOT_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut stream = response.into_body().into_data_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("SSE event arrives")
        .expect("body remains open")
        .expect("valid chunk");
    let text = String::from_utf8_lossy(&chunk);
    assert!(text.contains("event: operational_event"), "{text}");
    assert!(text.contains("id: 2"), "{text}");
    assert!(text.contains("request-2"), "{text}");
    assert!(!text.contains("request-1"), "{text}");
}
