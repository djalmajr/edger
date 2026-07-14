//! Regression: concurrent requests to the same ephemeral worker (ttl=0) must
//! all succeed. A queued dispatcher that wakes up on an already-terminated
//! shared instance must re-resolve a fresh one instead of failing NotReady.

mod helpers;

use std::sync::Arc;

use async_trait::async_trait;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerRef,
};
use edger_worker::{IsolateFactory, PoolConfig};
use helpers::{pool_with_factory, serialized_get, temp_worker_dir};

struct EchoIsolate;

#[async_trait]
impl Isolate for EchoIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        // Yield so concurrent dispatchers interleave on the shared instance.
        tokio::task::yield_now().await;
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(bytes::Bytes::from(req.uri.into_bytes())),
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
        tokio::task::yield_now().await;
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(bytes::Bytes::from(path.to_string().into_bytes())),
        })
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new("NOT_IMPLEMENTED", "wasm"))
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

struct EchoFactory;

impl IsolateFactory for EchoFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(EchoIsolate)
    }
}

// Mutation captured: dropping the re-resolve loop in `fetch_worker_inner`
// (reverting to a single `get_or_create` + dispatch) makes queued dispatchers
// hit the terminated shared instance and fail with `worker not ready for
// dispatch`, so at least one of the concurrent asset fetches goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_to_ephemeral_worker_all_succeed() {
    // ttl absent -> ttl_ms == 0 -> ephemeral (terminated after each request).
    let (dir, config, _manifest) = temp_worker_dir("name: assets\n");
    // Generous ephemeral gate so this exercises the terminated-instance
    // re-resolve race, not queue backpressure (which is legitimate).
    let pool = pool_with_factory(
        Arc::new(EchoFactory),
        PoolConfig {
            max_size: 32,
            ephemeral_concurrency: 8,
            ephemeral_queue_limit: 64,
        },
    );
    let dir_path = dir.path().to_path_buf();

    let mut handles = Vec::new();
    for index in 0..24 {
        let pool = pool.clone();
        let config = config.clone();
        let dir_path = dir_path.clone();
        handles.push(tokio::spawn(async move {
            let uri = format!("/assets/module-{index}.js");
            pool.fetch(&dir_path, &config, serialized_get(&uri), None)
                .await
                .map(|res| (res.status, res.body.map(|b| b.to_vec())))
        }));
    }

    for (index, handle) in handles.into_iter().enumerate() {
        let result = handle.await.expect("task join");
        let (status, body) = result.unwrap_or_else(|err| panic!("request {index} failed: {err}"));
        assert_eq!(status, 200, "request {index} unexpected status");
        let expected = format!("/assets/module-{index}.js");
        assert_eq!(body.as_deref(), Some(expected.as_bytes()));
    }
}
