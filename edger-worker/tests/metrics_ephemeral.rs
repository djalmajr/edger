//! PoolMetrics + ephemeral gate tests (story 04.03) — written first (TDD red).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    create_worker_ref, ExecutionKind, Isolate, SerializedRequest, SerializedResponse, WorkerConfig,
    WorkerManifest, WorkerRef,
};
use edger_worker::{
    EphemeralGate, IsolateFactory, MetricsCollector, PoolConfig, WorkerError, WorkerPool,
};

struct SlowFactory {
    delay_ms: u64,
}

impl IsolateFactory for SlowFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        if self.delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.delay_ms));
        }
        Box::new(EchoIsolate { delay_ms: 0 })
    }
}

struct EchoIsolate {
    delay_ms: u64,
}

#[async_trait]
impl Isolate for EchoIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        if self.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        }
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(format!("echo:{}", req.uri))),
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(format!("spa:{path}"))),
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(req, config).await
    }
}

fn sample_req(uri: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: uri.into(),
        headers: vec![],
        body: None,
        request_id: "metrics-req".into(),
        base_href: None,
    }
}

fn make_worker_ref(dir: PathBuf, name: &str, ttl_ms: u64, max_requests: u32) -> WorkerRef {
    let mut worker_ref = create_worker_ref(
        dir,
        WorkerManifest {
            name: name.into(),
            ..Default::default()
        },
    )
    .unwrap();
    worker_ref.config.ttl_ms = ttl_ms;
    worker_ref.config.max_requests = max_requests;
    worker_ref
}

fn make_versioned_worker_ref(
    dir: PathBuf,
    name: &str,
    version: &str,
    ttl_ms: u64,
    max_requests: u32,
) -> WorkerRef {
    let mut worker_ref = make_worker_ref(dir, name, ttl_ms, max_requests);
    worker_ref.version = version.into();
    worker_ref
}

fn pool_with_factory(config: PoolConfig, factory: Arc<dyn IsolateFactory>) -> WorkerPool {
    WorkerPool::with_factory(config, factory)
}

#[tokio::test]
async fn get_metrics_reflects_cache_hit_on_second_fetch() {
    let dir = PathBuf::from("/workers/metrics-hit");
    let mut config = make_worker_ref(dir.clone(), "hit", 30_000, 0).config;
    config.ttl_ms = 30_000;
    let pool = pool_with_factory(PoolConfig::default(), Arc::new(SlowFactory { delay_ms: 0 }));

    pool.fetch(
        &dir,
        &config,
        sample_req("/a"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();
    pool.fetch(
        &dir,
        &config,
        sample_req("/b"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    let metrics = pool.get_metrics();
    assert!(metrics.cache_hits >= 1, "expected cache hit metric");
    assert!(metrics.cache_misses >= 1, "expected at least one miss");
}

#[tokio::test]
async fn worker_stats_snapshot_reports_identity_state_and_requests() {
    // Mutation captured: deriving stats identity from the directory instead of
    // the resolved worker ref would lose the manifest version in the snapshot.
    let dir = PathBuf::from("/workers/metrics-worker");
    let worker_ref = make_versioned_worker_ref(dir, "metrics-worker", "1.2.3", 30_000, 0);
    let pool = pool_with_factory(PoolConfig::default(), Arc::new(SlowFactory { delay_ms: 0 }));

    pool.fetch_worker(
        &worker_ref,
        sample_req("/a"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();
    pool.fetch_worker(
        &worker_ref,
        sample_req("/b"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    let workers = pool.worker_stats();
    assert_eq!(workers.len(), 1);
    let worker = &workers[0];
    assert_eq!(worker.app, "metrics-worker@1.2.3");
    assert_eq!(worker.name, "metrics-worker");
    assert_eq!(worker.version, "1.2.3");
    assert_eq!(worker.request_count, 2);
    assert_eq!(worker.state, edger_worker::WorkerState::Idle);
    assert!(!worker.unhealthy);
    assert_eq!(
        pool.get_worker_stats(worker.worker_id)
            .map(|stats| stats.request_count),
        Some(2)
    );
}

#[tokio::test]
async fn spawn_latency_recorded_on_cache_miss() {
    let dir = PathBuf::from("/workers/spawn-lat");
    let config = make_worker_ref(dir.clone(), "spawn", 30_000, 0).config;
    let pool = pool_with_factory(PoolConfig::default(), Arc::new(SlowFactory { delay_ms: 5 }));

    pool.fetch(
        &dir,
        &config,
        sample_req("/"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    let metrics = pool.get_metrics();
    assert!(
        metrics.spawn_latency_ms_last >= 1,
        "spawn latency should be recorded, got {}",
        metrics.spawn_latency_ms_last
    );
}

#[tokio::test]
async fn max_requests_retires_worker_after_limit() {
    let dir = PathBuf::from("/workers/max-req");
    let config = make_worker_ref(dir.clone(), "max", 30_000, 2).config;
    let pool = pool_with_factory(PoolConfig::default(), Arc::new(SlowFactory { delay_ms: 0 }));

    pool.fetch(
        &dir,
        &config,
        sample_req("/1"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();
    pool.fetch(
        &dir,
        &config,
        sample_req("/2"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    let metrics = pool.get_metrics();
    assert!(
        metrics.terminated_total >= 1,
        "worker should retire after max_requests"
    );
    assert_eq!(pool.len(), 0, "retired worker must be removed from pool");

    let before_misses = metrics.cache_misses;
    pool.fetch(
        &dir,
        &config,
        sample_req("/3"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();
    let after = pool.get_metrics();
    assert!(
        after.cache_misses > before_misses,
        "third fetch should spawn a new worker after retirement"
    );
}

#[tokio::test]
async fn ephemeral_gate_rejects_when_queue_limit_zero() {
    let metrics = Arc::new(MetricsCollector::default());
    let gate = EphemeralGate::new(1, 0, Arc::clone(&metrics));
    let _permit = gate.acquire().await.unwrap();
    let second = gate.acquire().await;
    assert!(matches!(second, Err(WorkerError::EphemeralQueueFull)));
    assert_eq!(metrics.snapshot().ephemeral_rejected, 1);
}

struct SlowEchoFactory {
    delay_ms: u64,
}

impl IsolateFactory for SlowEchoFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(EchoIsolate {
            delay_ms: self.delay_ms,
        })
    }
}

#[tokio::test]
async fn ephemeral_concurrency_limits_parallel_fetches() {
    let pool = WorkerPool::with_factory(
        PoolConfig {
            max_size: 8,
            ephemeral_concurrency: 1,
            ephemeral_queue_limit: 0,
        },
        Arc::new(SlowEchoFactory { delay_ms: 120 }),
    );

    let dir = PathBuf::from("/workers/ephem-conc");
    let config = make_worker_ref(dir.clone(), "ephem", 0, 0).config;

    let pool_a = pool.clone();
    let pool_b = pool.clone();
    let config_a = config.clone();
    let config_b = config.clone();
    let dir_a = dir.clone();
    let dir_b = dir.clone();

    let first = tokio::spawn(async move {
        pool_a
            .fetch(
                &dir_a,
                &config_a,
                sample_req("/slow"),
                Some(ExecutionKind::FetchHandler),
            )
            .await
    });

    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(40)).await;

    let second = pool_b
        .fetch(
            &dir_b,
            &config_b,
            sample_req("/blocked"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;

    assert!(
        second.is_err(),
        "second ephemeral fetch should be rejected when queue limit is 0"
    );
    assert!(second.unwrap_err().to_string().contains("ephemeral"));

    let _ = first.await.unwrap().unwrap();
}

#[tokio::test]
async fn ephemeral_queue_full_returns_typed_error() {
    let pool = WorkerPool::with_factory(
        PoolConfig {
            max_size: 8,
            ephemeral_concurrency: 1,
            ephemeral_queue_limit: 1,
        },
        Arc::new(SlowEchoFactory { delay_ms: 150 }),
    );

    let dir = PathBuf::from("/workers/ephem-queue");
    let config = make_worker_ref(dir.clone(), "qfull", 0, 0).config;

    let pool_a = pool.clone();
    let pool_b = pool.clone();
    let pool_c = pool.clone();
    let cfg = config.clone();
    let dir_a = dir.clone();
    let dir_b = dir.clone();
    let dir_c = dir.clone();

    let t1 = tokio::spawn(async move {
        pool_a
            .fetch(
                &dir_a,
                &cfg,
                sample_req("/1"),
                Some(ExecutionKind::FetchHandler),
            )
            .await
    });

    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let t2 = tokio::spawn(async move {
        pool_b
            .fetch(
                &dir_b,
                &config,
                sample_req("/2"),
                Some(ExecutionKind::FetchHandler),
            )
            .await
    });

    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    let third = pool_c
        .fetch(
            &dir_c,
            &make_worker_ref(dir_c.clone(), "qfull", 0, 0).config,
            sample_req("/3"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;

    assert!(third.is_err());
    assert!(third.unwrap_err().to_string().contains("ephemeral queue"));

    let _ = tokio::join!(t1, t2);
}
