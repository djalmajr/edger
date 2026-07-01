//! Cron scheduler integration tests (story 07.03).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use edger_core::{
    CronJob, Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig,
    WorkerManifest, INTERNAL_REQUEST_HEADER,
};
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_orchestrator::{
    build_pipeline, collect_cron_registrations, AuthGate, AuthGateConfig, CronScheduler,
    CronSchedulerConfig, ExtensionRegistry, ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct RecordedRequests {
    inner: Arc<Mutex<Vec<SerializedRequest>>>,
}

impl RecordedRequests {
    fn push(&self, request: SerializedRequest) {
        self.inner.lock().expect("recorded requests").push(request);
    }

    fn first(&self) -> Option<SerializedRequest> {
        self.inner
            .lock()
            .expect("recorded requests")
            .first()
            .cloned()
    }
}

struct RecordingFactory {
    requests: RecordedRequests,
}

impl IsolateFactory for RecordingFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(RecordingIsolate {
            requests: self.requests.clone(),
        })
    }
}

struct RecordingIsolate {
    requests: RecordedRequests,
}

#[async_trait]
impl Isolate for RecordingIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.requests.push(req);
        Ok(SerializedResponse {
            status: 204,
            headers: vec![],
            body: None,
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
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: Some(Bytes::from(format!("<html>{path}</html>"))),
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

fn cron_manifest(enabled: Option<bool>, schedule: &str) -> WorkerManifest {
    WorkerManifest {
        name: "cron-worker".into(),
        version: Some("1.0.0".into()),
        enabled,
        entrypoint: Some("index.ts".into()),
        cron: Some(vec![CronJob {
            schedule: schedule.into(),
            path: "/tick".into(),
            method: Some("POST".into()),
        }]),
        ..Default::default()
    }
}

fn public_worker_manifest() -> WorkerManifest {
    WorkerManifest {
        name: "cron-worker".into(),
        version: Some("1.0.0".into()),
        entrypoint: Some("index.ts".into()),
        visibility: Some("public".into()),
        ..Default::default()
    }
}

fn state_with_index(index: ManifestIndex, requests: RecordedRequests) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(
        PoolConfig::default(),
        Arc::new(RecordingFactory { requests }),
    );
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
                Some("root-secret".into()),
            )),
        ),
    }
}

async fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition was not met before timeout");
}

#[tokio::test]
async fn cron_manifest_dispatches_internal_authenticated_request_and_counts_execution() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/cron-worker"),
            cron_manifest(None, "@every 25ms"),
        )
        .unwrap();
    let requests = RecordedRequests::default();
    let state = state_with_index(index, requests.clone());
    let metrics = state.server.cron_metrics();
    let registrations = collect_cron_registrations(&state.index).unwrap();
    assert_eq!(registrations.len(), 1);

    let app = build_pipeline(state);
    let scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("root-secret".into())),
        registrations,
        app.clone(),
        metrics.clone(),
    )
    .unwrap();
    wait_until(|| metrics.executions_total() > 0).await;
    let metrics_response = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let metrics_body = axum::body::to_bytes(metrics_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let metrics_text = String::from_utf8(metrics_body.to_vec()).unwrap();
    scheduler.shutdown().await;

    assert_eq!(metrics.failures_total(), 0);
    assert!(metrics_text.contains("# TYPE edger_cron_executions_total counter"));
    assert!(metrics_text.contains("edger_cron_executions_total "));
    let request = requests.first().expect("cron worker request");
    assert_eq!(request.method, "POST");
    assert_eq!(request.uri, "/tick");
    assert!(request
        .headers
        .iter()
        .any(|(name, value)| name == INTERNAL_REQUEST_HEADER && value == "true"));
    assert!(request
        .headers
        .iter()
        .all(|(name, _)| name != "authorization"));
}

#[tokio::test]
async fn cron_shutdown_cancels_registered_jobs() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/cron-worker"),
            cron_manifest(None, "@every 20ms"),
        )
        .unwrap();
    let requests = RecordedRequests::default();
    let state = state_with_index(index, requests);
    let metrics = state.server.cron_metrics();
    let scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("root-secret".into())),
        collect_cron_registrations(&state.index).unwrap(),
        build_pipeline(state),
        metrics.clone(),
    )
    .unwrap();
    wait_until(|| metrics.executions_total() > 0).await;

    scheduler.shutdown().await;
    let count_after_shutdown = metrics.executions_total();
    tokio::time::sleep(Duration::from_millis(80)).await;

    assert_eq!(metrics.executions_total(), count_after_shutdown);
}

#[test]
fn disabled_worker_does_not_register_cron_jobs_until_reload() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/cron-worker"),
            cron_manifest(Some(false), "@every 1s"),
        )
        .unwrap();

    let registrations = collect_cron_registrations(&index).unwrap();

    assert!(registrations.is_empty());
}

#[test]
fn invalid_schedule_fails_startup_validation_with_typed_error() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/cron-worker"),
            cron_manifest(None, "0 0 * * *"),
        )
        .unwrap();

    let err = collect_cron_registrations(&index).unwrap_err();

    assert_eq!(err.code, "CRON_SCHEDULE_INVALID");
    assert!(err.message.contains("0 0 * * *"));
}

#[tokio::test]
async fn cron_jobs_require_root_key_for_internal_dispatch() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/cron-worker"),
            cron_manifest(None, "@every 1s"),
        )
        .unwrap();
    let requests = RecordedRequests::default();
    let state = state_with_index(index, requests);
    let metrics = state.server.cron_metrics();

    let err = match CronScheduler::start(
        CronSchedulerConfig::new(None),
        collect_cron_registrations(&state.index).unwrap(),
        build_pipeline(state),
        metrics,
    ) {
        Ok(_) => panic!("cron scheduler started without ROOT_API_KEY"),
        Err(err) => err,
    };

    assert_eq!(err.code, "CRON_AUTH_MISSING");
}

#[tokio::test]
async fn spoofed_internal_header_is_not_forwarded_to_public_worker() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/cron-worker"),
            public_worker_manifest(),
        )
        .unwrap();
    let requests = RecordedRequests::default();
    let app = build_pipeline(state_with_index(index, requests.clone()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/cron-worker")
                .header(INTERNAL_REQUEST_HEADER, "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let request = requests.first().expect("public worker request");
    assert!(request
        .headers
        .iter()
        .all(|(name, _)| name != INTERNAL_REQUEST_HEADER));
}
