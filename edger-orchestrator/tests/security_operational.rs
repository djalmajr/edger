//! Operational security integration tests (Story 08.03).

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use edger_core::{WorkerManifest, MAX_HEADERS};
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, ControlAuth, ManifestIndex, OrchestratorState, ServerState, MAX_BODY_BYTES,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct CapturedLogs {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8(self.buffer.lock().expect("log buffer").clone()).expect("utf8 logs")
    }
}

struct CapturedLogWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl Write for CapturedLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer
            .lock()
            .expect("log buffer")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CapturedLogWriter {
            buffer: self.buffer.clone(),
        }
    }
}

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn test_state() -> OrchestratorState {
    let mut index = ManifestIndex::new();
    for (dir, name) in [
        ("/workers/acme-api", "@acme/api"),
        ("/workers/other-api", "@other/api"),
        ("/workers/todos", "todos"),
    ] {
        index
            .insert(
                PathBuf::from(dir),
                WorkerManifest {
                    name: name.into(),
                    version: Some("1.0.0".into()),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());

    OrchestratorState {
        server,
        pool,
        index,
        auth: ControlAuth::with_static_key("root-secret"),
    }
}

async fn request(builder: axum::http::request::Builder, body: Body) -> axum::response::Response {
    build_pipeline(test_state())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

async fn response_json(res: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn root_worker_inventory_lists_all_workers() {
    let res = request(
        Request::builder()
            .uri("/api/admin/workers")
            .header("authorization", "Bearer root-secret"),
        Body::empty(),
    )
    .await;
    let (status, body) = response_json(res).await;

    assert_eq!(status, StatusCode::OK);
    let names = body["workers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|worker| worker["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["@acme/api", "@other/api", "todos"]);
}

#[tokio::test]
async fn worker_inventory_rejects_invalid_root_key() {
    let res = request(
        Request::builder()
            .uri("/api/admin/workers")
            .header("authorization", "Bearer wrong"),
        Body::empty(),
    )
    .await;
    let (status, body) = response_json(res).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn missing_root_key_cannot_mutate_worker_state() {
    let res = request(
        Request::builder()
            .method("POST")
            .uri("/api/admin/workers/todos/disable"),
        Body::empty(),
    )
    .await;
    let (status, body) = response_json(res).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn admin_mutation_rejects_browser_request_without_same_origin() {
    let missing_origin = request(
        Request::builder()
            .method("POST")
            .uri("/api/admin/workers/todos/disable")
            .header("authorization", "Bearer root-secret")
            .header("host", "edger.local")
            .header("sec-fetch-mode", "cors"),
        Body::empty(),
    )
    .await;
    let (status, body) = response_json(missing_origin).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "CSRF_DENIED");

    let mismatched_origin = request(
        Request::builder()
            .method("POST")
            .uri("/api/admin/workers/todos/disable")
            .header("authorization", "Bearer root-secret")
            .header("host", "edger.local")
            .header("origin", "https://evil.local"),
        Body::empty(),
    )
    .await;
    let (status, body) = response_json(mismatched_origin).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "CSRF_DENIED");
}

#[tokio::test]
async fn admin_mutation_allows_same_origin_and_authenticated_internal_bypass() {
    let same_origin = request(
        Request::builder()
            .method("POST")
            .uri("/api/admin/workers/todos/disable")
            .header("authorization", "Bearer root-secret")
            .header("host", "edger.local")
            .header("origin", "https://edger.local"),
        Body::empty(),
    )
    .await;
    let (status, body) = response_json(same_origin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], "OK");
    assert_eq!(body["status"], "disabled");

    let internal = request(
        Request::builder()
            .method("POST")
            .uri("/api/admin/workers/todos/disable")
            .header("authorization", "Bearer root-secret")
            .header("sec-fetch-mode", "cors")
            .header("x-edger-internal", "true"),
        Body::empty(),
    )
    .await;
    let (status, body) = response_json(internal).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], "OK");
    assert_eq!(body["status"], "disabled");
}

#[tokio::test]
async fn internal_header_does_not_authenticate_public_requests() {
    let res = request(
        Request::builder()
            .method("POST")
            .uri("/api/admin/workers/todos/disable")
            .header("sec-fetch-mode", "cors")
            .header("x-edger-internal", "true"),
        Body::empty(),
    )
    .await;
    let (status, body) = response_json(res).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn internal_header_does_not_elevate_invalid_keys() {
    let res = request(
        Request::builder()
            .method("POST")
            .uri("/api/admin/workers/todos/disable")
            .header("authorization", "Bearer wrong")
            .header("x-edger-internal", "true"),
        Body::empty(),
    )
    .await;
    let (status, body) = response_json(res).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn admin_errors_preserve_request_id() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(true)
        .without_time()
        .with_writer(logs.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let res = request(
        Request::builder()
            .uri("/api/admin/workers")
            .header("authorization", "Bearer should-not-leak")
            .header("x-request-id", "trace-08-03"),
        Body::from("body-should-not-leak"),
    )
    .await;
    drop(guard);

    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("trace-08-03")
    );

    let text = logs.text();
    assert!(text.contains("edger.operational"), "logs:\n{text}");
    assert!(text.contains("surface=\"admin_api\""), "logs:\n{text}");
    assert!(text.contains("request_id=\"trace-08-03\""), "logs:\n{text}");
    assert!(text.contains("status=401"), "logs:\n{text}");
    assert!(text.contains("code=UNAUTHORIZED"), "logs:\n{text}");
    assert!(!text.contains("authorization"), "logs:\n{text}");
    assert!(!text.contains("should-not-leak"), "logs:\n{text}");
    assert!(!text.contains("body-should-not-leak"), "logs:\n{text}");
    assert!(
        !text.contains("missing or invalid API key"),
        "logs:\n{text}"
    );
}

#[tokio::test]
async fn ingress_limits_return_typed_errors_before_worker_dispatch() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(true)
        .without_time()
        .with_writer(logs.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let mut oversized_body = vec![b'x'; MAX_BODY_BYTES + 1];
    oversized_body[..17].copy_from_slice(b"limit-body-secret");
    let body_too_large = request(
        Request::builder()
            .method("POST")
            .uri("/@acme/api")
            .header("authorization", "Bearer root-secret")
            .header("x-request-id", "trace-limit")
            .header("x-test-token", "limit-secret"),
        Body::from(oversized_body),
    )
    .await;
    drop(guard);
    let (status, body) = response_json(body_too_large).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["code"], "PAYLOAD_TOO_LARGE");

    let text = logs.text();
    assert!(text.contains("edger.operational"), "logs:\n{text}");
    assert!(text.contains("surface=\"pipeline\""), "logs:\n{text}");
    assert!(text.contains("request_id=\"trace-limit\""), "logs:\n{text}");
    assert!(text.contains("status=413"), "logs:\n{text}");
    assert!(text.contains("code=PAYLOAD_TOO_LARGE"), "logs:\n{text}");
    assert!(!text.contains("authorization"), "logs:\n{text}");
    assert!(!text.contains("root-secret"), "logs:\n{text}");
    assert!(!text.contains("limit-secret"), "logs:\n{text}");
    assert!(!text.contains("limit-body-secret"), "logs:\n{text}");

    let mut builder = Request::builder()
        .uri("/@acme/api")
        .header("authorization", "Bearer root-secret");
    for index in 0..=MAX_HEADERS {
        builder = builder.header(format!("x-extra-{index}"), "v");
    }
    let too_many_headers = request(builder, Body::empty()).await;
    let (status, body) = response_json(too_many_headers).await;
    assert_eq!(status, StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    assert_eq!(body["code"], "HEADER_TOO_LARGE");
}
