//! Metrics and operational probe integration tests (Story 08.07).

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerManifest,
};
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_orchestrator::{
    build_pipeline, AuthGate, AuthGateConfig, ExtensionRegistry, ManifestIndex, OrchestratorState,
    ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

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

    let auth = Arc::new(AuthExtension::new(
        Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
        Some("test-root".into()),
    ));

    OrchestratorState {
        auth: AuthGate::new(AuthGateConfig::default(), auth),
        index,
        pool,
        registry: ExtensionRegistry::new(),
        server,
    }
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
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
    assert!(body.contains("edger_pool_workers 0"));
    assert!(!body.contains("test-root"));
    assert!(!body.to_ascii_lowercase().contains("authorization"));
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
    assert!(!body.contains("echo@1.0.0"));
    assert!(!body.contains("worker_id"));
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
    assert_eq!(worker["state"], "idle");
    assert_eq!(worker["requests"], 2);
    assert!(worker["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(worker["uptimeSeconds"].is_u64());
    assert_eq!(worker["unhealthy"], false);

    assert!(!text.contains("test-root"));
    assert!(!text.contains("secret request body"));
    assert!(!text.to_ascii_lowercase().contains("authorization"));
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
