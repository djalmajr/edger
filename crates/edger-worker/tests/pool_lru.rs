//! WorkerPool LRU tests (story 04.01) — written first (TDD red).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    create_worker_ref, ExecutionKind, Isolate, SerializedRequest, SerializedResponse, WorkerConfig,
    WorkerManifest, WorkerRef,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};

struct EchoFactory;

impl IsolateFactory for EchoFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(EchoIsolate)
    }
}

struct EchoIsolate;

#[async_trait]
impl Isolate for EchoIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
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

struct SlowEchoFactory;

impl IsolateFactory for SlowEchoFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(SlowEchoIsolate)
    }
}

struct SlowEchoIsolate;

#[async_trait]
impl Isolate for SlowEchoIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(format!("slow:{}", req.uri))),
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
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(format!("slow-spa:{path}"))),
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
        request_id: "pool-req".into(),
        base_href: None,
    }
}

fn make_worker_ref(dir: PathBuf, name: &str) -> WorkerRef {
    create_worker_ref(
        dir,
        WorkerManifest {
            name: name.into(),
            ..Default::default()
        },
    )
    .unwrap()
}

fn pool(max_size: usize) -> WorkerPool {
    WorkerPool::with_factory(
        PoolConfig {
            max_size,
            ephemeral_concurrency: 4,
            ephemeral_queue_limit: 8,
        },
        Arc::new(EchoFactory),
    )
}

fn slow_pool(max_size: usize) -> WorkerPool {
    WorkerPool::with_factory(
        PoolConfig {
            max_size,
            ephemeral_concurrency: 4,
            ephemeral_queue_limit: 8,
        },
        Arc::new(SlowEchoFactory),
    )
}

#[tokio::test]
async fn lru_evicts_oldest_when_full() {
    let pool = pool(2);
    let w1 = make_worker_ref(PathBuf::from("/workers/a"), "worker-a");
    let w2 = make_worker_ref(PathBuf::from("/workers/b"), "worker-b");
    let w3 = make_worker_ref(PathBuf::from("/workers/c"), "worker-c");

    pool.get_or_create(&w1).await.unwrap();
    pool.get_or_create(&w2).await.unwrap();
    pool.get_or_create(&w3).await.unwrap();

    assert!(pool.get_or_create(&w1).await.is_err());
    assert!(pool.get_or_create(&w2).await.is_ok());
    assert!(pool.get_or_create(&w3).await.is_ok());
}

#[tokio::test]
async fn second_fetch_is_cache_hit() {
    let pool = pool(4);
    let dir = PathBuf::from("/workers/hit");
    let mut config = make_worker_ref(dir.clone(), "hit").config;
    config.ttl_ms = 30_000;

    pool.fetch(
        &dir,
        &config,
        sample_req("/first"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    pool.fetch(
        &dir,
        &config,
        sample_req("/second"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    let metrics = pool.get_metrics();
    assert!(metrics.cache_hits >= 1, "expected cache hit");
}

#[tokio::test]
async fn concurrent_fetches_for_same_worker_queue_instead_of_failing_active_state() {
    let pool = slow_pool(4);
    let dir = PathBuf::from("/workers/concurrent");
    let mut config = make_worker_ref(dir.clone(), "concurrent").config;
    config.ttl_ms = 30_000;

    let first = pool.fetch(
        &dir,
        &config,
        sample_req("/first"),
        Some(ExecutionKind::FetchHandler),
    );
    let second = pool.fetch(
        &dir,
        &config,
        sample_req("/second"),
        Some(ExecutionKind::FetchHandler),
    );

    let (first, second) = tokio::join!(first, second);

    assert_eq!(first.unwrap().status, 200);
    assert_eq!(second.unwrap().status, 200);

    let instance = pool
        .get_or_create(&make_worker_ref(dir, "concurrent"))
        .await
        .unwrap();
    assert_eq!(instance.state(), edger_worker::WorkerState::Idle);
    assert_eq!(instance.request_count(), 2);
}

#[tokio::test]
async fn namespaced_and_unscoped_are_distinct_keys() {
    let pool = pool(4);
    let scoped = make_worker_ref(PathBuf::from("/ns/acme"), "@acme/app");
    let unscoped = make_worker_ref(PathBuf::from("/plain/app"), "app");

    let a = pool.get_or_create(&scoped).await.unwrap();
    let b = pool.get_or_create(&unscoped).await.unwrap();

    assert_ne!(a.worker_ref.name, b.worker_ref.name);
    assert_eq!(pool.len(), 2);
}

#[tokio::test]
async fn shutdown_rejects_new_fetch() {
    let pool = pool(2);
    let worker_ref = make_worker_ref(PathBuf::from("/workers/shutdown"), "shutdown-worker");
    let config = worker_ref.config.clone();

    pool.shutdown();
    let err = pool
        .fetch(
            &worker_ref.dir,
            &config,
            sample_req("/"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("shut down"));
}
