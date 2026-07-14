//! Shared helpers for edger-worker integration tests (story 04.04).
//!
//! Maps Buntime-style manifest fixtures to `edger-core` config for pool fetch.

#![allow(dead_code)]

use std::sync::Arc;

use edger_core::{
    parse_worker_config, ExecutionKind, SerializedRequest, WorkerConfig, WorkerManifest,
};
use edger_isolation::MockIsolate;
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tempfile::TempDir;

/// Factory producing configurable `MockIsolate` instances (dev-dep only).
#[derive(Default)]
pub struct MockIsolateFactory {
    pub slow_fetch_ms: u64,
    pub spa_html: Option<String>,
}

impl IsolateFactory for MockIsolateFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        let mut mock = MockIsolate::new();
        if self.slow_fetch_ms > 0 {
            mock = mock.with_slow_fetch_ms(self.slow_fetch_ms);
        }
        if let Some(html) = &self.spa_html {
            mock = mock.with_spa_html(html.clone());
        }
        Box::new(mock)
    }
}

/// Write `manifest.yaml` into a temp worker dir and return parsed config.
pub fn temp_worker_dir(manifest_yaml: &str) -> (TempDir, WorkerConfig, WorkerManifest) {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("manifest.yaml"), manifest_yaml).expect("write manifest");
    std::fs::write(dir.path().join("index.ts"), "// stub entrypoint").expect("write stub");

    let manifest: WorkerManifest =
        serde_yaml::from_str(manifest_yaml).expect("manifest yaml parse");
    let config = parse_worker_config(&manifest);
    (dir, config, manifest)
}

pub fn serialized_get(path: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: path.into(),
        headers: vec![],
        body: None,
        request_id: "integration-req".into(),
        base_href: Some("/@app/".into()),
    }
}

pub fn pool_with_factory(factory: Arc<dyn IsolateFactory>, config: PoolConfig) -> WorkerPool {
    WorkerPool::with_factory(config, factory)
}

pub fn default_pool_config() -> PoolConfig {
    PoolConfig {
        max_size: 16,
        ephemeral_concurrency: 4,
        ephemeral_queue_limit: 8,
    }
}

pub fn execution_kind_from_manifest(manifest: &WorkerManifest) -> Option<ExecutionKind> {
    parse_worker_config(manifest).kind
}
