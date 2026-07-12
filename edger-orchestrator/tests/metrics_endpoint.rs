//! Metrics and operational probe integration tests (Story 08.07).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::{io, io::Write};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerManifest,
};
use edger_orchestrator::{
    build_pipeline, ControlAuth, ManifestIndex, OrchestratorState, ServerState,
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

struct RequestIdEchoFactory;

impl IsolateFactory for RequestIdEchoFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(RequestIdEchoIsolate)
    }
}

struct RequestIdEchoIsolate;

#[async_trait::async_trait]
impl Isolate for RequestIdEchoIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let header_request_id = req
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-request-id"))
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        Ok(SerializedResponse {
            body: Some(Bytes::from(format!(
                "field={} header={}",
                req.request_id, header_request_id
            ))),
            headers: vec![("content-type".into(), "text/plain".into())],
            status: 200,
        })
    }

    async fn execute_routes(
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
        Ok(SerializedResponse {
            body: Some(Bytes::from_static(b"spa")),
            headers: vec![],
            status: 200,
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }
}

fn test_state() -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/echo"),
            WorkerManifest {
                name: "echo".into(),
                version: Some("1.0.0".into()),
                ttl: Some(serde_yaml::Value::String("30s".into())),
                ..Default::default()
            },
        )
        .unwrap();

    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(RequestIdEchoFactory));
    server.mark_ready(pool.clone());

    OrchestratorState {
        auth: ControlAuth::with_static_key("test-root"),
        index,
        pool,
        server,
    }
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn assert_worker_metric_labels_are_low_cardinality(body: &str) {
    for line in body
        .lines()
        .filter(|line| line.starts_with("edger_worker_"))
    {
        let Some((_, rest)) = line.split_once('{') else {
            continue;
        };
        let Some((labels, _)) = rest.split_once('}') else {
            panic!("worker metric line has no closing label set: {line}");
        };
        for label in labels.split(',').filter_map(|label| label.split_once('=')) {
            assert!(
                matches!(
                    label.0,
                    "worker" | "version" | "namespace" | "state" | "cause"
                ),
                "unexpected high-cardinality worker metric label `{}` in line `{}`",
                label.0,
                line
            );
        }
    }
}

#[tokio::test]
async fn metrics_endpoint_is_prometheus_text_without_secrets() {
    // Mutation captured: accidentally serializing config/env into metrics would
    // leak credentials instead of exposing only numeric pool counters.
    let app = build_pipeline(test_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/plain; version=0.0.4; charset=utf-8")
    );
    let body = body_text(response).await;
    assert!(body.contains("# TYPE edger_pool_cache_hits_total counter"));
    assert!(body.contains("# TYPE edger_http_requests_total counter"));
    assert!(body.contains("edger_pool_workers 0"));
    assert!(!body.contains("test-root"));
    assert!(!body.to_ascii_lowercase().contains("authorization"));
    assert!(!body.to_ascii_lowercase().contains("request_id"));
    assert!(!body.to_ascii_lowercase().contains("requestid"));
    assert!(!body.to_ascii_lowercase().contains("path=\""));
}

#[tokio::test]
async fn metrics_reflect_worker_pool_cache_hit_after_dispatch() {
    // Mutation captured: wiring /metrics to a fresh collector instead of the
    // runtime pool would keep cache hits at zero after repeated dispatch.
    let app = build_pipeline(test_state());
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/echo")
                    .header("authorization", "Bearer test-root")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_text(response).await;

    assert!(body.contains("edger_pool_cache_hits_total 1"));
    assert!(body.contains("edger_pool_cache_misses_total 1"));
    assert!(
        body.contains(
            "edger_worker_processes{worker=\"echo\",version=\"1.0.0\",namespace=\"\",state=\"total\"} 1"
        ),
        "{body}"
    );
    assert!(
        body.contains(
            "edger_worker_processes{worker=\"echo\",version=\"1.0.0\",namespace=\"\",state=\"idle\"} 1"
        ),
        "{body}"
    );
    assert!(
        body.contains(
            "edger_worker_queue_depth{worker=\"echo\",version=\"1.0.0\",namespace=\"\"} 0"
        ),
        "{body}"
    );
    assert!(
        body.contains(
            "edger_worker_recycle_total{worker=\"echo\",version=\"1.0.0\",namespace=\"\",cause=\"ttl\"} 0"
        ),
        "{body}"
    );
    assert!(body.contains("edger_http_requests_total{method=\"GET\",status=\"200\"}"));
    assert!(body.contains("edger_http_request_duration_ms_last"));
    assert!(!body.contains("echo@1.0.0"));
    assert!(!body.contains("worker_id"));
    assert!(!body.contains("/workers/echo"));
    assert!(!body.to_ascii_lowercase().contains("authorization"));
    assert!(!body.to_ascii_lowercase().contains("request_id"));
    assert_worker_metric_labels_are_low_cardinality(&body);
}

#[tokio::test]
async fn metrics_stats_returns_pool_and_worker_snapshot_without_secrets() {
    // Mutation captured: using the aggregate Prometheus formatter for stats
    // would omit the worker identity and request count operators need.
    let app = build_pipeline(test_state());
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/echo")
                    .header("authorization", "Bearer test-root")
                    .body(Body::from("secret request body"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
        .oneshot(
            Request::builder()
                .uri("/metrics/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let text = body_text(response).await;
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(body["pool"]["totalWorkers"], 1);
    assert_eq!(body["pool"]["cacheHits"], 1);
    assert_eq!(body["pool"]["cacheMisses"], 1);

    let workers = body["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 1);
    let worker = &workers[0];
    assert_eq!(worker["app"], "echo@1.0.0");
    assert_eq!(worker["name"], "echo");
    assert_eq!(worker["version"], "1.0.0");
    assert!(worker["requestTotal"].is_number());
    assert!(worker["requestDurationMsLast"].is_number());
    assert!(worker["requestDurationMsP95"].is_number());
    assert_eq!(worker["health"]["status"], "healthy");
    assert_eq!(worker["health"]["windowMs"], 300_000);
    assert_eq!(worker["health"]["sampleCount"], 2);
    assert_eq!(worker["health"]["successCount"], 2);
    assert_eq!(worker["health"]["failureCount"], 0);
    assert_eq!(worker["state"], "idle");
    assert_eq!(worker["requests"], 2);
    assert_eq!(worker["activeProcesses"], 0);
    assert_eq!(worker["idleProcesses"], 1);
    assert_eq!(worker["totalProcesses"], 1);
    assert_eq!(worker["queued"], 0);
    assert_eq!(worker["waitMs"], 0);
    assert_eq!(worker["rejectedTotal"], 0);
    assert_eq!(worker["timeoutTotal"], 0);
    assert_eq!(worker["recycle"]["ttl"], 0);
    assert_eq!(worker["recycle"]["maxRequests"], 0);
    assert_eq!(worker["recycle"]["error"], 0);
    assert_eq!(worker["recycle"]["oomShutdown"], 0);
    let processes = worker["processes"].as_array().unwrap();
    assert_eq!(processes.len(), 1);
    assert_eq!(processes[0]["state"], "idle");
    assert_eq!(processes[0]["requests"], 2);
    assert_eq!(processes[0]["unhealthy"], false);
    assert!(processes[0]["uptimeSeconds"].is_u64());
    assert!(worker["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(worker["uptimeSeconds"].is_u64());
    assert_eq!(worker["unhealthy"], false);

    assert!(!text.contains("test-root"));
    assert!(!text.contains("secret request body"));
    assert!(!text.contains("/workers/echo"));
    assert!(!text.to_ascii_lowercase().contains("authorization"));
    assert!(!text.to_ascii_lowercase().contains("requestid"));
    assert!(!text.to_ascii_lowercase().contains("request_id"));
    assert!(!text.to_ascii_lowercase().contains("header"));
    assert!(!text.to_ascii_lowercase().contains("root_api_key"));
    assert!(!text.to_ascii_lowercase().contains("env"));
}

#[tokio::test]
async fn health_readiness_liveness_aliases_keep_request_id() {
    // Mutation captured: adding probe aliases outside the request-id middleware
    // would make probes harder to correlate in logs and clients.
    let app = build_pipeline(test_state());
    for (path, status) in [
        ("/health", "ok"),
        ("/healthz", "ok"),
        ("/livez", "live"),
        ("/ready", "ready"),
        ("/readyz", "ready"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("x-request-id", "trace-08-07")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("trace-08-07")
        );
        let body: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(body["status"], status, "{path}");
    }
}

#[tokio::test]
async fn worker_dispatch_receives_request_id_field_and_header() {
    // Mutation captured: dropping the generated x-request-id before worker
    // dispatch leaves worker logs uncorrelated with orchestrator responses.
    let app = build_pipeline(test_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/echo")
                .header("authorization", "Bearer test-root")
                .header("x-request-id", "trace-worker")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("trace-worker")
    );
    let body = body_text(response).await;
    assert_eq!(body, "field=trace-worker header=trace-worker");
}

#[tokio::test]
async fn generated_request_id_is_propagated_to_worker_and_response() {
    // Mutation captured: generating the response header after dispatch but not
    // inserting it into the request would make the worker see a different ID.
    let app = build_pipeline(test_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/echo")
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let response_request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("generated request id")
        .to_string();
    assert!(!response_request_id.is_empty());
    let body = body_text(response).await;
    assert_eq!(
        body,
        format!("field={response_request_id} header={response_request_id}")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn worker_dispatch_log_includes_correlation_fields_without_secrets() {
    // Mutation captured: dispatch logs must include correlation data without
    // dumping auth headers or request bodies.
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(true)
        .without_time()
        .with_max_level(tracing::Level::INFO)
        .with_writer(logs.clone())
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
    let app = build_pipeline(test_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/echo")
                .header("authorization", "Bearer test-root")
                .header("x-request-id", "trace-dispatch")
                .body(Body::from("secret body"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let text = logs.text();
    assert!(text.contains("worker dispatch"), "logs:\n{text}");
    assert!(text.contains("request_id=trace-dispatch"), "logs:\n{text}");
    assert!(text.contains("worker_name=echo"), "logs:\n{text}");
    assert!(!text.contains("authorization"), "logs:\n{text}");
    assert!(!text.contains("test-root"), "logs:\n{text}");
    assert!(!text.contains("secret body"), "logs:\n{text}");
}
