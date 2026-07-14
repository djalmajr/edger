//! Regression: an isolate failure must not brick the pooled worker.

mod helpers;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerRef,
};
use edger_worker::IsolateFactory;
use helpers::{default_pool_config, pool_with_factory, serialized_get, temp_worker_dir};

struct FlakyIsolate {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Isolate for FlakyIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(IsolationError::new("EXEC_FAILED", "boom on first call"));
        }
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(bytes::Bytes::from_static(b"recovered")),
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
        Err(IsolationError::new("NOT_IMPLEMENTED", "spa"))
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

struct FlakyFactory {
    calls: Arc<AtomicUsize>,
}

impl IsolateFactory for FlakyFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(FlakyIsolate {
            calls: Arc::clone(&self.calls),
        })
    }
}

// Mutation captured: dropping the on-error recycle in
// `WorkerPool::fetch_worker_inner` leaves the instance in `Active`, the
// second fetch fails with `worker not ready for dispatch`, and this test
// goes red.
#[tokio::test]
async fn pool_recycles_worker_after_isolate_error() {
    let calls = Arc::new(AtomicUsize::new(0));
    let pool = pool_with_factory(
        Arc::new(FlakyFactory {
            calls: Arc::clone(&calls),
        }),
        default_pool_config(),
    );
    let (dir, config, _manifest) = temp_worker_dir("name: flaky\nttl: 60\n");

    let first = pool
        .fetch(dir.path(), &config, serialized_get("/flaky"), None)
        .await;
    assert!(first.is_err(), "first dispatch must surface the failure");

    // The failed instance must be evicted so no stale worker remains pooled.
    assert!(pool.is_empty(), "failed worker must leave the cache");

    let second = pool
        .fetch(dir.path(), &config, serialized_get("/flaky"), None)
        .await
        .expect("second dispatch must get a fresh worker");
    assert_eq!(second.status, 200);
    assert_eq!(second.body.unwrap().as_ref(), b"recovered");
}
