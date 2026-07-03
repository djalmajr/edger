use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    create_worker_ref, Isolate, IsolationError, SerializedRequest, SerializedResponse,
    WorkerConfig, WorkerManifest, WorkerRef, DEFAULT_MAX_BODY_BYTES,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerError, WorkerPool};
use tempfile::TempDir;

struct CountingFactory {
    created: Arc<AtomicUsize>,
    executed: Arc<AtomicUsize>,
}

impl IsolateFactory for CountingFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        self.created.fetch_add(1, Ordering::SeqCst);
        Box::new(CountingIsolate {
            executed: Arc::clone(&self.executed),
        })
    }
}

struct CountingIsolate {
    executed: Arc<AtomicUsize>,
}

#[async_trait]
impl Isolate for CountingIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.executed.fetch_add(1, Ordering::SeqCst);
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
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

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(serialized_post(0), config).await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }
}

fn worker_ref(max_body_size: Option<&str>) -> (TempDir, WorkerRef) {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("index.ts"), "// stub").expect("write stub");
    let manifest = WorkerManifest {
        name: "body-limit".into(),
        entrypoint: Some("index.ts".into()),
        kind: Some("fetch".into()),
        max_body_size: max_body_size.map(str::to_string),
        version: Some("1.0.0".into()),
        ..Default::default()
    };
    let worker = create_worker_ref(dir.path().to_path_buf(), manifest).expect("worker ref");
    (dir, worker)
}

fn serialized_post(body_len: usize) -> SerializedRequest {
    SerializedRequest {
        method: "POST".into(),
        uri: "/body-limit".into(),
        headers: vec![],
        body: Some(Bytes::from(vec![b'x'; body_len])),
        request_id: "body-limit-test".into(),
        base_href: None,
    }
}

async fn assert_body_limit_rejected(worker: WorkerRef, body_len: usize) {
    let created = Arc::new(AtomicUsize::new(0));
    let executed = Arc::new(AtomicUsize::new(0));
    let pool = WorkerPool::with_factory(
        PoolConfig::default(),
        Arc::new(CountingFactory {
            created: Arc::clone(&created),
            executed: Arc::clone(&executed),
        }),
    );

    let err = pool
        .fetch_worker(&worker, serialized_post(body_len), None)
        .await
        .unwrap_err();

    match err {
        WorkerError::Isolation(err) => assert_eq!(err.code, "PAYLOAD_TOO_LARGE"),
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(created.load(Ordering::SeqCst), 0);
    assert_eq!(executed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn worker_pool_rejects_body_above_worker_limit_before_dispatch() {
    let (_dir, worker) = worker_ref(Some("4"));

    assert_body_limit_rejected(worker, 5).await;
}

#[tokio::test]
async fn worker_pool_rejects_body_above_default_global_limit_without_manifest_override() {
    let (_dir, worker) = worker_ref(None);

    assert_body_limit_rejected(worker, DEFAULT_MAX_BODY_BYTES as usize + 1).await;
}
