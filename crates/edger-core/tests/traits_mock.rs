//! Core worker vocabulary compile/smoke tests.

use async_trait::async_trait;
use edger_core::{
    create_worker_ref, Isolate, IsolationError, SerializedRequest, SerializedResponse,
    WorkerConfig, WorkerManifest,
};

struct MockIsolate;

#[async_trait]
impl Isolate for MockIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: None,
        })
    }

    async fn execute_routes(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new("NOT_IMPL", "routes"))
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: None,
        })
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new("NOT_IMPL", "wasm"))
    }
}

#[tokio::test]
async fn worker_ref_and_isolate_mock_compile() {
    let manifest = WorkerManifest {
        name: "hello".into(),
        ..Default::default()
    };
    let worker = create_worker_ref(std::path::PathBuf::from("/w"), manifest).unwrap();
    let req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "r".into(),
        base_href: None,
    };

    let mut isolate = MockIsolate;
    let config = worker.config.clone();
    let out = isolate.execute_fetch(req, &config).await.unwrap();
    assert_eq!(out.status, 200);
}
