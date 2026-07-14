//! WorkerPool — LRU cache + fetch entry point with supervisor integration.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use edger_core::{
    create_worker_ref, BodyStream, ExecutionKind, Isolate, SerializedRequest, SerializedResponse,
    StreamedResponse, TerminationOutcome, WorkerConfig, WorkerManifest, WorkerRef, WorkerResponse,
};
use edger_isolation::{
    dispatch_fullstack_stream, execute_with_limits, try_serve_fullstack_asset, validate_request,
    ResourceLimits,
};
use tracing::Instrument;
use uuid::Uuid;

use crate::ephemeral::EphemeralGate;
use crate::error::WorkerError;
use crate::factory::IsolateFactory;
use crate::instance::WorkerInstance;
use crate::lru::{QueueEnterResult, ReservedSlot, WorkerGroup, WorkerLru};
use crate::metrics::{
    MetricsCollector, PoolMetrics, WorkerGroupIdentity, WorkerGroupMetrics, WorkerProcessMetrics,
    WorkerRecycleCause, WorkerRequestOutcome, WorkerStats,
};
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
    circuit_breakers: Mutex<HashMap<WorkerCacheKey, CircuitBreakerState>>,
    shutdown: AtomicBool,
    lifecycle_events: Option<LifecycleEventSender>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerLifecycleEventKind {
    DrainStarted,
    DrainCompleted,
    DrainTimedOut,
    Terminated,
}

#[derive(Clone, Debug)]
pub struct WorkerLifecycleEvent {
    pub kind: WorkerLifecycleEventKind,
    pub worker_ref: WorkerRef,
    pub process_id: Option<String>,
    pub drained_count: Option<u64>,
    pub duration_ms: Option<u64>,
    pub reason: &'static str,
}

pub type LifecycleEventSender = tokio::sync::mpsc::Sender<WorkerLifecycleEvent>;

#[derive(Default)]
struct CircuitBreakerState {
    consecutive_failures: u32,
    first_failure_at: Option<Instant>,
    open_until: Option<Instant>,
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
        Self::with_factory_and_lifecycle(config, factory, None)
    }

    pub fn with_factory_and_lifecycle(
        config: PoolConfig,
        factory: Arc<dyn IsolateFactory>,
        lifecycle_events: Option<LifecycleEventSender>,
    ) -> Self {
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
                circuit_breakers: Mutex::new(HashMap::new()),
                shutdown: AtomicBool::new(false),
                lifecycle_events,
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

    fn create_instance(&self, worker_ref: &WorkerRef) -> Arc<WorkerInstance> {
        let isolate = self.inner.factory.create_isolate(worker_ref);
        Arc::new(WorkerInstance::new(worker_ref.clone(), isolate))
    }

    fn create_group(&self, worker_ref: &WorkerRef) -> Arc<WorkerGroup> {
        let initial_processes = worker_ref
            .config
            .min_processes
            .max(1)
            .min(worker_ref.config.max_processes.max(1));
        let instances = (0..initial_processes)
            .map(|_| self.create_instance(worker_ref))
            .collect();
        Arc::new(WorkerGroup::new(instances))
    }

    fn worker_ref_with_dir(worker_ref: &WorkerRef) -> WorkerRef {
        let mut worker_ref = worker_ref.clone();
        worker_ref.config.worker_dir = Some(worker_ref.dir.clone());
        worker_ref
    }

    fn ensure_circuit_closed(&self, worker_ref: &WorkerRef) -> Result<(), WorkerError> {
        if worker_ref.config.circuit_breaker_failures == 0 {
            return Ok(());
        }

        let key = WorkerCacheKey::from_worker_ref(worker_ref);
        let now = Instant::now();
        let mut circuit_breakers = self.inner.circuit_breakers.lock().expect("breaker lock");
        let Some(state) = circuit_breakers.get_mut(&key) else {
            return Ok(());
        };
        let Some(open_until) = state.open_until else {
            return Ok(());
        };

        if now < open_until {
            let retry_after_ms = open_until.saturating_duration_since(now).as_millis() as u64;
            return Err(WorkerError::CircuitOpen { retry_after_ms });
        }

        circuit_breakers.remove(&key);
        Ok(())
    }

    fn record_spawn_success(&self, worker_ref: &WorkerRef) {
        let key = WorkerCacheKey::from_worker_ref(worker_ref);
        self.inner
            .circuit_breakers
            .lock()
            .expect("breaker lock")
            .remove(&key);
    }

    fn record_spawn_failure(&self, worker_ref: &WorkerRef) {
        let threshold = worker_ref.config.circuit_breaker_failures;
        if threshold == 0 {
            return;
        }

        let cooldown = Duration::from_millis(worker_ref.config.cooldown_ms);
        let key = WorkerCacheKey::from_worker_ref(worker_ref);
        let now = Instant::now();
        let mut circuit_breakers = self.inner.circuit_breakers.lock().expect("breaker lock");
        let state = circuit_breakers.entry(key).or_default();
        let starts_new_window = state
            .first_failure_at
            .is_none_or(|first| now.duration_since(first) > cooldown);

        if starts_new_window {
            state.first_failure_at = Some(now);
            state.consecutive_failures = 1;
            state.open_until = None;
        } else {
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        }

        if state.consecutive_failures >= threshold {
            state.open_until = Some(now + cooldown);
        }
    }

    async fn spawn_instance(&self, instance: &Arc<WorkerInstance>) -> Result<(), WorkerError> {
        if let Err(err) = self.ensure_circuit_closed(&instance.worker_ref) {
            self.remove_instance_with_cause(instance, WorkerRecycleCause::Error);
            self.sync_worker_counts();
            return Err(err);
        }
        let spawn_start = Instant::now();
        let result = Supervisor::spawn(instance).await;
        self.inner
            .metrics
            .record_spawn_latency(spawn_start.elapsed().as_millis().max(1) as u64);

        match result {
            Ok(()) => {
                self.record_spawn_success(&instance.worker_ref);
                Ok(())
            }
            Err(err) => {
                self.record_spawn_failure(&instance.worker_ref);
                self.remove_instance_with_cause(instance, WorkerRecycleCause::Error);
                self.sync_worker_counts();
                Err(err)
            }
        }
    }

    fn get_or_create_group(&self, worker_ref: &WorkerRef) -> Result<Arc<WorkerGroup>, WorkerError> {
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

        if let Some(group) = self.inner.cache.get_group(&key) {
            if let Some(instance) = group.instances_snapshot().first() {
                if instance.worker_ref.namespace != worker_ref.namespace {
                    return Err(WorkerError::Collision {
                        key: format!("{key:?}"),
                        detail: "namespace mismatch for cache key".into(),
                    });
                }
            }
            self.inner.metrics.record_hit();
            return Ok(group);
        }

        let spawn_start = Instant::now();
        let group = self.create_group(worker_ref);

        if let Some(instance) = group.instances_snapshot().first() {
            if instance.worker_ref.namespace != worker_ref.namespace {
                return Err(WorkerError::Collision {
                    key: format!("{key:?}"),
                    detail: "namespace mismatch for cache key".into(),
                });
            }
        }

        if let Some(evicted_key) = self
            .inner
            .cache
            .insert_group(key.clone(), Arc::clone(&group))
        {
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
        Ok(group)
    }

    pub async fn prewarm_worker(&self, worker_ref: &WorkerRef) -> Result<usize, WorkerError> {
        self.ensure_active()?;
        if worker_ref.config.min_processes == 0 || !worker_ref.kind.uses_process_backend() {
            return Ok(0);
        }

        let worker_ref = Self::worker_ref_with_dir(worker_ref);
        self.ensure_circuit_closed(&worker_ref)?;
        let target = worker_ref
            .config
            .min_processes
            .min(worker_ref.config.max_processes.max(1));
        let group = self.get_or_create_group(&worker_ref)?;
        let instances = group.ensure_min_processes(target, || {
            let spawn_start = Instant::now();
            let instance = self.create_instance(&worker_ref);
            self.inner.metrics.record_miss();
            self.inner
                .metrics
                .record_spawn_latency(spawn_start.elapsed().as_millis().max(1) as u64);
            instance
        });
        self.sync_worker_counts();

        let mut spawned = 0;
        for instance in instances {
            let dispatch_lock = instance.dispatch_lock();
            let _guard = dispatch_lock.lock_owned().await;
            if instance.state() == WorkerState::Creating {
                self.spawn_instance(&instance).await?;
                spawned += 1;
            }
            if instance.state() == WorkerState::Ready {
                instance.set_state(WorkerState::Idle);
            }
        }
        self.sync_worker_counts();
        Ok(spawned)
    }

    /// Resolve or create a pooled worker instance (new entries start in `Creating`).
    pub async fn get_or_create(
        &self,
        worker_ref: &WorkerRef,
    ) -> Result<Arc<WorkerInstance>, WorkerError> {
        let group = self.get_or_create_group(worker_ref)?;
        group
            .instances_snapshot()
            .into_iter()
            .find(|instance| instance.state() != WorkerState::Terminated)
            .ok_or(WorkerError::Retired)
    }

    async fn acquire_dispatch_slot(
        &self,
        worker_ref: &WorkerRef,
    ) -> Result<DispatchSlot, WorkerError> {
        self.ensure_circuit_closed(worker_ref)?;
        if worker_ref.config.ttl_ms == 0 {
            return self.acquire_ephemeral_dispatch_slot(worker_ref).await;
        }

        let group = self.get_or_create_group(worker_ref)?;
        let max_processes = worker_ref.config.max_processes.max(1);

        match group.reserve_slot_with_min(max_processes, worker_ref.config.min_processes, || {
            let spawn_start = Instant::now();
            let instance = self.create_instance(worker_ref);
            self.inner.metrics.record_miss();
            self.inner
                .metrics
                .record_spawn_latency(spawn_start.elapsed().as_millis().max(1) as u64);
            instance
        }) {
            ReservedSlot::Acquired { instance, guard } => {
                self.sync_worker_counts();
                Ok(DispatchSlot::new(instance, Arc::clone(&group), guard))
            }
            ReservedSlot::Wait(_) => {
                self.acquire_queued_dispatch_slot(worker_ref, group, max_processes)
                    .await
            }
            ReservedSlot::Unavailable if self.inner.shutdown.load(Ordering::SeqCst) => {
                Err(WorkerError::Shutdown)
            }
            ReservedSlot::Unavailable => Err(WorkerError::Retired),
        }
    }

    async fn acquire_ephemeral_dispatch_slot(
        &self,
        worker_ref: &WorkerRef,
    ) -> Result<DispatchSlot, WorkerError> {
        let group = self.get_or_create_group(worker_ref)?;

        match group.reserve_slot(usize::MAX, || {
            let spawn_start = Instant::now();
            let instance = self.create_instance(worker_ref);
            self.inner.metrics.record_miss();
            self.inner
                .metrics
                .record_spawn_latency(spawn_start.elapsed().as_millis().max(1) as u64);
            instance
        }) {
            ReservedSlot::Acquired { instance, guard } => {
                self.sync_worker_counts();
                Ok(DispatchSlot::new(instance, Arc::clone(&group), guard))
            }
            ReservedSlot::Wait(instance) => {
                let guard = instance.dispatch_lock().lock_owned().await;
                Ok(DispatchSlot::new(instance, group, guard))
            }
            ReservedSlot::Unavailable if self.inner.shutdown.load(Ordering::SeqCst) => {
                Err(WorkerError::Shutdown)
            }
            ReservedSlot::Unavailable => Err(WorkerError::Retired),
        }
    }

    async fn acquire_queued_dispatch_slot(
        &self,
        worker_ref: &WorkerRef,
        group: Arc<WorkerGroup>,
        max_processes: usize,
    ) -> Result<DispatchSlot, WorkerError> {
        let _waiter = match group.try_enter_queue(
            worker_ref.config.queue_limit,
            Arc::clone(&self.inner.metrics),
            WorkerGroupIdentity::from_worker_ref(worker_ref),
        ) {
            QueueEnterResult::Accepted(waiter) => {
                self.inner
                    .metrics
                    .record_worker_group_queue_enqueued(worker_ref, group.queued_waiters() as u64);
                waiter
            }
            QueueEnterResult::Closed => return Err(WorkerError::Shutdown),
            QueueEnterResult::Full => {
                self.inner.metrics.record_worker_queue_rejected();
                self.inner
                    .metrics
                    .record_worker_group_queue_rejected(worker_ref, group.queued_waiters() as u64);
                self.inner
                    .metrics
                    .record_worker_group_outcome(worker_ref, WorkerRequestOutcome::Rejected);
                return Err(WorkerError::WorkerQueueFull);
            }
        };
        self.inner.metrics.record_worker_queue_enqueued();
        let queued_start = Instant::now();

        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(worker_ref.config.queue_timeout_ms);
        loop {
            if self.inner.shutdown.load(Ordering::SeqCst) || group.is_closed() {
                self.inner.metrics.record_worker_group_queue_wait(
                    worker_ref,
                    group.queued_waiters().saturating_sub(1) as u64,
                    queued_start.elapsed().as_millis() as u64,
                );
                return Err(WorkerError::Shutdown);
            }

            match group.reserve_slot_with_min(
                max_processes,
                worker_ref.config.min_processes,
                || {
                    let spawn_start = Instant::now();
                    let instance = self.create_instance(worker_ref);
                    self.inner.metrics.record_miss();
                    self.inner
                        .metrics
                        .record_spawn_latency(spawn_start.elapsed().as_millis().max(1) as u64);
                    instance
                },
            ) {
                ReservedSlot::Acquired { instance, guard } => {
                    self.sync_worker_counts();
                    self.inner.metrics.record_worker_group_queue_wait(
                        worker_ref,
                        group.queued_waiters().saturating_sub(1) as u64,
                        queued_start.elapsed().as_millis() as u64,
                    );
                    return Ok(DispatchSlot::new(instance, Arc::clone(&group), guard));
                }
                ReservedSlot::Wait(_) => {}
                ReservedSlot::Unavailable if self.inner.shutdown.load(Ordering::SeqCst) => {
                    self.inner.metrics.record_worker_group_queue_wait(
                        worker_ref,
                        group.queued_waiters().saturating_sub(1) as u64,
                        queued_start.elapsed().as_millis() as u64,
                    );
                    return Err(WorkerError::Shutdown);
                }
                ReservedSlot::Unavailable => {
                    self.inner.metrics.record_worker_group_queue_wait(
                        worker_ref,
                        group.queued_waiters().saturating_sub(1) as u64,
                        queued_start.elapsed().as_millis() as u64,
                    );
                    return Err(WorkerError::Retired);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                self.inner.metrics.record_worker_queue_timeout();
                self.inner.metrics.record_worker_group_queue_timeout(
                    worker_ref,
                    group.queued_waiters().saturating_sub(1) as u64,
                    queued_start.elapsed().as_millis() as u64,
                );
                self.inner
                    .metrics
                    .record_worker_group_outcome(worker_ref, WorkerRequestOutcome::Timeout);
                return Err(WorkerError::WorkerQueueTimeout);
            }

            if tokio::time::timeout_at(deadline, group.wait_for_slot_release())
                .await
                .is_err()
            {
                self.inner.metrics.record_worker_queue_timeout();
                self.inner.metrics.record_worker_group_queue_timeout(
                    worker_ref,
                    group.queued_waiters().saturating_sub(1) as u64,
                    queued_start.elapsed().as_millis() as u64,
                );
                self.inner
                    .metrics
                    .record_worker_group_outcome(worker_ref, WorkerRequestOutcome::Timeout);
                return Err(WorkerError::WorkerQueueTimeout);
            }
        }
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

    /// Streaming pool entry (story 16.D): FetchHandler/RoutesTable on a
    /// streaming-capable isolate return `WorkerResponse::Streamed` whose body
    /// carries the dispatch guards until end-of-stream (clean completion) or
    /// drop (client disconnect -> instance recycled). Everything else falls
    /// back to the buffered path unchanged.
    pub async fn fetch_worker_stream(
        &self,
        worker_ref: &WorkerRef,
        req: SerializedRequest,
        kind_hint: Option<ExecutionKind>,
    ) -> Result<WorkerResponse, WorkerError> {
        let span = tracing::info_span!(
            "pool.fetch_stream",
            request_id = %req.request_id,
            worker_name = %worker_ref.name,
            worker_version = %worker_ref.version,
            worker_namespace = worker_ref.namespace.as_deref().unwrap_or("")
        );
        self.fetch_worker_stream_inner(worker_ref, req, kind_hint)
            .instrument(span)
            .await
    }

    async fn fetch_worker_stream_inner(
        &self,
        worker_ref: &WorkerRef,
        req: SerializedRequest,
        kind_hint: Option<ExecutionKind>,
    ) -> Result<WorkerResponse, WorkerError> {
        let kind = kind_hint
            .clone()
            .or(worker_ref.config.kind.clone())
            .unwrap_or(worker_ref.kind.clone());
        let streamable = kind.uses_process_backend();
        // Ephemeral workers (ttl 0) hold a lifetimed concurrency permit that
        // cannot travel inside a 'static body stream — they stay buffered.
        if !streamable || worker_ref.config.ttl_ms == 0 {
            return self
                .fetch_worker_inner(worker_ref, req, kind_hint)
                .await
                .map(WorkerResponse::Buffered);
        }

        self.ensure_active()?;
        let started = Instant::now();

        let mut worker_ref = worker_ref.clone();
        let mut config = worker_ref.config.clone();
        config.worker_dir = Some(worker_ref.dir.clone());
        worker_ref.config = config.clone();
        validate_request(&req, &config).map_err(WorkerError::Isolation)?;
        if matches!(kind, ExecutionKind::Fullstack { .. }) {
            if let Some(asset) =
                try_serve_fullstack_asset(&req, &config).map_err(WorkerError::Isolation)?
            {
                let duration_ms = started.elapsed().as_millis().max(1) as u64;
                self.record_worker_result(&worker_ref, duration_ms, asset.status);
                return Ok(WorkerResponse::Buffered(asset));
            }
        }

        const MAX_RESOLVE_ATTEMPTS: usize = 32;
        let mut attempt = 0;
        let dispatch_slot = loop {
            attempt += 1;
            let dispatch_slot = match self.acquire_dispatch_slot(&worker_ref).await {
                Ok(slot) => slot,
                Err(WorkerError::Retired | WorkerError::Evicted)
                    if attempt < MAX_RESOLVE_ATTEMPTS =>
                {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(err) => return Err(err),
            };
            let instance = Arc::clone(&dispatch_slot.instance);

            if instance.state() == WorkerState::Creating {
                self.spawn_instance(&instance).await?;
            }

            if crate::state::accepts_dispatch(instance.state()) {
                break dispatch_slot;
            }

            drop(dispatch_slot);
            if attempt >= MAX_RESOLVE_ATTEMPTS {
                return Err(WorkerError::NotReady);
            }
            tokio::task::yield_now().await;
        };
        let instance = Arc::clone(&dispatch_slot.instance);

        Supervisor::on_request_start(&instance).await?;

        let mut cancel_guard = DispatchCancelGuard {
            pool: self,
            instance: instance.clone(),
            armed: true,
        };

        let mut isolate_guard = instance.isolate().lock_owned().await;
        let res = match kind {
            ExecutionKind::RoutesTable => isolate_guard.execute_routes_stream(req, &config).await,
            ExecutionKind::Fullstack { .. } => {
                dispatch_fullstack_stream(isolate_guard.as_mut(), req, &config).await
            }
            _ => isolate_guard.execute_fetch_stream(req, &config).await,
        };

        match res {
            Ok(WorkerResponse::Buffered(res)) => {
                drop(isolate_guard);
                Supervisor::on_request_complete(instance, &config, self).await?;
                cancel_guard.armed = false;
                let duration_ms = started.elapsed().as_millis().max(1) as u64;
                self.inner.metrics.record_request_duration(duration_ms);
                self.inner
                    .metrics
                    .record_worker_group_request(&worker_ref, duration_ms);
                self.inner.metrics.record_worker_group_outcome(
                    &worker_ref,
                    request_outcome_for_status(res.status),
                );
                self.sync_worker_counts();
                Ok(WorkerResponse::Buffered(res))
            }
            Ok(WorkerResponse::Streamed(streamed)) => {
                // The guards move INTO the body: the instance stays Active and
                // the process exclusive until the stream ends or is dropped.
                cancel_guard.armed = false;
                let state = StreamDispatchState {
                    pool: self.clone(),
                    instance,
                    config,
                    outcome: request_outcome_for_status(streamed.status),
                    started,
                    _dispatch_slot: dispatch_slot,
                    isolate_guard: Some(isolate_guard),
                };
                Ok(WorkerResponse::Streamed(StreamedResponse {
                    status: streamed.status,
                    headers: streamed.headers,
                    body: Box::pin(GuardedBody {
                        inner: streamed.body,
                        state: Some(state),
                    }),
                }))
            }
            Err(err) => {
                drop(isolate_guard);
                cancel_guard.armed = false;
                let _ = Supervisor::on_critical_error(&instance, self).await;
                self.remove_instance(&instance);
                let duration_ms = started.elapsed().as_millis().max(1) as u64;
                self.inner.metrics.record_request_duration(duration_ms);
                self.inner
                    .metrics
                    .record_worker_group_request(&worker_ref, duration_ms);
                self.inner
                    .metrics
                    .record_worker_group_outcome(&worker_ref, WorkerRequestOutcome::IsolationError);
                self.sync_worker_counts();
                Err(WorkerError::Isolation(err))
            }
        }
    }

    async fn fetch_worker_inner(
        &self,
        worker_ref: &WorkerRef,
        req: SerializedRequest,
        kind_hint: Option<ExecutionKind>,
    ) -> Result<SerializedResponse, WorkerError> {
        self.ensure_active()?;
        let started = Instant::now();
        let record_observation = !is_health_check_request(&req);

        let mut worker_ref = worker_ref.clone();
        let mut config = worker_ref.config.clone();
        config.worker_dir = Some(worker_ref.dir.clone());
        worker_ref.config = config.clone();
        validate_request(&req, &config).map_err(WorkerError::Isolation)?;

        let kind = kind_hint
            .clone()
            .or(config.kind.clone())
            .or(Some(worker_ref.kind.clone()))
            .unwrap_or(ExecutionKind::FetchHandler);
        if let ExecutionKind::StaticSpa { inject_base } = &kind {
            let response = serve_static_spa_request(&req, &config, *inject_base)?;
            let duration_ms = started.elapsed().as_millis().max(1) as u64;
            if record_observation {
                self.record_worker_result(&worker_ref, duration_ms, response.status);
            }
            return Ok(response);
        }
        if matches!(kind, ExecutionKind::Fullstack { .. }) {
            if let Some(asset) =
                try_serve_fullstack_asset(&req, &config).map_err(WorkerError::Isolation)?
            {
                let duration_ms = started.elapsed().as_millis().max(1) as u64;
                if record_observation {
                    self.record_worker_result(&worker_ref, duration_ms, asset.status);
                }
                return Ok(asset);
            }
        }

        let _ephemeral_permit = if config.ttl_ms == 0 {
            Some(self.inner.ephemeral.acquire().await?)
        } else {
            None
        };

        // Concurrent requests to the same worker share one cached instance and
        // queue on its dispatch lock. An ephemeral instance (ttl_ms == 0) is
        // terminated after each request, so a queued dispatcher can wake up
        // holding a lock on an already-terminated instance. When that happens,
        // re-resolve a fresh instance instead of failing the request.
        const MAX_RESOLVE_ATTEMPTS: usize = 32;
        let mut attempt = 0;
        let dispatch_slot = loop {
            attempt += 1;
            let dispatch_slot = match self.acquire_dispatch_slot(&worker_ref).await {
                Ok(slot) => slot,
                Err(WorkerError::Retired | WorkerError::Evicted)
                    if attempt < MAX_RESOLVE_ATTEMPTS =>
                {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(err) => return Err(err),
            };
            let instance = Arc::clone(&dispatch_slot.instance);

            if instance.state() == WorkerState::Creating {
                self.spawn_instance(&instance).await?;
            }

            if crate::state::accepts_dispatch(instance.state()) {
                break dispatch_slot;
            }

            // A concurrent ephemeral dispatch terminated this shared instance
            // while we waited on its lock; drop it and resolve a fresh one.
            drop(dispatch_slot);
            if attempt >= MAX_RESOLVE_ATTEMPTS {
                return Err(WorkerError::NotReady);
            }
            tokio::task::yield_now().await;
        };
        let instance = Arc::clone(&dispatch_slot.instance);
        let _dispatch_slot = dispatch_slot;

        Supervisor::on_request_start(&instance).await?;

        // Cancellation-safety: if this future is dropped while a dispatch is in
        // flight (e.g. the HTTP client disconnected mid-request — easy to hit
        // with a multi-second streaming response), `on_request_complete` never
        // runs and the instance would be stuck `Active` forever, wedging the
        // worker so every later request fails with `NotReady`. This guard
        // recycles the instance on any unclean exit; it is disarmed once we
        // reach a normal completion or the explicit error path below.
        let mut cancel_guard = DispatchCancelGuard {
            pool: self,
            instance: instance.clone(),
            armed: true,
        };

        let isolate_arc = instance.isolate();
        let mut isolate = isolate_arc.lock().await;
        let res = dispatch_to_isolate(isolate.as_mut(), kind, req, &config).await;
        drop(isolate);

        let res = match res {
            Ok(res) => res,
            Err(err) => {
                cancel_guard.armed = false;
                // An isolate failure must not leave the instance stuck in
                // `Active`: recycle it so the next dispatch gets a fresh worker.
                let _ = Supervisor::on_critical_error(&instance, self).await;
                self.remove_instance(&instance);
                if record_observation {
                    let duration_ms = started.elapsed().as_millis().max(1) as u64;
                    self.inner.metrics.record_request_duration(duration_ms);
                    self.inner
                        .metrics
                        .record_worker_group_request(&worker_ref, duration_ms);
                    self.inner.metrics.record_worker_group_outcome(
                        &worker_ref,
                        WorkerRequestOutcome::IsolationError,
                    );
                }
                self.sync_worker_counts();
                return Err(WorkerError::Isolation(err));
            }
        };

        Supervisor::on_request_complete(instance, &config, self).await?;
        cancel_guard.armed = false;

        if record_observation {
            let duration_ms = started.elapsed().as_millis().max(1) as u64;
            self.inner.metrics.record_request_duration(duration_ms);
            self.inner
                .metrics
                .record_worker_group_request(&worker_ref, duration_ms);
            self.inner
                .metrics
                .record_worker_group_outcome(&worker_ref, request_outcome_for_status(res.status));
        }
        self.sync_worker_counts();
        Ok(res)
    }

    fn record_worker_result(&self, worker_ref: &WorkerRef, duration_ms: u64, status: u16) {
        self.inner.metrics.record_request_duration(duration_ms);
        self.inner
            .metrics
            .record_worker_group_request(worker_ref, duration_ms);
        self.inner
            .metrics
            .record_worker_group_outcome(worker_ref, request_outcome_for_status(status));
    }

    /// Force-recycle a worker whose dispatch was cancelled mid-flight: mark it
    /// non-dispatchable and evict it so a fresh instance (and process) is
    /// spawned next time, instead of leaving it wedged in `Active`.
    fn recycle_cancelled(&self, instance: &Arc<WorkerInstance>) {
        instance.set_state(WorkerState::Terminated);
        self.remove_instance_with_cause(instance, WorkerRecycleCause::Error);
    }

    /// Remove a terminated/ephemeral worker from the LRU cache.
    pub fn remove_instance(&self, instance: &WorkerInstance) {
        let cause = infer_recycle_cause(instance);
        self.remove_instance_with_cause(instance, cause);
    }

    fn remove_instance_with_cause(&self, instance: &WorkerInstance, cause: WorkerRecycleCause) {
        let key = WorkerCacheKey::from_worker_ref(&instance.worker_ref);
        self.inner.cache.remove_instance(&key, instance.id());
        self.inner
            .metrics
            .record_worker_group_recycle(&instance.worker_ref, cause);
        self.inner.metrics.record_terminated();
        self.sync_worker_counts();
    }

    pub(crate) async fn terminate_isolate_with_lifecycle(
        &self,
        instance: &WorkerInstance,
        reason: &'static str,
    ) {
        emit_lifecycle(
            self.inner.lifecycle_events.as_ref(),
            WorkerLifecycleEvent {
                kind: WorkerLifecycleEventKind::DrainStarted,
                worker_ref: instance.worker_ref.clone(),
                process_id: None,
                drained_count: None,
                duration_ms: None,
                reason,
            },
        );
        let started = Instant::now();
        let isolate = instance.isolate();
        let report = match tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, isolate.lock()).await {
            Ok(mut guard) => guard.terminate_with_report().await.ok(),
            Err(_) => None,
        };
        let timed_out = report.is_none()
            || report
                .as_ref()
                .is_some_and(|report| report.outcome == TerminationOutcome::TimedOut);
        let duration_ms = started.elapsed().as_millis() as u64;
        emit_lifecycle(
            self.inner.lifecycle_events.as_ref(),
            WorkerLifecycleEvent {
                kind: if timed_out {
                    WorkerLifecycleEventKind::DrainTimedOut
                } else {
                    WorkerLifecycleEventKind::DrainCompleted
                },
                worker_ref: instance.worker_ref.clone(),
                process_id: report.as_ref().and_then(|report| report.process_id.clone()),
                drained_count: report.as_ref().and_then(|report| report.drained_count),
                duration_ms: Some(duration_ms),
                reason,
            },
        );
        emit_lifecycle(
            self.inner.lifecycle_events.as_ref(),
            WorkerLifecycleEvent {
                kind: WorkerLifecycleEventKind::Terminated,
                worker_ref: instance.worker_ref.clone(),
                process_id: report.and_then(|report| report.process_id),
                drained_count: None,
                duration_ms: Some(duration_ms),
                reason: if timed_out { "drain_timeout" } else { reason },
            },
        );
    }

    /// Begins graceful shutdown and returns the drain task handle (when a Tokio
    /// runtime is present) so the caller can AWAIT the beforeunload/waitUntil drain
    /// before exiting. Fire-and-forget would let the process exit mid-drain.
    pub fn shutdown(&self) -> Option<tokio::task::JoinHandle<()>> {
        if self.inner.shutdown.swap(true, Ordering::SeqCst) {
            return None;
        }

        let groups = self.inner.cache.groups_snapshot();
        for group in &groups {
            group.close_queue();
        }
        let instances = groups
            .iter()
            .flat_map(|group| group.instances_snapshot())
            .collect::<Vec<_>>();
        for instance in &instances {
            self.inner
                .metrics
                .record_worker_group_recycle(&instance.worker_ref, WorkerRecycleCause::OomShutdown);
        }

        self.inner.cache.clear();
        self.inner.evicted.lock().expect("evicted lock").clear();
        self.inner
            .circuit_breakers
            .lock()
            .expect("breaker lock")
            .clear();
        self.sync_worker_counts();

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            Some(handle.spawn(shutdown_instances_after_drain(
                instances,
                self.inner.lifecycle_events.clone(),
            )))
        } else {
            for instance in instances {
                instance.cancel_ttl_timer();
                instance.set_state(WorkerState::Terminated);
            }
            None
        }
    }

    pub fn get_metrics(&self) -> PoolMetrics {
        let mut metrics = self.inner.metrics.snapshot();
        metrics.worker_groups = self.worker_group_metrics();
        metrics
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

fn is_health_check_request(request: &SerializedRequest) -> bool {
    request
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("x-edger-health-check"))
}

fn infer_recycle_cause(instance: &WorkerInstance) -> WorkerRecycleCause {
    if instance.is_unhealthy() {
        return WorkerRecycleCause::Error;
    }
    if instance.worker_ref.config.max_requests > 0
        && instance.request_count() >= instance.worker_ref.config.max_requests
    {
        return WorkerRecycleCause::MaxRequests;
    }
    WorkerRecycleCause::Ttl
}

fn merge_worker_group_metrics(
    mut live: BTreeMap<WorkerGroupIdentity, WorkerGroupMetrics>,
    runtime: BTreeMap<WorkerGroupIdentity, crate::metrics::WorkerGroupRuntimeMetrics>,
) -> Vec<WorkerGroupMetrics> {
    for (identity, counters) in runtime {
        let group = live
            .entry(identity.clone())
            .or_insert_with(|| WorkerGroupMetrics {
                name: identity.name,
                namespace: identity.namespace,
                version: identity.version,
                ..Default::default()
            });
        group.enqueued_total = counters.enqueued_total;
        group.health = counters.health_at(crate::metrics::unix_time_ms());
        group.queued = counters.queued;
        group.recycle_error_total = counters.recycle_error_total;
        group.recycle_max_requests_total = counters.recycle_max_requests_total;
        group.recycle_oom_shutdown_total = counters.recycle_oom_shutdown_total;
        group.recycle_ttl_total = counters.recycle_ttl_total;
        group.rejected_total = counters.rejected_total;
        group.request_duration_ms_last = counters.request_duration_ms_last;
        group.request_duration_ms_p95 = counters.request_duration_ms_p95;
        group.request_total = counters.request_total;
        group.timeout_total = counters.timeout_total;
        group.wait_ms_last = counters.wait_ms_last;
        group.wait_ms_p50 = counters.wait_ms_p50;
        group.wait_ms_p95 = counters.wait_ms_p95;
    }
    live.into_values().collect()
}

impl WorkerPool {
    fn worker_group_metrics(&self) -> Vec<WorkerGroupMetrics> {
        let mut live = BTreeMap::new();
        for group in self.inner.cache.groups_snapshot() {
            let instances = group.instances_snapshot();
            let Some(first) = instances.first() else {
                continue;
            };
            let identity = WorkerGroupIdentity::from_worker_ref(&first.worker_ref);
            let processes = instances
                .iter()
                .map(|instance| WorkerProcessMetrics {
                    request_count: instance.request_count(),
                    state: instance.state(),
                    unhealthy: instance.is_unhealthy(),
                    uptime_seconds: instance.uptime_seconds(),
                })
                .collect::<Vec<_>>();
            let active_processes = processes
                .iter()
                .filter(|process| process.state == WorkerState::Active)
                .count();
            let idle_processes = processes
                .iter()
                .filter(|process| process.state == WorkerState::Idle)
                .count();
            let terminating_processes = processes
                .iter()
                .filter(|process| process.state == WorkerState::Terminating)
                .count();
            live.insert(
                identity,
                WorkerGroupMetrics {
                    active_processes,
                    idle_processes,
                    max_processes: first.worker_ref.config.max_processes.max(1),
                    name: first.worker_ref.name.clone(),
                    namespace: first.worker_ref.namespace.clone(),
                    processes,
                    queued: group.queued_waiters() as u64,
                    terminating_processes,
                    total_processes: instances.len(),
                    version: first.worker_ref.version.clone(),
                    ..Default::default()
                },
            );
        }

        merge_worker_group_metrics(live, self.inner.metrics.worker_group_runtime_snapshots())
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
        worker_id: instance.id(),
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
    let limits = ResourceLimits::from_config(config);
    execute_with_limits(isolate, kind, req, config, &limits)
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

fn serve_static_spa_request(
    req: &SerializedRequest,
    config: &WorkerConfig,
    inject_base: bool,
) -> Result<SerializedResponse, WorkerError> {
    let base = if inject_base {
        Some(req.base_href.as_deref().unwrap_or("/"))
    } else {
        None
    };
    edger_isolation::static_spa::serve_static_spa(&req.uri, base, config)
        .map_err(WorkerError::Isolation)
}

/// Dispatch context that travels inside a streamed response body: it keeps the
/// instance `Active` and the isolate/dispatch locks held until the stream ends
/// (clean completion -> back to `Idle`) or is dropped mid-flight (client
/// disconnect -> the process socket is desynced, recycle everything).
struct StreamDispatchState {
    pool: WorkerPool,
    instance: Arc<WorkerInstance>,
    config: WorkerConfig,
    outcome: WorkerRequestOutcome,
    started: Instant,
    _dispatch_slot: DispatchSlot,
    isolate_guard: Option<tokio::sync::OwnedMutexGuard<Box<dyn Isolate>>>,
}

struct DispatchSlot {
    instance: Arc<WorkerInstance>,
    group: Arc<WorkerGroup>,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl DispatchSlot {
    fn new(
        instance: Arc<WorkerInstance>,
        group: Arc<WorkerGroup>,
        guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> Self {
        Self {
            instance,
            group,
            guard: Some(guard),
        }
    }
}

impl Drop for DispatchSlot {
    fn drop(&mut self) {
        drop(self.guard.take());
        self.group.notify_slot_released();
    }
}

/// Body stream wrapper enforcing the lifecycle above.
struct GuardedBody {
    inner: BodyStream,
    state: Option<StreamDispatchState>,
}

impl futures_core::Stream for GuardedBody {
    type Item = Result<Bytes, edger_core::IsolationError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(chunk))) => std::task::Poll::Ready(Some(Ok(chunk))),
            std::task::Poll::Ready(Some(Err(err))) => {
                if let Some(state) = self.state.take() {
                    tokio::spawn(recycle_stream_state(state));
                }
                std::task::Poll::Ready(Some(Err(err)))
            }
            std::task::Poll::Ready(None) => {
                if let Some(state) = self.state.take() {
                    tokio::spawn(complete_stream_state(state));
                }
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl Drop for GuardedBody {
    fn drop(&mut self) {
        // Dropped before end-of-stream: the client disconnected while frames
        // were in flight — the process socket cannot be reused.
        if let Some(state) = self.state.take() {
            tokio::spawn(recycle_stream_state(state));
        }
    }
}

/// Clean end-of-stream: release the isolate, transition Active -> Idle, record
/// metrics — the streamed equivalent of the buffered completion path.
async fn complete_stream_state(state: StreamDispatchState) {
    let StreamDispatchState {
        pool,
        instance,
        config,
        outcome,
        started,
        _dispatch_slot,
        isolate_guard,
    } = state;
    drop(isolate_guard);
    let worker_ref = instance.worker_ref.clone();
    let _ = Supervisor::on_request_complete(instance, &config, &pool).await;
    let duration_ms = started.elapsed().as_millis().max(1) as u64;
    pool.inner.metrics.record_request_duration(duration_ms);
    pool.inner
        .metrics
        .record_worker_group_request(&worker_ref, duration_ms);
    pool.inner
        .metrics
        .record_worker_group_outcome(&worker_ref, outcome);
    pool.sync_worker_counts();
}

/// Abnormal end (mid-stream error or client disconnect): the process socket is
/// desynced — terminate the isolate and evict the instance so the next request
/// gets a fresh process.
async fn recycle_stream_state(mut state: StreamDispatchState) {
    let worker_ref = state.instance.worker_ref.clone();
    let duration_ms = state.started.elapsed().as_millis().max(1) as u64;
    drop(state.isolate_guard.take());
    state
        .pool
        .terminate_isolate_with_lifecycle(&state.instance, "stream_recycle")
        .await;
    state.pool.recycle_cancelled(&state.instance);
    state
        .pool
        .inner
        .metrics
        .record_worker_group_request(&worker_ref, duration_ms);
    state
        .pool
        .inner
        .metrics
        .record_worker_group_outcome(&worker_ref, WorkerRequestOutcome::IsolationError);
    state.pool.sync_worker_counts();
}

fn request_outcome_for_status(status: u16) -> WorkerRequestOutcome {
    if status >= 500 {
        WorkerRequestOutcome::Http5xx
    } else {
        WorkerRequestOutcome::Success
    }
}

const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

async fn shutdown_instances_after_drain(
    instances: Vec<Arc<WorkerInstance>>,
    lifecycle_events: Option<LifecycleEventSender>,
) {
    for instance in instances {
        instance.cancel_ttl_timer();
        emit_lifecycle(
            lifecycle_events.as_ref(),
            WorkerLifecycleEvent {
                kind: WorkerLifecycleEventKind::DrainStarted,
                worker_ref: instance.worker_ref.clone(),
                process_id: None,
                drained_count: None,
                duration_ms: None,
                reason: "shutdown",
            },
        );
        let started = Instant::now();
        let dispatch_lock = instance.dispatch_lock();
        let dispatch_drained =
            tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, dispatch_lock.lock_owned())
                .await
                .ok();
        let dispatch_timed_out = dispatch_drained.is_none();
        drop(dispatch_drained);
        terminate_shutdown_instance(
            instance,
            lifecycle_events.as_ref(),
            started,
            dispatch_timed_out,
        )
        .await;
    }
}

async fn terminate_shutdown_instance(
    instance: Arc<WorkerInstance>,
    lifecycle_events: Option<&LifecycleEventSender>,
    started: Instant,
    dispatch_timed_out: bool,
) {
    if instance.state() == WorkerState::Terminated {
        return;
    }

    instance.set_state(WorkerState::Terminating);
    let isolate = instance.isolate();
    let report = match tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, isolate.lock()).await {
        Ok(mut guard) => guard.terminate_with_report().await.ok(),
        Err(_) => None,
    };
    instance.set_state(WorkerState::Terminated);
    let timed_out = dispatch_timed_out
        || report.is_none()
        || report
            .as_ref()
            .is_some_and(|report| report.outcome == TerminationOutcome::TimedOut);
    let duration_ms = started.elapsed().as_millis() as u64;
    emit_lifecycle(
        lifecycle_events,
        WorkerLifecycleEvent {
            kind: if timed_out {
                WorkerLifecycleEventKind::DrainTimedOut
            } else {
                WorkerLifecycleEventKind::DrainCompleted
            },
            worker_ref: instance.worker_ref.clone(),
            process_id: report.as_ref().and_then(|report| report.process_id.clone()),
            drained_count: report.as_ref().and_then(|report| report.drained_count),
            duration_ms: Some(duration_ms),
            reason: "shutdown",
        },
    );
    emit_lifecycle(
        lifecycle_events,
        WorkerLifecycleEvent {
            kind: WorkerLifecycleEventKind::Terminated,
            worker_ref: instance.worker_ref.clone(),
            process_id: report.and_then(|report| report.process_id),
            drained_count: None,
            duration_ms: Some(duration_ms),
            reason: if timed_out {
                "drain_timeout"
            } else {
                "shutdown"
            },
        },
    );
}

fn emit_lifecycle(sender: Option<&LifecycleEventSender>, event: WorkerLifecycleEvent) {
    if let Some(sender) = sender {
        let _ = sender.try_send(event);
    }
}

/// RAII guard that recycles an `Active` instance if the dispatch future is
/// dropped before it completes (cancellation, e.g. an HTTP client disconnect).
/// Disarmed on the normal completion and explicit-error paths, so it only fires
/// on an otherwise-silent cancellation.
struct DispatchCancelGuard<'a> {
    pool: &'a WorkerPool,
    instance: Arc<WorkerInstance>,
    armed: bool,
}

impl Drop for DispatchCancelGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.pool.recycle_cancelled(&self.instance);
        }
    }
}
