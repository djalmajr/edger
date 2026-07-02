//! WorkerPool — LRU cache + fetch entry point with supervisor integration.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use edger_core::{
    create_worker_ref, ExecutionKind, Isolate, SerializedRequest, SerializedResponse, WorkerConfig,
    WorkerManifest, WorkerRef,
};
use tracing::Instrument;
use uuid::Uuid;

use crate::ephemeral::EphemeralGate;
use crate::error::WorkerError;
use crate::factory::IsolateFactory;
use crate::instance::WorkerInstance;
use crate::lru::WorkerLru;
use crate::metrics::{MetricsCollector, PoolMetrics, WorkerStats};
use crate::state::WorkerState;
use crate::supervisor::Supervisor;
use crate::types::{PoolConfig, WorkerCacheKey};

struct WorkerPoolInner {
    #[allow(dead_code)]
    config: PoolConfig,
    cache: WorkerLru,
    factory: Arc<dyn IsolateFactory>,
    metrics: Arc<MetricsCollector>,
    ephemeral: EphemeralGate,
    evicted: Mutex<HashSet<WorkerCacheKey>>,
    shutdown: AtomicBool,
}

/// Shared worker pool — cheaply cloneable for TTL timer callbacks.
#[derive(Clone)]
pub struct WorkerPool {
    inner: Arc<WorkerPoolInner>,
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
        let metrics = Arc::new(MetricsCollector::default());
        let ephemeral = EphemeralGate::new(
            config.ephemeral_concurrency,
            config.ephemeral_queue_limit,
            Arc::clone(&metrics),
        );
        Self {
            inner: Arc::new(WorkerPoolInner {
                cache: WorkerLru::new(config.max_size),
                config,
                factory,
                metrics,
                ephemeral,
                evicted: Mutex::new(HashSet::new()),
                shutdown: AtomicBool::new(false),
            }),
        }
    }

    fn ensure_active(&self) -> Result<(), WorkerError> {
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return Err(WorkerError::Shutdown);
        }
        Ok(())
    }

    fn sync_worker_counts(&self) {
        let active = self.inner.cache.len();
        let idle = self.inner.cache.count_idle();
        self.inner.metrics.set_active_workers(active);
        self.inner.metrics.set_idle_workers(idle);
    }

    /// Resolve or create a pooled worker instance (new entries start in `Creating`).
    pub async fn get_or_create(
        &self,
        worker_ref: &WorkerRef,
    ) -> Result<Arc<WorkerInstance>, WorkerError> {
        self.ensure_active()?;
        let key = WorkerCacheKey::from_worker_ref(worker_ref);

        if self
            .inner
            .evicted
            .lock()
            .expect("evicted lock")
            .contains(&key)
        {
            return Err(WorkerError::Evicted);
        }

        if let Some(instance) = self.inner.cache.get(&key) {
            if instance.worker_ref.namespace != worker_ref.namespace {
                return Err(WorkerError::Collision {
                    key: format!("{key:?}"),
                    detail: "namespace mismatch for cache key".into(),
                });
            }
            if instance.state() == WorkerState::Terminated {
                return Err(WorkerError::Retired);
            }
            self.inner.metrics.record_hit();
            return Ok(instance);
        }

        let spawn_start = Instant::now();
        let isolate = self.inner.factory.create_isolate(worker_ref);
        let instance = Arc::new(WorkerInstance::new(worker_ref.clone(), isolate));

        if let Some(evicted_key) = self.inner.cache.insert(key.clone(), Arc::clone(&instance)) {
            if evicted_key == key {
                return Err(WorkerError::Collision {
                    key: format!("{key:?}"),
                    detail: "concurrent insert".into(),
                });
            }
            self.inner
                .evicted
                .lock()
                .expect("evicted lock")
                .insert(evicted_key);
        }

        let elapsed_ms = spawn_start.elapsed().as_millis().max(1) as u64;
        self.inner.metrics.record_miss();
        self.inner.metrics.record_spawn_latency(elapsed_ms);
        self.sync_worker_counts();
        Ok(instance)
    }

    /// Legacy pool entry that derives identity from the worker directory.
    pub async fn fetch(
        &self,
        worker_dir: &Path,
        config: &WorkerConfig,
        req: SerializedRequest,
        kind_hint: Option<ExecutionKind>,
    ) -> Result<SerializedResponse, WorkerError> {
        let mut config = config.clone();
        config.worker_dir = Some(worker_dir.to_path_buf());

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
        if let Some(kind) = config.kind.clone() {
            worker_ref.kind = kind;
        }
        worker_ref.config = config.clone();
        self.fetch_worker(&worker_ref, req, kind_hint).await
    }

    /// Fetch using a resolved worker identity from the orchestrator manifest index.
    pub async fn fetch_worker(
        &self,
        worker_ref: &WorkerRef,
        req: SerializedRequest,
        kind_hint: Option<ExecutionKind>,
    ) -> Result<SerializedResponse, WorkerError> {
        let span = tracing::info_span!(
            "pool.fetch",
            request_id = %req.request_id,
            worker_name = %worker_ref.name,
            worker_version = %worker_ref.version,
            worker_namespace = worker_ref.namespace.as_deref().unwrap_or("")
        );
        self.fetch_worker_inner(worker_ref, req, kind_hint)
            .instrument(span)
            .await
    }

    async fn fetch_worker_inner(
        &self,
        worker_ref: &WorkerRef,
        req: SerializedRequest,
        kind_hint: Option<ExecutionKind>,
    ) -> Result<SerializedResponse, WorkerError> {
        self.ensure_active()?;
        let started = Instant::now();

        let mut worker_ref = worker_ref.clone();
        let mut config = worker_ref.config.clone();
        config.worker_dir = Some(worker_ref.dir.clone());

        let _ephemeral_permit = if config.ttl_ms == 0 {
            Some(self.inner.ephemeral.acquire().await?)
        } else {
            None
        };
        worker_ref.config = config.clone();

        // Concurrent requests to the same worker share one cached instance and
        // queue on its dispatch lock. An ephemeral instance (ttl_ms == 0) is
        // terminated after each request, so a queued dispatcher can wake up
        // holding a lock on an already-terminated instance. When that happens,
        // re-resolve a fresh instance instead of failing the request.
        const MAX_RESOLVE_ATTEMPTS: usize = 32;
        let mut attempt = 0;
        let (instance, dispatch_guard) = loop {
            attempt += 1;
            let instance = match self.get_or_create(&worker_ref).await {
                Ok(instance) => instance,
                Err(WorkerError::Retired | WorkerError::Evicted)
                    if attempt < MAX_RESOLVE_ATTEMPTS =>
                {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(err) => return Err(err),
            };
            let dispatch_guard = instance.dispatch_lock().lock_owned().await;

            if instance.state() == WorkerState::Creating {
                let spawn_start = Instant::now();
                Supervisor::spawn(&instance).await?;
                self.inner
                    .metrics
                    .record_spawn_latency(spawn_start.elapsed().as_millis().max(1) as u64);
            }

            if crate::state::accepts_dispatch(instance.state()) {
                break (instance, dispatch_guard);
            }

            // A concurrent ephemeral dispatch terminated this shared instance
            // while we waited on its lock; drop it and resolve a fresh one.
            drop(dispatch_guard);
            if attempt >= MAX_RESOLVE_ATTEMPTS {
                return Err(WorkerError::NotReady);
            }
            tokio::task::yield_now().await;
        };
        let _dispatch_guard = dispatch_guard;

        Supervisor::on_request_start(&instance).await?;

        let kind = kind_hint
            .or(config.kind.clone())
            .or(Some(worker_ref.kind.clone()))
            .unwrap_or(ExecutionKind::FetchHandler);

        let isolate_arc = instance.isolate();
        let mut isolate = isolate_arc.lock().await;
        let res = dispatch_to_isolate(isolate.as_mut(), kind, req, &config).await;
        drop(isolate);

        let res = match res {
            Ok(res) => res,
            Err(err) => {
                // An isolate failure must not leave the instance stuck in
                // `Active`: recycle it so the next dispatch gets a fresh worker.
                let _ = Supervisor::on_critical_error(&instance).await;
                self.remove_instance(&instance);
                self.sync_worker_counts();
                return Err(WorkerError::Isolation(err));
            }
        };

        Supervisor::on_request_complete(instance, &config, self).await?;

        self.inner
            .metrics
            .record_request_duration(started.elapsed().as_millis() as u64);
        self.sync_worker_counts();
        Ok(res)
    }

    /// Remove a terminated/ephemeral worker from the LRU cache.
    pub fn remove_instance(&self, instance: &WorkerInstance) {
        let key = WorkerCacheKey::from_worker_ref(&instance.worker_ref);
        self.inner.cache.remove(&key);
        self.inner.metrics.record_terminated();
        self.sync_worker_counts();
    }

    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.cache.clear();
        self.inner.evicted.lock().expect("evicted lock").clear();
        self.sync_worker_counts();
    }

    pub fn get_metrics(&self) -> PoolMetrics {
        self.inner.metrics.snapshot()
    }

    pub fn get_worker_stats(&self, worker_id: Uuid) -> Option<WorkerStats> {
        self.inner
            .cache
            .find_by_worker_id(worker_id)
            .map(|instance| worker_stats_for_instance(instance.as_ref()))
    }

    pub fn worker_stats(&self) -> Vec<WorkerStats> {
        let mut workers = self
            .inner
            .cache
            .values_snapshot()
            .iter()
            .map(|instance| worker_stats_for_instance(instance.as_ref()))
            .collect::<Vec<_>>();
        workers.sort_by(|a, b| {
            a.app
                .cmp(&b.app)
                .then_with(|| a.worker_id.cmp(&b.worker_id))
        });
        workers
    }

    pub fn len(&self) -> usize {
        self.inner.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.cache.is_empty()
    }
}

fn worker_stats_for_instance(instance: &WorkerInstance) -> WorkerStats {
    WorkerStats {
        app: format!(
            "{}@{}",
            instance.worker_ref.name, instance.worker_ref.version
        ),
        name: instance.worker_ref.name.clone(),
        namespace: instance.worker_ref.namespace.clone(),
        request_count: instance.request_count(),
        state: instance.state(),
        unhealthy: instance.is_unhealthy(),
        uptime_seconds: instance.uptime_seconds(),
        version: instance.worker_ref.version.clone(),
        worker_id: instance.worker_ref.id,
    }
}

async fn dispatch_to_isolate<I: Isolate + ?Sized>(
    isolate: &mut I,
    kind: ExecutionKind,
    req: SerializedRequest,
    config: &WorkerConfig,
) -> Result<SerializedResponse, edger_core::IsolationError> {
    let execution_kind = execution_kind_label(&kind).to_string();
    let request_id = req.request_id.clone();
    async move {
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
    .instrument(tracing::debug_span!(
        "isolate.execute",
        request_id = %request_id,
        execution_kind = %execution_kind
    ))
    .await
}

fn execution_kind_label(kind: &ExecutionKind) -> &'static str {
    match kind {
        ExecutionKind::FetchHandler => "fetch",
        ExecutionKind::RoutesTable => "routes",
        ExecutionKind::StaticSpa { .. } => "static_spa",
        ExecutionKind::WasmModule { .. } => "wasm",
        ExecutionKind::Fullstack { .. } => "fullstack",
    }
}
