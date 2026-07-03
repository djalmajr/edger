use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerRef,
    MAX_HEADERS, MAX_HEADER_VALUE_BYTES,
};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
    MAX_BODY_BYTES,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

#[derive(Clone)]
struct CountingFactory {
    created: Arc<AtomicUsize>,
}

impl IsolateFactory for CountingFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        self.created.fetch_add(1, Ordering::SeqCst);
        Box::new(CountingIsolate)
    }
}

struct CountingIsolate;

#[async_trait]
impl Isolate for CountingIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: Some(Bytes::from_static(b"ok")),
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(
            SerializedRequest {
                method: "GET".into(),
                uri: "/".into(),
                headers: vec![],
                body: None,
                request_id: "spa".into(),
                base_href: None,
            },
            _config,
        )
        .await
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

fn test_app(created: Arc<AtomicUsize>) -> axum::Router {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("limited");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: limited
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        "Deno.serve(() => new Response('ok'));",
    )
    .unwrap();

    let server = ServerState::new_unready();
    let pool =
        WorkerPool::with_factory(PoolConfig::default(), Arc::new(CountingFactory { created }));
    server.mark_ready(pool.clone());
    let root_path = root.keep();

    build_pipeline(OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root_path]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    })
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn oversized_body_returns_413_without_worker_dispatch() {
    let created = Arc::new(AtomicUsize::new(0));
    let app = test_app(created.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/limited")
                .header("authorization", "Bearer test-root")
                .body(Body::from(vec![b'x'; MAX_BODY_BYTES + 1]))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_text(response).await;
    assert!(body.contains("PAYLOAD_TOO_LARGE"), "{body}");
    assert_eq!(created.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn too_many_headers_returns_431_without_worker_dispatch() {
    let created = Arc::new(AtomicUsize::new(0));
    let app = test_app(created.clone());
    let mut request = Request::builder()
        .uri("/limited")
        .header("authorization", "Bearer test-root");
    for i in 0..=MAX_HEADERS {
        request = request.header(format!("x-limit-{i}"), "v");
    }

    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
    );
    let body = body_text(response).await;
    assert!(body.contains("HEADER_TOO_LARGE"), "{body}");
    assert_eq!(created.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn oversized_header_value_returns_431_without_worker_dispatch() {
    let created = Arc::new(AtomicUsize::new(0));
    let app = test_app(created.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/limited")
                .header("authorization", "Bearer test-root")
                .header("x-large", "x".repeat(MAX_HEADER_VALUE_BYTES + 1))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
    );
    let body = body_text(response).await;
    assert!(body.contains("HEADER_TOO_LARGE"), "{body}");
    assert_eq!(created.load(Ordering::SeqCst), 0);
}
