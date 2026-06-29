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
    fn create_isolate(&self) -> Box<dyn Isolate> {
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

#[tokio::test]
async fn lru_evicts_oldest_when_full() {
    let pool = pool(2);
    let w1 = make_worker_ref(PathBuf::from("/workers/a"), "worker-a");
    let w2 = make_worker_ref(PathBuf::from("/workers/b"), "worker-b");
    let w3 = make_worker_ref(PathBuf::from("/workers/c"), "worker-c");

    pool.get_or_create(&w1).unwrap();
    pool.get_or_create(&w2).unwrap();
    pool.get_or_create(&w3).unwrap();

    assert!(pool.get_or_create(&w1).is_err());
    assert!(pool.get_or_create(&w2).is_ok());
    assert!(pool.get_or_create(&w3).is_ok());
}

#[tokio::test]
async fn second_fetch_is_cache_hit() {
    let pool = pool(4);
    let dir = PathBuf::from("/workers/hit");
    let config = make_worker_ref(dir.clone(), "hit").config;

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
async fn namespaced_and_unscoped_are_distinct_keys() {
    let pool = pool(4);
    let scoped = make_worker_ref(PathBuf::from("/ns/acme"), "@acme/app");
    let unscoped = make_worker_ref(PathBuf::from("/plain/app"), "app");

    let a = pool.get_or_create(&scoped).unwrap();
    let b = pool.get_or_create(&unscoped).unwrap();

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
