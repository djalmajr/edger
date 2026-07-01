use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};
use edger_core::{WorkerManifest, WorkerRef};
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_orchestrator::{
    build_pipeline, AuthGate, AuthGateConfig, ExtensionRegistry, ManifestIndex, OrchestratorState,
    ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

#[derive(Clone)]
struct PerfFactory;

impl IsolateFactory for PerfFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(PerfIsolate)
    }
}

struct PerfIsolate;

#[async_trait]
impl Isolate for PerfIsolate {
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
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: Some(Bytes::from_static(b"<html></html>")),
        })
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

fn perf_state() -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/perf-echo"),
            WorkerManifest {
                name: "perf-echo".into(),
                version: Some("1.0.0".into()),
                ttl: Some(serde_yaml::Value::String("30s".into())),
                ..Default::default()
            },
        )
        .unwrap();

    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(PerfFactory));
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

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    let index = ((sorted.len() * percentile).saturating_sub(1) / 100).min(sorted.len() - 1);
    sorted[index]
}

#[tokio::test]
#[ignore = "performance harness; run explicitly with cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture"]
async fn persistent_worker_warm_hit_baseline() {
    let state = perf_state();
    let pool = state.pool.clone();
    let app = build_pipeline(state);
    let mut durations = Vec::with_capacity(50);

    for _ in 0..50 {
        let started = Instant::now();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/perf-echo")
                    .header("authorization", "Bearer test-root")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        durations.push(started.elapsed());
    }

    durations.sort_unstable();
    let p50 = percentile(&durations, 50);
    let p95 = percentile(&durations, 95);
    let metrics = pool.get_metrics();

    println!(
        "perf_harness scenario=persistent_worker_warm_hit requests=50 p50_us={} p95_us={} cache_hits={} cache_misses={} active_workers={}",
        p50.as_micros(),
        p95.as_micros(),
        metrics.cache_hits,
        metrics.cache_misses,
        metrics.active_workers
    );

    assert_eq!(metrics.cache_misses, 1);
    assert_eq!(metrics.cache_hits, 49);
    assert_eq!(metrics.active_workers, 1);
}
