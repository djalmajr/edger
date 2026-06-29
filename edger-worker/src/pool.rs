//! WorkerPool — LRU cache + fetch entry point.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use edger_core::{
    create_worker_ref, ExecutionKind, Isolate, SerializedRequest, SerializedResponse, WorkerConfig,
    WorkerManifest, WorkerRef,
};

use crate::error::WorkerError;
use crate::factory::IsolateFactory;
use crate::instance::WorkerInstance;
use crate::lru::WorkerLru;
use crate::metrics::PoolMetrics;
use crate::types::{PoolConfig, WorkerCacheKey};

pub struct WorkerPool {
    #[allow(dead_code)]
    config: PoolConfig,
    cache: WorkerLru,
    factory: Arc<dyn IsolateFactory>,
    metrics: Mutex<PoolMetrics>,
    evicted: Mutex<HashSet<WorkerCacheKey>>,
    shutdown: AtomicBool,
}

impl WorkerPool {
    pub fn new(
        max_size: usize,
        ephemeral_concurrency: usize,
        ephemeral_queue_limit: usize,
        factory: Arc<dyn IsolateFactory>,
    ) -> Self {
        Self::with_factory(
            PoolConfig {
                max_size,
                ephemeral_concurrency,
                ephemeral_queue_limit,
            },
            factory,
        )
    }

    pub fn with_factory(config: PoolConfig, factory: Arc<dyn IsolateFactory>) -> Self {
        Self {
            cache: WorkerLru::new(config.max_size),
            config,
            factory,
            metrics: Mutex::new(PoolMetrics::default()),
            evicted: Mutex::new(HashSet::new()),
            shutdown: AtomicBool::new(false),
        }
    }

    fn ensure_active(&self) -> Result<(), WorkerError> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(WorkerError::Shutdown);
        }
        Ok(())
    }

    fn record_hit(&self) {
        let mut m = self.metrics.lock().expect("metrics lock");
        m.cache_hits += 1;
    }

    fn record_miss(&self) {
        let mut m = self.metrics.lock().expect("metrics lock");
        m.cache_misses += 1;
    }

    fn sync_active_count(&self) {
        let mut m = self.metrics.lock().expect("metrics lock");
        m.active_workers = self.cache.len();
    }

    /// Resolve or create a pooled worker instance.
    pub fn get_or_create(
        &self,
        worker_ref: &WorkerRef,
    ) -> Result<Arc<WorkerInstance>, WorkerError> {
        self.ensure_active()?;
        let key = WorkerCacheKey::from_worker_ref(worker_ref);

        if self.evicted.lock().expect("evicted lock").contains(&key) {
            return Err(WorkerError::Evicted);
        }

        if let Some(instance) = self.cache.get(&key) {
            if instance.worker_ref.namespace != worker_ref.namespace {
                return Err(WorkerError::Collision {
                    key: format!("{key:?}"),
                    detail: "namespace mismatch for cache key".into(),
                });
            }
            self.record_hit();
            return Ok(instance);
        }

        let isolate = self.factory.create_isolate();
        let instance = Arc::new(WorkerInstance::new(worker_ref.clone(), isolate));

        if let Some(evicted_key) = self.cache.insert(key.clone(), Arc::clone(&instance)) {
            if evicted_key == key {
                return Err(WorkerError::Collision {
                    key: format!("{key:?}"),
                    detail: "concurrent insert".into(),
                });
            }
            self.evicted
                .lock()
                .expect("evicted lock")
                .insert(evicted_key);
        }

        self.record_miss();
        self.sync_active_count();
        Ok(instance)
    }

    /// Primary orchestrator entry — resolves worker from dir + config manifest name.
    pub async fn fetch(
        &self,
        worker_dir: &Path,
        config: &WorkerConfig,
        req: SerializedRequest,
        kind_hint: Option<ExecutionKind>,
    ) -> Result<SerializedResponse, WorkerError> {
        self.ensure_active()?;

        let manifest = WorkerManifest {
            name: worker_dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("worker")
                .to_string(),
            ..Default::default()
        };
        let mut worker_ref =
            create_worker_ref(worker_dir.to_path_buf(), manifest).map_err(|e| {
                WorkerError::Isolation(edger_core::IsolationError::new(&e.code, e.message))
            })?;
        worker_ref.config = config.clone();

        let instance = self.get_or_create(&worker_ref)?;
        let kind = kind_hint
            .or(Some(worker_ref.kind.clone()))
            .or(config.kind.clone())
            .unwrap_or(ExecutionKind::FetchHandler);

        let isolate_arc = instance.isolate();
        let mut isolate = isolate_arc.lock().await;
        let res = dispatch_to_isolate(isolate.as_mut(), kind, req, config)
            .await
            .map_err(WorkerError::Isolation)?;
        Ok(res)
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.cache.clear();
        self.evicted.lock().expect("evicted lock").clear();
        self.sync_active_count();
    }

    pub fn get_metrics(&self) -> PoolMetrics {
        self.metrics.lock().expect("metrics lock").clone()
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

async fn dispatch_to_isolate<I: Isolate + ?Sized>(
    isolate: &mut I,
    kind: ExecutionKind,
    req: SerializedRequest,
    config: &WorkerConfig,
) -> Result<SerializedResponse, edger_core::IsolationError> {
    match kind {
        ExecutionKind::FetchHandler => isolate.execute_fetch(req, config).await,
        ExecutionKind::RoutesTable => isolate.execute_routes(req, config).await,
        ExecutionKind::StaticSpa { inject_base } => {
            let base = if inject_base {
                Some(req.base_href.as_deref().unwrap_or("/"))
            } else {
                None
            };
            isolate.serve_static_spa(&req.uri, base, config).await
        }
        ExecutionKind::WasmModule { .. } => isolate.execute_wasm(req, config).await,
        ExecutionKind::Fullstack { adapter } => Ok(SerializedResponse {
            status: 501,
            headers: vec![("x-adapter".into(), adapter)],
            body: Some(Bytes::from_static(b"fullstack not implemented")),
        }),
    }
}
