//! End-to-end pipeline tests (story 05.03 / 06.02).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerManifest,
    WorkerRef,
};
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, AuthGate, AuthGateConfig, ExtensionRegistry, ManifestIndex, OrchestratorState,
    ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedRequest {
    base_href: Option<String>,
    uri: String,
    worker_name: String,
    x_base: Option<String>,
}

#[derive(Clone, Default)]
struct RecordingFactory {
    records: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl RecordingFactory {
    fn records(&self) -> Vec<RecordedRequest> {
        self.records.lock().expect("recording lock").clone()
    }
}

struct RecordingIsolate {
    records: Arc<Mutex<Vec<RecordedRequest>>>,
    worker_name: String,
}

impl RecordingIsolate {
    fn record(&self, req: &SerializedRequest) {
        let x_base = req
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-base"))
            .map(|(_, value)| value.clone());
        self.records
            .lock()
            .expect("recording lock")
            .push(RecordedRequest {
                base_href: req.base_href.clone(),
                uri: req.uri.clone(),
                worker_name: self.worker_name.clone(),
                x_base,
            });
    }
}

impl IsolateFactory for RecordingFactory {
    fn create_isolate(&self, worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(RecordingIsolate {
            records: self.records.clone(),
            worker_name: worker_ref.name.clone(),
        })
    }
}

#[async_trait]
impl Isolate for RecordingIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.record(&req);
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from_static(b"recorded")),
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
        path: &str,
        base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.records
            .lock()
            .expect("recording lock")
            .push(RecordedRequest {
                base_href: base_href.map(String::from),
                uri: path.to_string(),
                worker_name: self.worker_name.clone(),
                x_base: None,
            });
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from_static(b"recorded")),
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
        auth: AuthGate::new(
            AuthGateConfig::default(),
            Arc::new(AuthExtension::new(
                Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
                Some("test-root".into()),
            )),
        ),
    }
}

fn orchestrator_with_index_and_factory(
    index: ManifestIndex,
    factory: Arc<dyn IsolateFactory>,
) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), factory);
    server.mark_ready(pool.clone());

    OrchestratorState {
        server,
        pool,
        index,
        registry: ExtensionRegistry::new(),
        auth: AuthGate::new(
            AuthGateConfig::default(),
            Arc::new(AuthExtension::new(
                Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
                Some("test-root".into()),
            )),
        ),
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

#[tokio::test]
async fn plugin_base_dispatches_root_remainder_with_custom_base() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/plugins/panel"),
            WorkerManifest {
                base: Some("/painel".into()),
                name: "panel-plugin".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let factory = RecordingFactory::default();
    let app = build_pipeline(orchestrator_with_index_and_factory(
        index,
        Arc::new(factory.clone()),
    ));

    let res = app
        .oneshot(
            Request::builder()
                .uri("/painel")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        factory.records(),
        vec![RecordedRequest {
            base_href: Some("/painel/".into()),
            uri: "/".into(),
            worker_name: "panel-plugin".into(),
            x_base: Some("/painel".into()),
        }]
    );
}

#[tokio::test]
async fn plugin_base_dispatches_subpath_and_preserves_query() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/painel"),
            WorkerManifest {
                name: "painel".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
    index
        .insert(
            PathBuf::from("/plugins/panel"),
            WorkerManifest {
                base: Some("/painel".into()),
                name: "panel-plugin".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let factory = RecordingFactory::default();
    let app = build_pipeline(orchestrator_with_index_and_factory(
        index,
        Arc::new(factory.clone()),
    ));

    let res = app
        .oneshot(
            Request::builder()
                .uri("/painel/sub/rota?q=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        factory.records(),
        vec![RecordedRequest {
            base_href: Some("/painel/".into()),
            uri: "/sub/rota?q=1".into(),
            worker_name: "panel-plugin".into(),
            x_base: Some("/painel".into()),
        }]
    );
}

#[tokio::test]
async fn disabled_plugin_base_returns_not_found() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/plugins/panel"),
            WorkerManifest {
                base: Some("/painel".into()),
                name: "panel-plugin".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
    index
        .set_worker_enabled("panel-plugin", None, false)
        .unwrap();
    let factory = RecordingFactory::default();
    let app = build_pipeline(orchestrator_with_index_and_factory(
        index,
        Arc::new(factory.clone()),
    ));

    let res = app
        .oneshot(
            Request::builder()
                .uri("/painel")
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
    assert_eq!(json["code"], "NOT_FOUND");
    assert!(factory.records().is_empty());
}
