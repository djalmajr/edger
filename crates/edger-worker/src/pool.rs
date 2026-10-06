//! WorkerPool — LRU cache + fetch entry point with supervisor integration.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use edger_core::{
    create_worker_ref, AbandonedStream, BodyStream, ExecutionKind, Isolate, SerializedRequest,
    SerializedResponse, StreamCompletion, StreamedResponse, TerminationOutcome, WorkerConfig,
    WorkerManifest, WorkerRef, WorkerResponse,
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
use crate::lru::{GroupInsertOutcome, QueueEnterResult, ReservedSlot, WorkerGroup, WorkerLru};
use crate::metrics::{
    MetricsCollector, PoolMetrics, WorkerGroupIdentity, WorkerGroupMetrics, WorkerProcessMetrics,
    WorkerRecycleCause, WorkerRequestOutcome, WorkerStats,
};
use crate::state::{transition, WorkerEvent, WorkerState};
use crate::supervisor::{Supervisor, TtlFloorDecision};
use crate::types::{PoolConfig, WorkerCacheKey};

/// (EDG-10 review P2 #1) Whether a removal may trigger a min-processes
/// replenishment attempt. Explicit on the removal itself — never inferred
/// from timing or instance state.
enum ReplenishPolicy {
    /// Regular removals (TTL expiry, `max_requests`, eviction cleanup, ...):
    /// when the group drops below `min_processes`, one background attempt is
    /// scheduled.
    Regular,
    /// A placeholder whose SPAWN failed (the error branch of
    /// `spawn_instance`): the failed attempt must NEVER schedule its
    /// successor — an always-failing `prepare` would otherwise retry
    /// forever, with no traffic and no backoff, whenever the circuit
    /// breaker is disabled (`circuit_breaker_failures = 0`). The next
    /// request or removal re-tries through the demand paths.
    SpawnFailed,
}

/// (EDG-10 rev3) Test seam for the min-processes admission section.
/// Compiled ONLY with the `test-hooks` feature (enabled by this
/// package's own test targets) and inert unless a test arms it: it
/// lets a test pause the admission section — after generation
/// revalidation, before placeholder admission — while it still holds
/// the `close_admission` lock, so a close path fired from another task
/// must wait on the held section; after release, the admitted
/// placeholder must be in the close-time drained set.
#[cfg(feature = "test-hooks")]
#[derive(Default)]
pub struct AdmissionSectionSeam {
    /// The next admission section pauses after validation (sticky until
    /// released).
    armed: AtomicBool,
    /// True while an admission section is paused (holding
    /// `close_admission`).
    paused: AtomicBool,
    /// Set by the test (or the release guard's Drop) to release a paused
    /// section.
    released: AtomicBool,
    /// True when a hold gave up on the deadline WITHOUT being released —
    /// the test consults this and fails: a held section that is never
    /// released means the test lost its own release path.
    timed_out: AtomicBool,
}

/// (EDG-10 rev4) Scope guard that releases the admission seam on DROP:
/// a failed assertion (unwinding) still drops the guard, so a test failure
/// never leaves the replenishment attempt parked in
/// [`AdmissionSectionSeam::hold_section_if_armed`]. Release it explicitly
/// at the intended point; `Drop` is the safety net.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub struct AdmissionSectionReleaseGuard<'a> {
    seam: &'a AdmissionSectionSeam,
}

#[cfg(feature = "test-hooks")]
impl<'a> AdmissionSectionReleaseGuard<'a> {
    /// Take over release responsibility for this seam.
    pub fn new(seam: &'a AdmissionSectionSeam) -> Self {
        Self { seam }
    }

    /// Release at the intended point (idempotent; `Drop` repeats it).
    pub fn release(&self) {
        self.seam.release();
    }
}

#[cfg(feature = "test-hooks")]
impl Drop for AdmissionSectionReleaseGuard<'_> {
    fn drop(&mut self) {
        self.seam.release();
    }
}

#[cfg(feature = "test-hooks")]
impl AdmissionSectionSeam {
    /// Arm: the next admission section of THIS pool pauses after
    /// generation revalidation, before placeholder admission. Re-arming
    /// resets the release and timeout flags. Must happen BEFORE the
    /// removal that schedules the attempt (the attempt may run on another
    /// worker of a multi-threaded runtime at any moment).
    pub fn arm(&self) {
        self.released.store(false, Ordering::SeqCst);
        self.timed_out.store(false, Ordering::SeqCst);
        self.armed.store(true, Ordering::SeqCst);
    }

    /// True while an admission section is paused on this seam.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Release the paused section: it completes the admission and drops
    /// the `close_admission` lock. Idempotent.
    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
    }

    /// True when a hold gave up on its deadline without being released.
    pub fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }

    /// Called from the admission section (under `close_admission`): pauses
    /// while armed. Polls with short REAL-TIME sleeps (no `await`) so the
    /// section's lock guard is never held across an await (the attempt's
    /// future stays `Send` for `tokio::spawn`): while this thread holds
    /// the section, the close task blocks on `close_admission` on its own
    /// worker and the test that will release runs on another — the test
    /// needs at least 3 workers (spinner + blocked close + test).
    ///
    /// BOUNDED: the hold gives up after 10s of real time, recording the
    /// timeout in `timed_out` (the test fails on it). `Runtime::drop`
    /// waits for spawned tasks, so an unbounded hold after a failed test
    /// would hang the whole test binary; the deadline bounds that. On
    /// success the release (explicit or via the guard's `Drop`) arrives in
    /// milliseconds — the budget only bounds failures.
    pub(crate) fn hold_section_if_armed(&self) {
        if !self.armed.load(Ordering::SeqCst) {
            return;
        }
        self.paused.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !self.released.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= deadline {
                self.timed_out.store(true, Ordering::SeqCst);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        self.paused.store(false, Ordering::SeqCst);
    }
}

struct WorkerPoolInner {
    #[allow(dead_code)]
    config: PoolConfig,
    cache: WorkerLru,
    factory: Arc<dyn IsolateFactory>,
    metrics: Arc<MetricsCollector>,
    ephemeral: EphemeralGate,
    circuit_breakers: Mutex<HashMap<WorkerCacheKey, CircuitBreakerState>>,
    shutdown: AtomicBool,
    /// (EDG-10) Serializes the atomic TTL-floor decisions (count of living
    /// group members + the `Idle -> Terminating` transition): two timers
    /// expiring together must each observe the other's decision, so the
    /// `min_processes` floor can never be breached by a simultaneous burst.
    ttl_decision: Mutex<()>,
    /// (EDG-10 rev2 P2 #2) Serializes group CLOSE (shutdown / recycle:
    /// `close_queue` + drained-set snapshot) with min-processes REPLENISH
    /// admission (generation revalidation + `ensure_min_processes`): a
    /// close that lands after the revalidation but before admission is
    /// impossible — it is queued behind the same lock — and a close before
    /// it is seen by the admission's `closed`/`shutdown` checks. No `await`
    /// is ever held under this lock.
    close_admission: Mutex<()>,
    /// (EDG-10 rev3) Test seam for the admission section (see
    /// [`AdmissionSectionSeam`]); not compiled without `test-hooks`.
    #[cfg(feature = "test-hooks")]
    admission_seam: AdmissionSectionSeam,
    lifecycle_events: Option<LifecycleEventSender>,
    /// Abandon-drain policy (EDG-9): a body dropped or errored BEFORE
    /// production completed waits at most `abandon_drain.drain_wait()` for
    /// the completion signal (the isolate's in-flight discard drain) before
    /// recycling. Mirrors the isolates' `EDGER_STREAM_ABANDON_DRAIN_MAX_*`;
    /// the slot stays held during the wait (short and bounded).
    abandon_drain: AbandonDrainLimits,
}

/// Grace added to the abandon-drain budget when the pool waits for the
/// completion signal on an early body drop/error (EDG-9). The drain's own
/// deadline is measured from the moment the READER enters discard mode —
/// the consumer loss — which happens before the body's drop observes it and
/// spawns the wait; the grace covers that handoff plus task scheduling, so
/// a drain that finishes on time always reaches the pool in time.
const ABANDON_DRAIN_GRACE_MS: u64 = 250;

/// The EDG-9 abandon-drain policy the pool mirrors (it must match the
/// isolates' `EDGER_STREAM_ABANDON_DRAIN_MAX_BYTES` / `_MAX_MS`): a body
/// dropped or errored BEFORE production completed waits at most
/// `max_ms + ABANDON_DRAIN_GRACE_MS` for the completion signal — the
/// in-flight abandon drain — before recycling. `0` in EITHER limit
/// disables the drain (the reader abandons the socket and reports the
/// `socket_poisoned` cause); then the relay wait is `Duration::ZERO` and
/// the pre-EDG-9 immediate recycle applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AbandonDrainLimits {
    pub max_bytes: u64,
    pub max_ms: u64,
}

impl Default for AbandonDrainLimits {
    fn default() -> Self {
        Self {
            max_bytes: edger_core::STREAM_ABANDON_DRAIN_MAX_BYTES_DEFAULT,
            max_ms: edger_core::STREAM_ABANDON_DRAIN_MAX_MS_DEFAULT,
        }
    }
}

impl AbandonDrainLimits {
    /// The drain runs only when BOTH limits are positive (mirrors the
    /// isolate's `AbandonDrain::enabled`).
    pub fn enabled(&self) -> bool {
        self.max_bytes > 0 && self.max_ms > 0
    }

    /// Bounded wait for the in-flight abandon drain: `Duration::ZERO` when
    /// the policy is disabled, otherwise `max_ms + ABANDON_DRAIN_GRACE_MS`
    /// — the grace covers the gap between the consumer loss and the
    /// reader observing it and starting (and running) the drain.
    pub fn drain_wait(&self) -> Duration {
        if !self.enabled() {
            Duration::ZERO
        } else {
            Duration::from_millis(self.max_ms.saturating_add(ABANDON_DRAIN_GRACE_MS))
        }
    }
}

/// The sub-cause label carried into the recycle lifecycle detail (and, via
/// it, the operational event) when the abandon drain reports a cause.
fn abandoned_detail(cause: AbandonedStream) -> &'static str {
    match cause {
        AbandonedStream::SocketPoisoned => "socket_poisoned",
        AbandonedStream::BytesLimit => "bytes_limit",
        AbandonedStream::TimeLimit => "time_limit",
        AbandonedStream::StreamError => "stream_error",
        // (EDG-9 slice 2) The harness acknowledged the cancel control
        // frame: a CLEAN end (the process is reused), carried into the
        // lifecycle detail of the `stream_abandoned_drained` completion.
        AbandonedStream::Cancelled => "cancelled",
        // Synthesized by the pool (never reported by the reader): the
        // bounded wait for the drain result expired before the reader
        // reported anything — an explicit, known outcome (EDG-9).
        AbandonedStream::RelayTimeout => "relay_timeout",
    }
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
    /// Sub-cause of the reason (EDG-9): e.g. `stream_error` for a
    /// mid-stream body error that recycled the process. `None` when the
    /// reason is self-explanatory.
    pub detail: Option<&'static str>,
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
        Self::with_factory_and_lifecycle_abandon_drain(
            config,
            factory,
            lifecycle_events,
            AbandonDrainLimits::default(),
        )
    }

    /// Like `with_factory_and_lifecycle`, with an explicit abandon-drain
    /// policy (EDG-9): a body dropped or errored before production
    /// completed waits at most `abandon_drain.drain_wait()` for the
    /// completion signal before recycling. Pass the SAME limits the isolates
    /// get from `EDGER_STREAM_ABANDON_DRAIN_MAX_BYTES` /
    /// `EDGER_STREAM_ABANDON_DRAIN_MAX_MS`; with `0` in either limit the
    /// drain is disabled and the relay wait is `Duration::ZERO` — the
    /// pre-EDG-9 immediate recycle applies.
    pub fn with_factory_and_lifecycle_abandon_drain(
        config: PoolConfig,
        factory: Arc<dyn IsolateFactory>,
        lifecycle_events: Option<LifecycleEventSender>,
        abandon_drain: AbandonDrainLimits,
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
                circuit_breakers: Mutex::new(HashMap::new()),
                shutdown: AtomicBool::new(false),
                ttl_decision: Mutex::new(()),
                close_admission: Mutex::new(()),
                #[cfg(feature = "test-hooks")]
                admission_seam: AdmissionSectionSeam::default(),
                lifecycle_events,
                abandon_drain,
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
        let group = Arc::new(WorkerGroup::new(instances));
        // (EDG-10) every instance learns its owning group for the TTL-floor
        // decision and the min-processes replenishment.
        for instance in group.instances_snapshot() {
            instance.attach_group(&group);
        }
        group
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
            self.remove_instance_with_cause(
                instance,
                WorkerRecycleCause::Error,
                ReplenishPolicy::SpawnFailed,
            );
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
                // (EDG-10 review P2 #1) a placeholder whose SPAWN failed is
                // removed with the explicit `SpawnFailed` policy: it never
                // schedules another replenishment attempt, whatever the
                // spawn was (request, prewarm or a replenishment itself). An
                // always-failing `prepare` would otherwise chain attempts
                // forever — no traffic, no backoff — whenever the circuit
                // breaker is disabled (`circuit_breaker_failures = 0`).
                self.remove_instance_with_cause(
                    instance,
                    WorkerRecycleCause::Error,
                    ReplenishPolicy::SpawnFailed,
                );
                self.sync_worker_counts();
                Err(err)
            }
        }
    }

    fn get_or_create_group(&self, worker_ref: &WorkerRef) -> Result<Arc<WorkerGroup>, WorkerError> {
        self.ensure_active()?;
        let key = WorkerCacheKey::from_worker_ref(worker_ref);

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

        match self
            .inner
            .cache
            .insert_group(key.clone(), Arc::clone(&group))
        {
            // A concurrent miss already owns the key: serve from the winning
            // group and discard ours (its instances are unspawned `Creating`
            // placeholders with no queue waiters, so dropping them is safe).
            GroupInsertOutcome::Existing(winner) => {
                // Revalidate the winner the same way the hit path does: the
                // miss path never checked the group it is about to use.
                if let Some(instance) = winner.instances_snapshot().first() {
                    if instance.worker_ref.namespace != worker_ref.namespace {
                        return Err(WorkerError::Collision {
                            key: format!("{key:?}"),
                            detail: "namespace mismatch for cache key".into(),
                        });
                    }
                }
                self.inner.metrics.record_hit();
                return Ok(winner);
            }
            GroupInsertOutcome::Inserted { evicted } => {
                if let Some((evicted_key, evicted_group)) = evicted {
                    if evicted_key == key {
                        return Err(WorkerError::Collision {
                            key: format!("{key:?}"),
                            detail: "concurrent insert".into(),
                        });
                    }
                    // Mark the victim evicted synchronously (before any
                    // await): the group then admits no new slots or
                    // instances, so its instance set is fixed and queued
                    // waiters wake up to re-resolve the identity against the
                    // current generation (retryable `Retired`, never
                    // `Shutdown`).
                    evicted_group.mark_evicted();
                    // Capacity eviction must not strand the victim: its idle
                    // instances are drained asynchronously (their TTL timer
                    // tasks hold instance Arcs and would keep the processes
                    // alive outside the LRU as orphaned copies), while
                    // in-flight dispatches finish on their own completion
                    // paths. A later request to the evicted identity
                    // cold-starts a fresh group instead of failing
                    // permanently.
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        let pool = self.clone();
                        handle.spawn(async move {
                            pool.drain_evicted_group(evicted_group).await;
                        });
                    }
                }
            }
        }

        let elapsed_ms = spawn_start.elapsed().as_millis().max(1) as u64;
        self.inner.metrics.record_miss();
        self.inner.metrics.record_spawn_latency(elapsed_ms);
        self.sync_worker_counts();
        Ok(group)
    }

    /// Gracefully terminates the idle instances of a group evicted by LRU
    /// capacity. The evicted group is unreachable from the cache, but its
    /// instances keep living on: the TTL timer task holds each instance Arc
    /// and would otherwise keep the evicted processes alive for up to
    /// `ttl_ms` as orphaned copies alongside any readmission. In-flight
    /// dispatches still holding instance Arcs finish on their own completion
    /// paths.
    ///
    /// The dispatch lock is awaited WITHOUT a timeout on purpose: a
    /// legitimate in-flight call longer than any drain budget must complete
    /// first and then be cleaned up, not be skipped (a skipped instance
    /// reschedules its TTL timer and becomes an orphan anyway). The lock is
    /// also held across the state check AND the termination, so a call that
    /// only acquires its slot after the check can never be terminated
    /// mid-flight.
    async fn drain_evicted_group(&self, group: Arc<WorkerGroup>) {
        for instance in group.instances_snapshot() {
            let dispatch_lock = instance.dispatch_lock();
            let guard = dispatch_lock.lock_owned().await;
            if matches!(instance.state(), WorkerState::Idle | WorkerState::Ready) {
                instance.cancel_ttl_timer();
                self.terminate_isolate_with_lifecycle(&instance, "lru_evicted")
                    .await;
                instance.set_state(WorkerState::Terminated);
                self.inner.metrics.record_terminated();
            }
            drop(guard);
        }
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
        if group.is_evicted() {
            // The group was evicted between the lookup and now: prewarming
            // it would orphan processes outside the cache. The identity's
            // next prewarm applies to the current generation.
            return Ok(0);
        }
        let instances = group.ensure_min_processes(target, || {
            let spawn_start = Instant::now();
            let instance = self.create_instance(&worker_ref);
            instance.attach_group(&group);
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
            if group.is_evicted() {
                break;
            }
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
            instance.attach_group(&group);
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
            instance.attach_group(&group);
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
                    instance.attach_group(&group);
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

        // Synthetic health checks are excluded from the request counters,
        // consistent with the buffered path.
        let count_metrics = !is_health_check_request(&req);
        let mut cancel_guard = DispatchCancelGuard {
            pool: self,
            instance: instance.clone(),
            terminal: DispatchTerminal::Pending,
            protect: true,
            count_metrics,
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
                // Known result: count `ok` NOW (centralized in the guard,
                // skips synthetic health checks); the cleanup protection
                // stays armed until on_request_complete finishes.
                cancel_guard.finish_ok();
                Supervisor::on_request_complete(instance, &config, self).await?;
                cancel_guard.complete();
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
                // The stream lifecycle owns the terminal request count
                // (completion/recycle), so the guard counts nothing here.
                cancel_guard.hand_off_to_stream();
                let state = StreamDispatchState {
                    pool: self.clone(),
                    instance,
                    config,
                    outcome: request_outcome_for_status(streamed.status),
                    started,
                    // The health-check exclusion travels WITH the body: the
                    // stream terminals (completion/recycle) apply the same
                    // exclusion to the group request counter.
                    count_metrics,
                    _dispatch_slot: dispatch_slot,
                    isolate_guard: Some(isolate_guard),
                };
                // Production-complete signal (EDG-8/EDG-9): the body, the
                // signal observer and the flag-checked drop/error paths share
                // the dispatch state and race to take it exactly once. The
                // flag guarantees a drop after a clean production end can
                // never recycle the (in-sync, reusable) process — whoever
                // wins the race drives the SAME lifecycle. (EDG-9) The relay
                // below lets a pre-completion drop/error WAIT (bounded) for
                // the in-flight abandon drain before deciding to recycle.
                let shared_state = Arc::new(Mutex::new(Some(state)));
                let (completion_wait, drain_wait) = match streamed.completed {
                    Some(signal) => {
                        let (relay_tx, relay_rx) =
                            tokio::sync::oneshot::channel::<StreamCompletion>();
                        let observer = Arc::clone(&shared_state);
                        tokio::spawn(async move {
                            let outcome = signal.await;
                            // Relay the OUTCOME (EDG-9) to a pre-completion
                            // drop/error waiting on it: the CAUSE rides with
                            // it into the recycle detail. If that wait is
                            // gone already, the send just fails — harmless.
                            let _ = relay_tx.send(outcome);
                            if matches!(outcome, StreamCompletion::Completed) {
                                // Production completed (clean end frame,
                                // possibly via the EDG-9 abandon drain):
                                // release the slot and isolate NOW, even
                                // though a slow client may still be
                                // downloading the buffered tail from memory.
                                if let Some(state) = take_stream_state(&observer) {
                                    complete_stream_state(state).await;
                                }
                            }
                            // Abandoned/Incomplete: production did not
                            // complete cleanly; the body's own
                            // error/end/drop path owns the lifecycle.
                        });
                        (Some(relay_rx), self.inner.abandon_drain.drain_wait())
                    }
                    None => (None, Duration::ZERO),
                };
                Ok(WorkerResponse::Streamed(StreamedResponse {
                    status: streamed.status,
                    headers: streamed.headers,
                    body: Box::pin(GuardedBody {
                        inner: streamed.body,
                        state: shared_state,
                        production_complete: streamed.production_complete,
                        completion_wait,
                        drain_wait,
                        drain_disabled: !self.inner.abandon_drain.enabled(),
                    }),
                    completed: None,           // consumed by the observer above
                    production_complete: None, // consumed by the guarded body
                }))
            }
            Err(err) => {
                drop(isolate_guard);
                // Terminal count, exactly once, centralized in the guard
                // (skips synthetic health checks).
                cancel_guard.finish_error();
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
            terminal: DispatchTerminal::Pending,
            protect: true,
            // Synthetic health checks are excluded from the request counters.
            count_metrics: record_observation,
        };

        let isolate_arc = instance.isolate();
        let mut isolate = isolate_arc.lock().await;
        let res = dispatch_to_isolate(isolate.as_mut(), kind, req, &config).await;
        drop(isolate);

        let res = match res {
            Ok(res) => {
                // Known result: count `ok` NOW (centralized in the guard,
                // skips synthetic health checks); the cleanup protection
                // stays armed until on_request_complete finishes, so a drop
                // or supervisor error in that window recycles the instance
                // without reclassifying the known result.
                cancel_guard.finish_ok();
                res
            }
            Err(err) => {
                // Terminal count, exactly once, centralized in the guard
                // (skips synthetic health checks).
                cancel_guard.finish_error();
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
        // The cleanup finished: disarm the guard's recycle protection (the
        // `ok` was already counted at `finish_ok`).
        cancel_guard.complete();

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
        self.remove_instance_with_cause(
            instance,
            WorkerRecycleCause::Error,
            ReplenishPolicy::Regular,
        );
    }

    /// Remove a terminated/ephemeral worker from the LRU cache.
    pub fn remove_instance(&self, instance: &WorkerInstance) {
        let cause = infer_recycle_cause(instance);
        self.remove_instance_with_cause(instance, cause, ReplenishPolicy::Regular);
    }

    fn remove_instance_with_cause(
        &self,
        instance: &WorkerInstance,
        cause: WorkerRecycleCause,
        policy: ReplenishPolicy,
    ) {
        let key = WorkerCacheKey::from_worker_ref(&instance.worker_ref);
        self.inner.cache.remove_instance(&key, instance.id());
        self.inner
            .metrics
            .record_worker_group_recycle(&instance.worker_ref, cause);
        self.inner.metrics.record_terminated();
        self.sync_worker_counts();
        // (EDG-10 review P2 #1) only regular removals may trigger the
        // min-processes replenishment; a failed-spawn placeholder never
        // chains another attempt (explicit policy, not a timing heuristic).
        if matches!(policy, ReplenishPolicy::Regular) {
            self.maybe_replenish_min_processes(instance);
        }
    }

    /// (EDG-10) One BACKGROUND min-processes replenishment attempt per
    /// removal: when a removal dropped the instance's group below
    /// `min_processes`, refill THAT group generation in the background. The
    /// attempt revalidates the generation at execution time (P2 #2) and
    /// never goes through `get_or_create_group` (P2 #2: it must not re-admit
    /// a removed identity or evict another group). There is no retry loop —
    /// a failed attempt is logged; the next removal or request tries again,
    /// and a failed-spawn placeholder never schedules its successor (P2 #1,
    /// `ReplenishPolicy::SpawnFailed`).
    ///
    /// No replenishment when: the pool is shutting down, the group was
    /// LRU-evicted (`is_evicted`) or closed (`recycle_worker`/`shutdown`), or
    /// `ttl_ms == 0` (ephemeral workers keep the current semantics, where
    /// `minProcesses` only prewarms at startup).
    fn maybe_replenish_min_processes(&self, removed: &WorkerInstance) {
        let worker_ref = &removed.worker_ref;
        if self.inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        if worker_ref.config.ttl_ms == 0 || worker_ref.config.min_processes == 0 {
            return;
        }
        // Capture the instance's OWN group as a WEAK now (promoted only at
        // execution — rev2 P2 #3): the task revalidates this exact
        // generation (P2 #2) at execution time.
        let Some(group) = removed.own_group() else {
            return;
        };
        if group.is_evicted() || group.is_closed() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            // No runtime to run the attempt on (synchronous teardown paths):
            // the next request re-establishes the floor through the demand
            // path instead.
            return;
        };
        // The removed instance is already out of the set (the caller removed
        // it first): this is the post-removal living count.
        let living = group
            .instances_snapshot()
            .iter()
            .filter(|instance| is_living(instance.state()))
            .count();
        if living >= worker_ref.config.min_processes {
            return;
        }
        // Counted at DISPATCH: "replenishments triggered" (the attempt itself
        // may still fail, e.g. on an open circuit — that failure is logged by
        // the attempt, not counted here).
        self.inner.metrics.record_worker_group_replenish(worker_ref);
        let pool = self.clone();
        let worker_ref = worker_ref.clone();
        let group_weak = Arc::downgrade(&group);
        handle.spawn(async move {
            pool.replenish_group(&group_weak, &worker_ref).await;
        });
    }

    /// (EDG-10 review P2 #2 / rev2 P2 #1-#3) The min-processes
    /// replenishment attempt, scoped to ONE group generation. The task
    /// captured a `Weak<WorkerGroup>` of the generation that originated the
    /// removal and promotes it ONLY at execution (a dropped generation —
    /// evicted and drained, recycled, or shut down — is a no-op). At
    /// execution the generation is revalidated and the placeholders are
    /// admitted in ONE critical section under the pool's `close_admission`
    /// lock, which the group-close paths (`shutdown`, `recycle_worker`)
    /// hold for their `close_queue` + drained-set snapshot:
    ///
    /// * the group must not have been evicted (LRU capacity) or closed
    ///   (`recycle_worker`/`shutdown`) since the task was scheduled;
    /// * the cache must still hold THIS exact generation for the worker's
    ///   identity (`Arc::ptr_eq`): a group that left the cache and was
    ///   re-admitted in the meantime is a DIFFERENT generation — its floor
    ///   is maintained by its own removals, and re-admitting here could
    ///   evict an unrelated group without any traffic.
    ///
    /// When valid, instances are created and spawned DIRECTLY into the
    /// captured group (`ensure_min_processes`), never through
    /// `get_or_create_group`: the attempt can neither re-admit a removed
    /// identity nor evict another group. A close that lands after the
    /// admission (the snapshot is taken under the same lock, later) puts
    /// the admitted placeholders INTO the drained set; the spawn revalidates
    /// `closed`/`evicted`/`shutdown` after each wait, so no prepare runs
    /// outside the drain. One attempt, no retry: a spawn failure removes
    /// the placeholder with `ReplenishPolicy::SpawnFailed` (P2 #1) and
    /// stops the loop.
    async fn replenish_group(&self, group: &Weak<WorkerGroup>, worker_ref: &WorkerRef) {
        // (rev2 P2 #3) Promote the captured WEAK only at execution: a
        // dropped generation has nothing left to replenish.
        let Some(group) = group.upgrade() else {
            return;
        };
        // (rev2 P2 #2) The admission critical section: generation
        // revalidation and placeholder admission share the `close_admission`
        // lock with the close paths, so no close can slip in between the
        // revalidation and `ensure_min_processes`.
        let admitted: Vec<Arc<WorkerInstance>> = {
            let _admission = self
                .inner
                .close_admission
                .lock()
                .expect("close/admission lock");
            if group.is_evicted() || group.is_closed() || self.inner.shutdown.load(Ordering::SeqCst)
            {
                return;
            }
            let key = WorkerCacheKey::from_worker_ref(worker_ref);
            match self.inner.cache.get_group(&key) {
                // The generation left the cache (capacity eviction without a
                // re-admission, or `recycle_worker`/`shutdown`): nothing to
                // refill — a later request re-admits a fresh generation on
                // demand.
                None => return,
                // A NEWER generation owns the identity: the captured group
                // is stale; its floor is not this attempt's to restore.
                Some(cached) if !Arc::ptr_eq(&cached, &group) => return,
                Some(_) => {}
            }
            if let Err(err) = self.ensure_circuit_closed(worker_ref) {
                tracing::warn!(
                    worker = %worker_ref.name,
                    version = %worker_ref.version,
                    "min-processes replenishment skipped (circuit open): {err}"
                );
                return;
            }
            // (EDG-10 rev3) Test seam: a test may pause the section HERE —
            // after generation revalidation, before placeholder admission,
            // still holding `close_admission` — to race a close against a
            // held section. Inert unless armed; not compiled without
            // `test-hooks`. Synchronous (no `await` while the guard is
            // held): the attempt's future must stay `Send`.
            #[cfg(feature = "test-hooks")]
            self.inner.admission_seam.hold_section_if_armed();
            let target = worker_ref
                .config
                .min_processes
                .min(worker_ref.config.max_processes.max(1));
            let before = group.instances_snapshot();
            let worker_ref = Self::worker_ref_with_dir(worker_ref);
            let instances = group.ensure_min_processes(target, || {
                let instance = self.create_instance(&worker_ref);
                instance.attach_group(&group);
                self.inner.metrics.record_miss();
                instance
            });
            // Only the instances THIS attempt added: the pre-existing
            // (living or already-spawned) members keep their own state.
            instances
                .into_iter()
                .filter(|instance| !before.iter().any(|existing| existing.id() == instance.id()))
                .collect()
        };
        self.sync_worker_counts();
        for instance in &admitted {
            let dispatch_lock = instance.dispatch_lock();
            let _guard = dispatch_lock.lock_owned().await;
            // (rev2 P2 #2) Re-validate after the wait: a close (shutdown /)
            // recycle or eviction that landed while the dispatch lock was
            // taken must not prepare an orphan OUTSIDE the drained set —
            // the admitted placeholders stay in the group's vector, and the
            // close-time snapshot (taken under the same lock, after this
            // admission) includes them, so the drain terminates them.
            if group.is_evicted() || group.is_closed() || self.inner.shutdown.load(Ordering::SeqCst)
            {
                break;
            }
            if instance.state() == WorkerState::Creating {
                if let Err(err) = self.spawn_instance(instance).await {
                    // One attempt, no retry: the failed placeholder was
                    // removed WITHOUT scheduling another attempt (P2 #1).
                    tracing::warn!(
                        worker = %worker_ref.name,
                        version = %worker_ref.version,
                        "min-processes replenishment attempt failed: {err}"
                    );
                    break;
                }
            }
            if instance.state() == WorkerState::Ready {
                instance.set_state(WorkerState::Idle);
            }
        }
        self.sync_worker_counts();
    }

    /// (EDG-10) Atomic TTL-floor decision: may the TTL of this idle instance
    /// terminate it without dropping its group below `min_processes`?
    ///
    /// The count of living group members and the `Idle -> Terminating`
    /// transition happen under the pool-wide `ttl_decision` lock: two timers
    /// expiring together are serialized, and each one observes the other's
    /// committed transition (or the already-removed member), so at most
    /// `living - min_processes` instances may terminate in one burst and the
    /// floor is never breached. Instances whose group left the pool (LRU-
    /// evicted, closed, or dropped) have no floor to protect and terminate
    /// as before (the eviction drain owns their cleanup).
    pub(crate) fn reserve_ttl_termination(&self, instance: &WorkerInstance) -> TtlFloorDecision {
        let _decision = self.inner.ttl_decision.lock().expect("ttl decision lock");
        if instance.state() != WorkerState::Idle {
            return TtlFloorDecision::NotIdle;
        }
        let group = instance.own_group();
        let floor = group
            .as_ref()
            .filter(|group| !group.is_evicted() && !group.is_closed())
            .map(|_| instance.worker_ref.config.min_processes)
            .unwrap_or(0);
        if floor == 0 {
            return self.transition_ttl_to_terminating(instance);
        }
        // The removed-member count is taken from the instance's own group
        // (not from the LRU: a re-admitted generation is a different group
        // and must not donate its floor to an evicted instance's timer).
        let living = group
            .expect("a nonzero floor implies a live, admitted group")
            .instances_snapshot()
            .iter()
            .filter(|member| is_living(member.state()))
            .count();
        if living.saturating_sub(1) < floor {
            return TtlFloorDecision::Keep;
        }
        self.transition_ttl_to_terminating(instance)
    }

    /// `Idle -> Terminating` under the instance state lock. Called with the
    /// decision lock held, which is what makes the count + transition atomic
    /// with respect to the other TTL decisions (EDG-10).
    fn transition_ttl_to_terminating(&self, instance: &WorkerInstance) -> TtlFloorDecision {
        let mut state = instance.state_lock();
        if *state != WorkerState::Idle {
            return TtlFloorDecision::NotIdle;
        }
        match transition(*state, WorkerEvent::TtlExpired) {
            Ok(next) => {
                *state = next;
                TtlFloorDecision::Terminate
            }
            // Unreachable: `(Idle, TtlExpired)` is a valid transition.
            Err(_) => TtlFloorDecision::NotIdle,
        }
    }

    /// (EDG-10) Count a TTL expiry that kept the instance alive because the
    /// `min_processes` floor required it.
    pub(crate) fn record_ttl_kept(&self, worker_ref: &WorkerRef) {
        self.inner.metrics.record_worker_group_ttl_kept(worker_ref);
    }

    pub(crate) async fn terminate_isolate_with_lifecycle(
        &self,
        instance: &WorkerInstance,
        reason: &'static str,
    ) {
        self.terminate_isolate_with_lifecycle_detail(instance, reason, None, false)
            .await
    }

    /// Variant for the stream-abandon recycle (EDG-9): `detail` carries the
    /// drain sub-cause, and the termination reason is rewritten ONLY when
    /// the report actually classifies a deadline wait: the 5 s isolate-lock
    /// timeout firing, or a shutdown that WAS sent and timed out (worker
    /// acked a beforeunload grace breach, or the ack never arrived within
    /// grace + margin) → `drain_timeout`. A socket that could not be
    /// reclaimed (nothing was sent — no ack was ever awaited) reports
    /// `SocketPoisoned` and terminates with the reason `socket_poisoned`
    /// instead of `drain_timeout`.
    pub(crate) async fn terminate_isolate_with_lifecycle_detail(
        &self,
        instance: &WorkerInstance,
        reason: &'static str,
        detail: Option<&'static str>,
        stream_abandoned: bool,
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
                detail,
            },
        );
        let started = Instant::now();
        let isolate = instance.isolate();
        let (report, lock_timed_out) =
            match tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, isolate.lock()).await {
                Ok(mut guard) => {
                    // The ACK wait lives inside `terminate_with_report` (the
                    // worker's grace budget plus the fixed margin) and the
                    // report CLASSIFIES which wait happened: a poisoned
                    // mid-stream socket reports `SocketPoisoned` without ever
                    // awaiting an ack — comparing aggregate elapsed time
                    // against the ack deadline would misclassify a slow socket
                    // recovery as a `drain_timeout` (EDG-9).
                    let report = guard.terminate_with_report().await.ok();
                    (report, false)
                }
                Err(_) => (None, true),
            };
        // (EDG-9) Only an ACTUAL deadline wait rewrites the reason to
        // `drain_timeout`: the 5 s isolate-lock timeout firing, or the
        // report's own classification of a sent-and-timed-out shutdown.
        // A `Completed` report (a real, completed wait) never becomes
        // `drain_timeout`, and neither does `SocketPoisoned` — that
        // terminates with the reason `socket_poisoned`, keeping the drain
        // cause (if any) in `detail`.
        let timed_out = if stream_abandoned {
            lock_timed_out
                || report
                    .as_ref()
                    .is_some_and(|report| report.outcome == TerminationOutcome::TimedOut)
        } else {
            report.is_none()
                || report
                    .as_ref()
                    .is_some_and(|report| report.outcome == TerminationOutcome::TimedOut)
        };
        let socket_poisoned = report
            .as_ref()
            .is_some_and(|report| report.outcome == TerminationOutcome::SocketPoisoned);
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
                detail,
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
                reason: if timed_out {
                    "drain_timeout"
                } else if socket_poisoned {
                    "socket_poisoned"
                } else {
                    reason
                },
                detail,
            },
        );
    }

    /// Evict and gracefully terminate every cached process for one worker
    /// identity. Removing the groups first guarantees that the next dispatch
    /// cold-starts from the current files while in-flight work drains.
    pub async fn recycle_worker(&self, name: &str, version: Option<&str>) -> usize {
        // (EDG-10 rev2 P2 #2) Remove + close + drained-set snapshot in ONE
        // critical section with min-processes replenishment admission
        // (`close_admission`): placeholders admitted before the section are
        // in the snapshot (drained); an admission after it sees the
        // closed/removed generation and refuses.
        let instances: Vec<Arc<WorkerInstance>> = {
            let _admission = self
                .inner
                .close_admission
                .lock()
                .expect("close/admission lock");
            let removed = self.inner.cache.remove_worker_groups(name, version);
            let mut instances: Vec<Arc<WorkerInstance>> = Vec::new();
            for (_, group) in &removed {
                group.close_queue();
                instances.extend(group.instances_snapshot());
            }
            instances
        };
        self.inner
            .circuit_breakers
            .lock()
            .expect("breaker lock")
            .retain(|key, _| {
                key.name != name || version.is_some_and(|version| key.version != version)
            });
        for instance in &instances {
            self.inner.metrics.record_terminated();
            instance.cancel_ttl_timer();
        }
        let recycled = instances.len();
        self.sync_worker_counts();
        shutdown_instances_after_drain(
            instances,
            self.inner.lifecycle_events.clone(),
            "worker_recycle",
        )
        .await;
        recycled
    }

    /// Begins graceful shutdown and returns the drain task handle (when a Tokio
    /// runtime is present) so the caller can AWAIT the beforeunload/waitUntil drain
    /// before exiting. Fire-and-forget would let the process exit mid-drain.
    pub fn shutdown(&self) -> Option<tokio::task::JoinHandle<()>> {
        if self.inner.shutdown.swap(true, Ordering::SeqCst) {
            return None;
        }

        let groups = {
            // (EDG-10 rev2 P2 #2) Close + drained-set snapshot in ONE
            // critical section with min-processes replenishment admission
            // (`close_admission`): placeholders admitted before the section
            // are in the snapshot (drained); an admission after it sees the
            // closed generation and refuses.
            let _admission = self
                .inner
                .close_admission
                .lock()
                .expect("close/admission lock");
            let groups = self.inner.cache.groups_snapshot();
            for group in &groups {
                group.close_queue();
            }
            groups
        };
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
                "shutdown",
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

    /// The current [`WorkerGroup`] generation held in the cache for this
    /// worker identity (no recency update). Test support: lets a test close
    /// or observe the exact generation a replenishment attempt captured.
    pub fn worker_group(&self, worker_ref: &WorkerRef) -> Option<Arc<WorkerGroup>> {
        self.inner
            .cache
            .get_group(&WorkerCacheKey::from_worker_ref(worker_ref))
    }

    /// (EDG-10 rev3) The admission-section test seam (see
    /// [`AdmissionSectionSeam`]). Compiled only with `test-hooks` — this
    /// package's own test targets.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub fn admission_section_seam(&self) -> &AdmissionSectionSeam {
        &self.inner.admission_seam
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

/// (EDG-10) States that count as a "living" group member for the floor
/// decision: anything that has not started terminating. `Terminating`
/// members already committed their TTL decision (their slot is reserved),
/// and `Terminated`/`EphemeralTerm` members are removed from the set by the
/// pool.
fn is_living(state: WorkerState) -> bool {
    matches!(
        state,
        WorkerState::Creating | WorkerState::Ready | WorkerState::Idle | WorkerState::Active
    )
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
        group.replenish_total = counters.replenish_total;
        group.rejected_total = counters.rejected_total;
        group.request_duration_ms_last = counters.request_duration_ms_last;
        group.request_duration_ms_p95 = counters.request_duration_ms_p95;
        group.request_total = counters.request_total;
        group.requests_error_total = counters.requests_error_total;
        group.requests_cancelled_total = counters.requests_cancelled_total;
        group.requests_ok_total = counters.requests_ok_total;
        group.timeout_total = counters.timeout_total;
        group.ttl_kept_total = counters.ttl_kept_total;
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
    // EDG-4: the request's Accept-Encoding selects a pre-compressed variant
    // of an immutable asset (`.br`/`.gz` generated at deploy time).
    let accept_encoding = edger_isolation::static_spa::accept_encoding_header(&req.headers);
    edger_isolation::static_spa::serve_static_spa_encoded(&req.uri, base, accept_encoding, config)
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
    /// Synthetic health-check traffic (`x-edger-health-check`): the stream
    /// terminals (completion/recycle) skip the group request counters, the
    /// same exclusion the dispatch entry applies. Existing counters (duration,
    /// outcome) are unaffected.
    count_metrics: bool,
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

/// Body stream wrapper enforcing the lifecycle above. `state` is shared with
/// the production-complete signal observer (EDG-8): whichever of the three
/// takes it first (clean end, signal, or early drop/error) drives the
/// lifecycle. The `production_complete` flag (set by the producer on a clean
/// end frame, BEFORE the signal fires) settles the race: a body that is
/// dropped or errors AFTER production completed must COMPLETE the dispatch
/// (the socket is in sync, the process reusable) — never recycle it.
///
/// (EDG-9) `completion_wait` relays the completion signal's outcome so a
/// drop/error BEFORE production completed can WAIT (bounded) for the
/// in-flight abandon drain; `drain_wait` is that budget.
struct GuardedBody {
    inner: BodyStream,
    state: Arc<Mutex<Option<StreamDispatchState>>>,
    production_complete: Option<Arc<AtomicBool>>,
    completion_wait: Option<tokio::sync::oneshot::Receiver<StreamCompletion>>,
    drain_wait: Duration,
    /// The pool's abandon-drain policy is disabled (a `0` limit): the reader
    /// abandons the socket on consumer loss, so the recycle sub-cause is
    /// known up front (`socket_poisoned`).
    drain_disabled: bool,
}

fn take_stream_state(state: &Mutex<Option<StreamDispatchState>>) -> Option<StreamDispatchState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

/// Drive the lifecycle for a dispatch state whose body ended BEFORE
/// production completed (mid-stream error or early drop). The decision is
/// made with the state ALREADY taken (see `GuardedBody::finish_abandoned`):
/// complete when production finished cleanly (socket in sync, process
/// reusable); otherwise, if the completion relay exists and the drain wait
/// is positive, wait for the in-flight abandon drain for at most
/// `drain_wait` — a `Completed` relay runs the normal completion (the
/// process is reused), anything else recycles, CARRYING THE DRAIN CAUSE
/// (EDG-9) into the recycle detail.
fn finish_stream_state(
    state: StreamDispatchState,
    production_completed: bool,
    completion_wait: Option<tokio::sync::oneshot::Receiver<StreamCompletion>>,
    drain_wait: Duration,
    recycle_reason: &'static str,
    detail: Option<&'static str>,
) {
    if production_completed {
        tokio::spawn(complete_stream_state(state));
        return;
    }
    match completion_wait {
        Some(rx) if !drain_wait.is_zero() => {
            let wait = drain_wait;
            tokio::spawn(async move {
                let started = Instant::now();
                // `Completed` only when the relay RESOLVED `Completed`
                // before the budget: the reader finished the abandoned
                // response cleanly within its limits and the socket is in
                // sync.
                let relayed = tokio::time::timeout(wait, rx).await;
                // The relay outcome, if it resolved within the budget AND
                // the reader actually reported one (a dropped sender or a
                // timed-out wait has no outcome).
                let wait_expired = relayed.is_err();
                let outcome = match relayed {
                    Ok(result) => result.ok(),
                    Err(_) => None,
                };
                let drained = outcome.as_ref().is_some_and(|outcome| {
                    matches!(
                        outcome,
                        StreamCompletion::Completed
                            // (EDG-9 slice 2) The cancel end is ALSO a
                            // clean end: the harness aborted the body,
                            // restored the socket and the process is
                            // reused, exactly like a drained clean end.
                            | StreamCompletion::Abandoned(AbandonedStream::Cancelled)
                    )
                });
                if drained {
                    // Register the real termination reason (EDG-9) and run
                    // the normal completion: the instance goes Idle and the
                    // slot is released — the process is reused.
                    // (EDG-9 slice 2) The cancel end carries its sub-cause
                    // into the lifecycle detail (and, via it, the
                    // operational event); a plain drained end carries none.
                    let detail = outcome.as_ref().and_then(|outcome| match outcome {
                        StreamCompletion::Abandoned(AbandonedStream::Cancelled) => {
                            Some("cancelled")
                        }
                        _ => None,
                    });
                    emit_lifecycle(
                        state.pool.inner.lifecycle_events.as_ref(),
                        WorkerLifecycleEvent {
                            kind: WorkerLifecycleEventKind::DrainCompleted,
                            worker_ref: state.instance.worker_ref.clone(),
                            process_id: None,
                            drained_count: None,
                            duration_ms: Some(started.elapsed().as_millis() as u64),
                            reason: "stream_abandoned_drained",
                            detail,
                        },
                    );
                    complete_stream_state(state).await;
                } else {
                    // (EDG-9) Carry the drain CAUSE the reader reported
                    // into the recycle detail. A wait that EXPIRED is an
                    // explicit, known outcome — not `None`: the reader
                    // simply had not reported within the budget (slow
                    // producer), so the sub-cause is `relay_timeout` and
                    // the late result, if any, is ignored (the relay
                    // receiver is already gone and the observer's send
                    // just fails, harmlessly). A resolved `Incomplete`
                    // (no reported cause) keeps the body-level detail.
                    let detail = detail
                        .or_else(|| {
                            outcome.as_ref().and_then(|outcome| match outcome {
                                StreamCompletion::Abandoned(cause) => {
                                    Some(abandoned_detail(*cause))
                                }
                                _ => None,
                            })
                        })
                        .or_else(|| {
                            wait_expired.then_some(abandoned_detail(AbandonedStream::RelayTimeout))
                        });
                    recycle_stream_state(state, recycle_reason, detail).await;
                }
            });
        }
        _ => {
            tokio::spawn(recycle_stream_state(state, recycle_reason, detail));
        }
    }
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
                // Mid-stream error: normally a desynced socket (recycle) —
                // UNLESS production had already completed cleanly, in which
                // case the socket is in sync and the process stays put.
                // (EDG-9) The decision is taken atomically with the state
                // take (see `finish_abandoned`); the sub-cause is the
                // mid-stream error itself.
                self.finish_abandoned(Some("stream_error"));
                std::task::Poll::Ready(Some(Err(err)))
            }
            std::task::Poll::Ready(None) => {
                if let Some(state) = take_stream_state(&self.state) {
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
        // Dropped before end-of-stream: normally the client disconnected
        // while frames were in flight — the process socket cannot be
        // reused (recycle). (EDG-9) UNLESS the reader is still draining
        // the abandoned response: the bounded wait below reuses the process
        // when the drain finishes cleanly.
        self.finish_abandoned(None);
    }
}

impl GuardedBody {
    /// The shared production-complete flag, read without blocking.
    fn production_completed(&self) -> bool {
        self.production_complete
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    /// Body end before production completed (drop or mid-stream error).
    ///
    /// (EDG-9, decision 3) The "complete or recycle" decision is taken
    /// ATOMICALLY with the state take: the dispatch state is taken FIRST,
    /// and the production-complete flag is read only AFTER the take. The
    /// flag is stored by the producer right before the completion signal
    /// fires, so a read that happened BEFORE the take can miss a store that
    /// lands in between and recycle a socket that is in sync and reusable.
    /// Reading after the take makes the decision reflect the state at the
    /// moment the dispatch is decided: flag set ⇒ the producer saw a clean
    /// end and restored the socket ⇒ complete; flag unset ⇒ production had
    /// not completed cleanly at decision time ⇒ recycle.
    fn finish_abandoned(&mut self, detail: Option<&'static str>) {
        let Some(state) = take_stream_state(&self.state) else {
            // The signal observer (or the body's own end path) took the
            // state first and owns the lifecycle.
            return;
        };
        let production_completed = self.production_completed();
        // A completion relay exists only when the detach pipeline is
        // active: with no pipeline the behavior is the pre-EDG-9 immediate
        // recycle (`stream_recycle`), exactly as before.
        let has_pipeline = self.completion_wait.is_some();
        // (EDG-9) When the drain policy is disabled the reader abandons the
        // socket on consumer loss — the recycle sub-cause is known up front
        // (the reader's own cause report never arrives: the pool does not
        // wait on the relay when the policy is disabled). A mid-stream
        // error detail wins over it.
        let detail =
            detail.or_else(|| (has_pipeline && self.drain_disabled).then_some("socket_poisoned"));
        finish_stream_state(
            state,
            production_completed,
            self.completion_wait.take(),
            self.drain_wait,
            if has_pipeline {
                "stream_abandoned_recycled"
            } else {
                "stream_recycle"
            },
            detail,
        );
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
        count_metrics,
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
    // Group request counter only (health checks are excluded, like at the
    // dispatch entry); the existing outcome/duration counters are unaffected.
    if count_metrics {
        pool.inner
            .metrics
            .record_worker_group_requests_ok(&worker_ref);
    }
    pool.inner
        .metrics
        .record_worker_group_outcome(&worker_ref, outcome);
    pool.sync_worker_counts();
}

/// Abnormal end (mid-stream error, client disconnect, or an abandoned
/// response whose drain failed, EDG-9): terminate the isolate and evict the
/// instance so the next request gets a fresh process. `reason` is the real
/// termination reason (`stream_recycle` for a pre-EDG-9 disconnect without
/// pipeline, `stream_abandoned_recycled` for the EDG-9 abandon path); the
/// terminate only rewrites it to `drain_timeout` after a real deadline wait.
async fn recycle_stream_state(
    mut state: StreamDispatchState,
    reason: &'static str,
    detail: Option<&'static str>,
) {
    let worker_ref = state.instance.worker_ref.clone();
    let duration_ms = state.started.elapsed().as_millis().max(1) as u64;
    drop(state.isolate_guard.take());
    state
        .pool
        .terminate_isolate_with_lifecycle_detail(&state.instance, reason, detail, true)
        .await;
    state.pool.recycle_cancelled(&state.instance);
    state
        .pool
        .inner
        .metrics
        .record_worker_group_request(&worker_ref, duration_ms);
    // Group request counter only (health checks are excluded, like at the
    // dispatch entry); the existing outcome/duration counters are unaffected.
    if state.count_metrics {
        state
            .pool
            .inner
            .metrics
            .record_worker_group_requests_error(&worker_ref);
    }
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
    reason: &'static str,
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
                reason,
                detail: None,
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
            reason,
        )
        .await;
    }
}

async fn terminate_shutdown_instance(
    instance: Arc<WorkerInstance>,
    lifecycle_events: Option<&LifecycleEventSender>,
    started: Instant,
    dispatch_timed_out: bool,
    reason: &'static str,
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
            reason,
            detail: None,
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
            reason: if timed_out { "drain_timeout" } else { reason },
            detail: None,
        },
    );
}

fn emit_lifecycle(sender: Option<&LifecycleEventSender>, event: WorkerLifecycleEvent) {
    if let Some(sender) = sender {
        let _ = sender.try_send(event);
    }
}

/// The terminal state of a dispatch that obtained a process slot. The
/// `DispatchCancelGuard` uses it to record the group request counter EXACTLY
/// ONCE per dispatch:
/// - a known result is finalized by the terminal site (`finish_ok` /
///   `finish_error`) and counted there;
/// - a response handed to the stream is `hand_off_to_stream`ed: the stream
///   lifecycle counts at completion/recycle, so the guard counts nothing;
/// - a future dropped before a known result (`Pending` at `Drop`) counts
///   `cancelled` in `Drop` — the cancel path. (Recycling an incomplete exit
///   is a separate duty, owned by the guard's `protect` flag.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchTerminal {
    /// Dropped before a known result: `cancelled` in `Drop`.
    Pending,
    /// The worker returned a response (any HTTP status).
    Ok,
    /// A worker/isolate error.
    Error,
    /// The stream lifecycle owns the terminal count.
    Streamed,
}

/// RAII guard that recycles an `Active` instance if the dispatch exits
/// incompletely (cancellation, e.g. an HTTP client disconnect, or a
/// supervisor cleanup failure) and centralizes the terminal group request
/// counter (ok / error / cancelled): each dispatch that obtained a process
/// slot is counted exactly once. It carries two independent duties:
/// - the TERMINAL RESULT (`terminal`): a known result is counted the moment
///   the isolate returns it (`finish_ok`/`finish_error`), so a drop or a
///   supervisor error DURING the `on_request_complete` cleanup can never
///   reclassify a known result as `cancelled`;
/// - the CLEANUP PROTECTION (`protect`): stays armed until the dispatch's
///   cleanup has fully finished (`complete` after `on_request_complete`,
///   `hand_off_to_stream` when the stream takes over, or `finish_error`
///   when the site explicitly evicts the instance); a drop while it is
///   armed recycles the instance.
///
/// `count_metrics` is false for synthetic health-check traffic
/// (`x-edger-health-check`), which is excluded from the counters.
struct DispatchCancelGuard<'a> {
    pool: &'a WorkerPool,
    instance: Arc<WorkerInstance>,
    terminal: DispatchTerminal,
    protect: bool,
    count_metrics: bool,
}

impl DispatchCancelGuard<'_> {
    /// A known success: the worker returned a response (any HTTP status).
    /// Counts the group `ok` once NOW — before the supervisor cleanup below —
    /// and marks the result known so `Drop` never counts `cancelled`. The
    /// cleanup protection stays armed until `complete()`.
    fn finish_ok(&mut self) {
        self.terminal = DispatchTerminal::Ok;
        if self.count_metrics {
            self.pool
                .inner
                .metrics
                .record_worker_group_requests_ok(&self.instance.worker_ref);
        }
    }

    /// A known failure: a worker/isolate error. Counts the group `error`
    /// once, marks the result known, and disarms the cleanup protection —
    /// the calling site explicitly evicts the instance.
    fn finish_error(&mut self) {
        self.terminal = DispatchTerminal::Error;
        if self.count_metrics {
            self.pool
                .inner
                .metrics
                .record_worker_group_requests_error(&self.instance.worker_ref);
        }
        self.protect = false;
    }

    /// The response became a stream: its terminal count belongs to the stream
    /// lifecycle (completion/recycle sites), so the guard counts nothing —
    /// no double count on the hand-off — and the stream owns the lifecycle,
    /// so the cleanup protection disarms here.
    fn hand_off_to_stream(&mut self) {
        self.terminal = DispatchTerminal::Streamed;
        self.protect = false;
    }

    /// The supervisor cleanup (`on_request_complete`) finished: the dispatch
    /// fully completed, so the cleanup protection disarms.
    fn complete(&mut self) {
        self.protect = false;
    }
}

impl Drop for DispatchCancelGuard<'_> {
    fn drop(&mut self) {
        if self.terminal == DispatchTerminal::Pending {
            // Cancelled before a known result: count it (exactly once — the
            // guard is per dispatch). A known result (`Ok`) already counted
            // at `finish_ok`, so a drop inside the `on_request_complete`
            // cleanup never reclassifies it.
            if self.count_metrics {
                self.pool
                    .inner
                    .metrics
                    .record_worker_group_requests_cancelled(&self.instance.worker_ref);
            }
        }
        if self.protect {
            self.pool.recycle_cancelled(&self.instance);
        }
    }
}

#[cfg(test)]
mod stream_drop_flag_tests {
    //! EDG-8: a body dropped AFTER production completed must COMPLETE the
    //! dispatch (the process socket is in sync and the process reusable),
    //! never recycle it — even when the drop wins the race against the
    //! completion-signal observer.

    use super::*;
    use edger_core::{AbandonedStream, CompletionSignal, IsolationError, StreamCompletion};
    use std::task::{Context, Poll};

    /// Body that yields exactly one chunk and then stays open (pending
    /// forever), so a drop — not an end-of-stream — decides the lifecycle.
    struct OneThenPendingBody {
        consumed: bool,
    }

    impl futures_core::Stream for OneThenPendingBody {
        type Item = Result<Bytes, IsolationError>;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Self::Item>> {
            if self.consumed {
                Poll::Pending
            } else {
                self.consumed = true;
                Poll::Ready(Some(Ok(Bytes::from_static(b"chunk-0"))))
            }
        }
    }

    /// Deterministic interleaving fixture: production has ALREADY completed
    /// (the flag is set) but the completion signal is still PENDING, so the
    /// drop path — not the signal observer — is the one that decides the
    /// lifecycle.
    #[derive(Default)]
    struct DropFlagFixture {
        fire: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<StreamCompletion>>>,
        terminated: std::sync::atomic::AtomicUsize,
    }

    struct DropFlagFactory {
        fixture: Arc<DropFlagFixture>,
    }

    struct DropFlagIsolate {
        fixture: Arc<DropFlagFixture>,
        fire_rx: Option<tokio::sync::oneshot::Receiver<StreamCompletion>>,
    }

    impl IsolateFactory for DropFlagFactory {
        fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
            let (fire_tx, fire_rx) = tokio::sync::oneshot::channel::<StreamCompletion>();
            self.fixture.fire.lock().unwrap().replace(fire_tx);
            Box::new(DropFlagIsolate {
                fixture: Arc::clone(&self.fixture),
                fire_rx: Some(fire_rx),
            })
        }
    }

    #[async_trait::async_trait]
    impl Isolate for DropFlagIsolate {
        async fn execute_fetch(
            &mut self,
            _req: SerializedRequest,
            _config: &WorkerConfig,
        ) -> Result<SerializedResponse, IsolationError> {
            Err(IsolationError::new(
                "NOT_STREAM",
                "fixture only streams /stream",
            ))
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
            self.execute_fetch(
                SerializedRequest {
                    method: "GET".into(),
                    uri: "/".into(),
                    headers: vec![],
                    body: None,
                    request_id: "spa".into(),
                    base_href: None,
                },
                config,
            )
            .await
        }

        async fn execute_wasm(
            &mut self,
            req: SerializedRequest,
            config: &WorkerConfig,
        ) -> Result<SerializedResponse, IsolationError> {
            self.execute_fetch(req, config).await
        }

        async fn execute_fetch_stream(
            &mut self,
            req: SerializedRequest,
            config: &WorkerConfig,
        ) -> Result<WorkerResponse, IsolationError> {
            if req.uri != "/stream" {
                return self
                    .execute_fetch(req, config)
                    .await
                    .map(WorkerResponse::Buffered);
            }
            // Production has ALREADY completed (flag `true`), but the signal
            // is still PENDING: the observer cannot take the dispatch state
            // before the body is dropped.
            let flag = Arc::new(AtomicBool::new(true));
            let fire_rx = self.fire_rx.take().expect("signal created by the factory");
            let signal: CompletionSignal =
                Box::pin(async move { fire_rx.await.unwrap_or(StreamCompletion::Incomplete) });
            Ok(WorkerResponse::Streamed(StreamedResponse {
                status: 200,
                headers: vec![],
                body: Box::pin(OneThenPendingBody { consumed: false }),
                completed: Some(signal),
                production_complete: Some(flag),
            }))
        }

        async fn terminate(&mut self) -> Result<(), IsolationError> {
            self.fixture.terminated.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    // Mutation captured (EDG-8, review P2 #3): a drop path that IGNORES the
    // production-complete flag recycles the in-sync, reusable process. The
    // current-thread runtime plus the pending signal make the interleaving
    // deterministic: the DROP is the one that takes the state, and it must
    // COMPLETE (Idle), never terminate.
    #[tokio::test(flavor = "current_thread")]
    async fn drop_after_production_complete_completes_instead_of_recycling() {
        let fixture = Arc::new(DropFlagFixture::default());
        let pool = WorkerPool::with_factory(
            PoolConfig {
                max_size: 16,
                ephemeral_concurrency: 4,
                ephemeral_queue_limit: 8,
            },
            Arc::new(DropFlagFactory {
                fixture: Arc::clone(&fixture),
            }),
        );
        let worker_ref = create_worker_ref(
            std::path::PathBuf::from("/workers/edg8-flag-drop"),
            WorkerManifest {
                name: "edg8-flag-drop".into(),
                max_processes: Some(1),
                ttl: Some(serde_yaml::Value::String("30s".into())),
                ..Default::default()
            },
        )
        .unwrap();
        let req = SerializedRequest {
            method: "GET".into(),
            uri: "/stream".into(),
            headers: vec![],
            body: None,
            request_id: "edg8-flag-drop".into(),
            base_href: None,
        };

        let streamed = pool
            .fetch_worker_stream(&worker_ref, req, Some(ExecutionKind::FetchHandler))
            .await
            .unwrap();
        let StreamedResponse { body, .. } = match streamed {
            WorkerResponse::Streamed(streamed) => streamed,
            _ => panic!("expected a streamed response"),
        };

        // The signal is still PENDING, so the signal observer cannot take the
        // state before the body is dropped. DROP NOW — the drop path is the
        // one that decides the lifecycle. The flag is set: COMPLETE, never
        // recycle.
        drop(body);
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }

        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            0,
            "a drop after production completed must not terminate the isolate"
        );
        let stats = pool.worker_stats();
        assert_eq!(stats.len(), 1, "the instance must still be cached");
        assert_eq!(
            stats[0].state,
            WorkerState::Idle,
            "the dispatch must COMPLETE (Idle), not recycle"
        );

        // Fire the signal NOW: the observer takes the state (already taken by
        // the drop) and must be a no-op — no second lifecycle, no recycle.
        fixture
            .fire
            .lock()
            .unwrap()
            .take()
            .expect("signal still pending")
            .send(StreamCompletion::Completed)
            .unwrap();
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            0,
            "the late observer must not double-run the lifecycle"
        );
        assert_eq!(
            pool.worker_stats()[0].state,
            WorkerState::Idle,
            "still Idle after the late signal"
        );
    }

    // (EDG-9) Abandon-drain fixtures: production had NOT completed at fetch
    // time (flag `false`, signal pending) and the test plays the reader's
    // drain: it stores the flag and fires the signal at a deterministic
    // moment — mirroring the reader's ordering (flag store happens-before
    // the completion send). The current-thread runtime makes the drop vs
    // drain ordering exact.

    #[derive(Default)]
    struct DropAbandonFixture {
        flag: std::sync::Mutex<Option<Arc<AtomicBool>>>,
        fire: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<StreamCompletion>>>,
        terminated: std::sync::atomic::AtomicUsize,
        /// The outcome the fixture's `terminate_with_report` returns
        /// (`Completed` unless a test configures one — EDG-9 amendment 2
        /// classifies the `SocketPoisoned` pool path).
        terminate_outcome: std::sync::Mutex<Option<edger_core::TerminationOutcome>>,
    }

    struct DropAbandonFactory {
        fixture: Arc<DropAbandonFixture>,
    }

    struct DropAbandonIsolate {
        fixture: Arc<DropAbandonFixture>,
        fire_rx: Option<tokio::sync::oneshot::Receiver<StreamCompletion>>,
    }

    impl IsolateFactory for DropAbandonFactory {
        fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
            let (fire_tx, fire_rx) = tokio::sync::oneshot::channel::<StreamCompletion>();
            self.fixture.fire.lock().unwrap().replace(fire_tx);
            Box::new(DropAbandonIsolate {
                fixture: Arc::clone(&self.fixture),
                fire_rx: Some(fire_rx),
            })
        }
    }

    #[async_trait::async_trait]
    impl Isolate for DropAbandonIsolate {
        async fn execute_fetch(
            &mut self,
            _req: SerializedRequest,
            _config: &WorkerConfig,
        ) -> Result<SerializedResponse, IsolationError> {
            Err(IsolationError::new(
                "NOT_STREAM",
                "fixture only streams /stream",
            ))
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
            self.execute_fetch(
                SerializedRequest {
                    method: "GET".into(),
                    uri: "/".into(),
                    headers: vec![],
                    body: None,
                    request_id: "spa".into(),
                    base_href: None,
                },
                config,
            )
            .await
        }

        async fn execute_wasm(
            &mut self,
            req: SerializedRequest,
            config: &WorkerConfig,
        ) -> Result<SerializedResponse, IsolationError> {
            self.execute_fetch(req, config).await
        }

        async fn execute_fetch_stream(
            &mut self,
            req: SerializedRequest,
            config: &WorkerConfig,
        ) -> Result<WorkerResponse, IsolationError> {
            if req.uri != "/stream" {
                return self
                    .execute_fetch(req, config)
                    .await
                    .map(WorkerResponse::Buffered);
            }
            // Production has NOT completed yet: flag `false`, signal pending.
            let flag = Arc::new(AtomicBool::new(false));
            self.fixture.flag.lock().unwrap().replace(flag.clone());
            let fire_rx = self.fire_rx.take().expect("signal created by the factory");
            // The reader sends a cause on every abandon-drain exit (EDG-9);
            // a dropped sender (the fixture's "no cause" path) reports
            // `Incomplete` — the pool recycles with the body-level detail.
            let signal: CompletionSignal =
                Box::pin(async move { fire_rx.await.unwrap_or(StreamCompletion::Incomplete) });
            Ok(WorkerResponse::Streamed(StreamedResponse {
                status: 200,
                headers: vec![],
                body: Box::pin(OneThenPendingBody { consumed: false }),
                completed: Some(signal),
                production_complete: Some(flag),
            }))
        }

        async fn terminate(&mut self) -> Result<(), IsolationError> {
            self.fixture.terminated.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn terminate_with_report(
            &mut self,
        ) -> Result<edger_core::TerminationReport, IsolationError> {
            self.fixture.terminated.fetch_add(1, Ordering::SeqCst);
            Ok(edger_core::TerminationReport {
                outcome: self
                    .fixture
                    .terminate_outcome
                    .lock()
                    .unwrap()
                    .unwrap_or(edger_core::TerminationOutcome::Completed),
                process_id: None,
                drained_count: None,
            })
        }
    }

    fn abandon_pool(
        fixture: &Arc<DropAbandonFixture>,
        name: &str,
        lifecycle: Option<tokio::sync::mpsc::Sender<WorkerLifecycleEvent>>,
    ) -> (WorkerPool, WorkerRef) {
        abandon_pool_limits(
            fixture,
            name,
            lifecycle,
            AbandonDrainLimits {
                max_bytes: edger_core::STREAM_ABANDON_DRAIN_MAX_BYTES_DEFAULT,
                max_ms: 50,
            },
        )
    }

    fn abandon_pool_limits(
        fixture: &Arc<DropAbandonFixture>,
        name: &str,
        lifecycle: Option<tokio::sync::mpsc::Sender<WorkerLifecycleEvent>>,
        abandon_drain: AbandonDrainLimits,
    ) -> (WorkerPool, WorkerRef) {
        let pool = WorkerPool::with_factory_and_lifecycle_abandon_drain(
            PoolConfig {
                max_size: 16,
                ephemeral_concurrency: 4,
                ephemeral_queue_limit: 8,
            },
            Arc::new(DropAbandonFactory {
                fixture: Arc::clone(fixture),
            }),
            lifecycle,
            abandon_drain,
        );
        let worker_ref = create_worker_ref(
            std::path::PathBuf::from(format!("/workers/{name}")),
            WorkerManifest {
                name: name.into(),
                max_processes: Some(1),
                ttl: Some(serde_yaml::Value::String("30s".into())),
                ..Default::default()
            },
        )
        .unwrap();
        (pool, worker_ref)
    }

    fn stream_request(worker: &str) -> SerializedRequest {
        SerializedRequest {
            method: "GET".into(),
            uri: "/stream".into(),
            headers: vec![],
            body: None,
            request_id: worker.into(),
            base_href: None,
        }
    }

    /// Drain the (small) lifecycle channel and return the first event of the
    /// given kind, if any.
    fn lifecycle_event_of(
        rx: &mut tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
        kind: WorkerLifecycleEventKind,
    ) -> Option<WorkerLifecycleEvent> {
        loop {
            match rx.try_recv() {
                Ok(event) if event.kind == kind => return Some(event),
                Ok(_) => continue,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return None,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return None,
            }
        }
    }

    fn drain_completed_event(
        rx: &mut tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
    ) -> Option<WorkerLifecycleEvent> {
        lifecycle_event_of(rx, WorkerLifecycleEventKind::DrainCompleted)
    }

    fn terminated_event(
        rx: &mut tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
    ) -> Option<WorkerLifecycleEvent> {
        lifecycle_event_of(rx, WorkerLifecycleEventKind::Terminated)
    }

    async fn fetch_stream_body(
        pool: &WorkerPool,
        worker_ref: &WorkerRef,
    ) -> edger_core::StreamedResponse {
        let streamed = pool
            .fetch_worker_stream(
                worker_ref,
                stream_request(&worker_ref.name),
                Some(ExecutionKind::FetchHandler),
            )
            .await
            .unwrap();
        match streamed {
            WorkerResponse::Streamed(streamed) => streamed,
            _ => panic!("expected a streamed response"),
        }
    }

    // (EDG-9, decision 2+3) The client disconnects WHILE the reader is still
    // draining the abandoned response; the drain then finishes cleanly within
    // the limits (flag store + completion). The pool must WAIT for the
    // completion relay and COMPLETE (the process is reused) — never recycle.
    // Mutation killed: a drop path that recycles without waiting.
    #[tokio::test(flavor = "current_thread")]
    async fn drop_while_abandon_drain_in_progress_completes_when_drain_finishes() {
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::channel(16);
        let fixture = Arc::new(DropAbandonFixture::default());
        let (pool, worker_ref) = abandon_pool(&fixture, "edg9-drained", Some(lifecycle_tx));

        let _streamed = fetch_stream_body(&pool, &worker_ref).await;
        // Client disconnects BEFORE the drain finishes: flag still `false`,
        // the drop spawns the bounded completion wait.
        drop(_streamed.body);

        // The drain finishes cleanly NOW: flag store happens-before the
        // completion send (the reader's real ordering).
        let flag = fixture
            .flag
            .lock()
            .unwrap()
            .clone()
            .expect("flag created by the isolate");
        flag.store(true, Ordering::SeqCst);
        fixture
            .fire
            .lock()
            .unwrap()
            .take()
            .expect("signal still pending")
            .send(StreamCompletion::Completed)
            .unwrap();
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }

        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            0,
            "a drain that finished within the limits must not terminate the isolate"
        );
        let stats = pool.worker_stats();
        assert_eq!(stats.len(), 1, "the instance must still be cached");
        assert_eq!(
            stats[0].state,
            WorkerState::Idle,
            "the dispatch must COMPLETE (Idle), not recycle"
        );
        let drained = drain_completed_event(&mut lifecycle_rx);
        assert!(
            drained.is_some_and(|event| event.reason == "stream_abandoned_drained"),
            "the real termination reason must be stream_abandoned_drained"
        );
    }

    // (EDG-9) The client disconnects and the drain does NOT finish within the
    // limits (the completion signal resolves `false`): the bounded wait
    // closes and the process is recycled with the REAL reason
    // `stream_abandoned_recycled` (no drain was waiting, so no
    // `drain_timeout`).
    #[tokio::test(flavor = "current_thread")]
    async fn drop_when_abandon_drain_fails_recycles_with_real_reason() {
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::channel(16);
        let fixture = Arc::new(DropAbandonFixture::default());
        let (pool, worker_ref) = abandon_pool(&fixture, "edg9-drain-fail", Some(lifecycle_tx));

        let _streamed = fetch_stream_body(&pool, &worker_ref).await;
        drop(_streamed.body);

        // The reader gives up (over a limit or a socket error): the
        // completion resolves `false`. Dropping the sender does it.
        fixture.fire.lock().unwrap().take();
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }

        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            1,
            "a failed drain must recycle the process"
        );
        let terminated = terminated_event(&mut lifecycle_rx);
        assert!(
            terminated.is_some_and(|event| {
                event.reason == "stream_abandoned_recycled" && event.detail.is_none()
            }),
            "the real reason must be stream_abandoned_recycled, not drain_timeout"
        );
    }

    // (EDG-9, decision 3) The completion arrives BEFORE the drop decides:
    // the flag is stored and the signal fired, but the drop wins the state
    // take (the observer is delayed on the current-thread runtime). The
    // decision reads the flag AFTER the take — flag set ⇒ complete, never
    // recycle.
    #[tokio::test(flavor = "current_thread")]
    async fn drop_after_drain_finished_completes_instead_of_recycling() {
        let fixture = Arc::new(DropAbandonFixture::default());
        let (pool, worker_ref) = abandon_pool(&fixture, "edg9-drop-late", None);

        let _streamed = fetch_stream_body(&pool, &worker_ref).await;
        let flag = fixture
            .flag
            .lock()
            .unwrap()
            .clone()
            .expect("flag created by the isolate");
        // The drain finished (flag + signal) — but on the current-thread
        // runtime the observer task cannot run before the drop, so the DROP
        // takes the dispatch state first.
        flag.store(true, Ordering::SeqCst);
        fixture
            .fire
            .lock()
            .unwrap()
            .take()
            .expect("signal still pending")
            .send(StreamCompletion::Completed)
            .unwrap();
        drop(_streamed.body);
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }

        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            0,
            "the flag (read after the state take) must settle the decision on COMPLETE"
        );
        let stats = pool.worker_stats();
        assert_eq!(stats.len(), 1, "the instance must still be cached");
        assert_eq!(
            stats[0].state,
            WorkerState::Idle,
            "the dispatch must COMPLETE (Idle), not recycle"
        );
    }

    // (EDG-9) The drain CAUSE the reader reports rides the completion relay
    // into the recycle lifecycle detail: the reader gives up at the byte
    // limit, the bounded wait resolves with `Abandoned(BytesLimit)` and the
    // pool recycles with the real sub-cause — not a generic detail-less
    // recycle, and not a drain_timeout.
    #[tokio::test(flavor = "current_thread")]
    async fn drop_carries_the_drain_cause_into_the_recycle_detail() {
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::channel(16);
        let fixture = Arc::new(DropAbandonFixture::default());
        let (pool, worker_ref) = abandon_pool(&fixture, "edg9-drain-cause", Some(lifecycle_tx));

        let _streamed = fetch_stream_body(&pool, &worker_ref).await;
        drop(_streamed.body);

        // The reader reports WHY it stopped: the byte limit.
        fixture
            .fire
            .lock()
            .unwrap()
            .take()
            .expect("signal still pending")
            .send(StreamCompletion::Abandoned(AbandonedStream::BytesLimit))
            .unwrap();
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }

        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            1,
            "a failed drain must recycle the process"
        );
        let terminated = terminated_event(&mut lifecycle_rx);
        assert!(
            terminated.is_some_and(|event| {
                event.reason == "stream_abandoned_recycled" && event.detail == Some("bytes_limit")
            }),
            "the recycle detail must carry the drain cause (bytes_limit)"
        );
    }

    // (EDG-9, amendment 2) The pool's bounded wait for the drain result
    // EXPIRES before the reader reports anything (slow producer): the
    // recycle carries the explicit sub-cause `relay_timeout` — never a
    // generic `None` — and the reader's LATE result, which lands after
    // the relay receiver is gone, is ignored without error.
    #[tokio::test(flavor = "current_thread")]
    async fn relay_wait_expiry_recycles_with_the_relay_timeout_cause() {
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::channel(16);
        let fixture = Arc::new(DropAbandonFixture::default());
        // drain_wait = 50 ms (max_ms) + 250 ms grace = 300 ms; the signal
        // only resolves ~1 s after the drop, so the wait is guaranteed to
        // expire first.
        let (pool, worker_ref) = abandon_pool(&fixture, "edg9-relay-timeout", Some(lifecycle_tx));

        let _streamed = fetch_stream_body(&pool, &worker_ref).await;
        // The late reader: it reports the drain cause AFTER the pool's
        // bounded wait has already expired — that result must be ignored
        // without error (the relay receiver is already dropped, so the
        // observer's send just fails, harmlessly).
        let late = fixture
            .fire
            .lock()
            .unwrap()
            .take()
            .expect("signal still pending");
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1_000)).await;
            let _ = late.send(StreamCompletion::Abandoned(AbandonedStream::TimeLimit));
        });
        drop(_streamed.body);

        // The wait (300 ms) expires and the recycle runs.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            1,
            "an expired wait must recycle the process"
        );
        // Let the late signal land: it must be ignored without error.
        tokio::time::sleep(Duration::from_millis(800)).await;

        let mut events = Vec::new();
        while let Ok(event) = lifecycle_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .all(|event| event.kind != WorkerLifecycleEventKind::DrainTimedOut),
            "an expired relay wait is a known cause, not a shutdown timeout"
        );
        let terminated = events
            .into_iter()
            .find(|event| event.kind == WorkerLifecycleEventKind::Terminated);
        assert!(
            terminated.is_some_and(|event| {
                event.reason == "stream_abandoned_recycled" && event.detail == Some("relay_timeout")
            }),
            "the expired wait must recycle with the explicit relay_timeout sub-cause"
        );
    }

    // (EDG-9, amendment 2) The termination report classifies the socket as
    // NOT reclaimed (`SocketPoisoned` — nothing was sent, no ack was ever
    // awaited): the termination carries the reason `socket_poisoned` (never
    // `drain_timeout`, never `DrainTimedOut`) and keeps the drain cause in
    // the detail.
    #[tokio::test(flavor = "current_thread")]
    async fn socket_poisoned_termination_reports_socket_poisoned_not_drain_timeout() {
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::channel(16);
        let fixture = Arc::new(DropAbandonFixture::default());
        let (pool, worker_ref) =
            abandon_pool(&fixture, "edg9-poison-terminate", Some(lifecycle_tx));

        let _streamed = fetch_stream_body(&pool, &worker_ref).await;
        // The isolate reports: the socket could not be reclaimed — no
        // shutdown was sent, so this is NOT a timeout.
        fixture
            .terminate_outcome
            .lock()
            .unwrap()
            .replace(edger_core::TerminationOutcome::SocketPoisoned);
        // The drain cause still rides the relay into the detail.
        fixture
            .fire
            .lock()
            .unwrap()
            .take()
            .expect("signal still pending")
            .send(StreamCompletion::Abandoned(AbandonedStream::BytesLimit))
            .unwrap();
        drop(_streamed.body);
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }

        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            1,
            "a poisoned-socket recycle must terminate the isolate"
        );
        let mut events = Vec::new();
        while let Ok(event) = lifecycle_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .all(|event| event.kind != WorkerLifecycleEventKind::DrainTimedOut),
            "a poisoned socket is not a shutdown timeout"
        );
        let terminated = events
            .into_iter()
            .find(|event| event.kind == WorkerLifecycleEventKind::Terminated);
        assert!(
            terminated.is_some_and(|event| {
                event.reason == "socket_poisoned"
                    && event.detail == Some("bytes_limit")
            }),
            "the termination must carry the reason socket_poisoned, keeping the drain cause in the detail"
        );
    }

    // (EDG-9) `drain_timeout` only when the ACTUAL wait timed out: the
    // isolate lock is held past grace+500 ms (800 ms) but under the 5 s
    // lock timeout, and the termination itself completes — the aggregate
    // elapsed time must NOT be rewritten into a `drain_timeout`; the real
    // reason is preserved and no `DrainTimedOut` event is emitted.
    #[tokio::test(flavor = "current_thread")]
    async fn lock_held_past_grace_then_completed_keeps_the_real_reason() {
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::channel(16);
        let fixture = Arc::new(DropAbandonFixture::default());
        // `max_ms: 0` disables the relay wait: the drop recycles IMMEDIATELY
        // (the terminate below models that recycle).
        let (pool, worker_ref) = abandon_pool_limits(
            &fixture,
            "edg9-lock-held",
            Some(lifecycle_tx),
            AbandonDrainLimits {
                max_bytes: edger_core::STREAM_ABANDON_DRAIN_MAX_BYTES_DEFAULT,
                max_ms: 0,
            },
        );

        // NOTE: the isolate is locked on an instance built WITHOUT an
        // in-flight dispatch — a live stream body holds the SAME isolate
        // lock (the dispatch state keeps the guard for the body's
        // lifetime), so locking it from outside while the body is alive
        // would deadlock (and did: the first version of this test hung).
        let isolate = DropAbandonIsolate {
            fixture: Arc::clone(&fixture),
            fire_rx: None,
        };
        let instance = Arc::new(WorkerInstance::new(worker_ref.clone(), Box::new(isolate)));
        let lock_guard = instance.isolate().lock_owned().await;

        let terminate_pool = pool.clone();
        let terminate_instance = Arc::clone(&instance);
        let terminate = tokio::spawn(async move {
            terminate_pool
                .terminate_isolate_with_lifecycle_detail(
                    &terminate_instance,
                    "stream_abandoned_recycled",
                    Some("bytes_limit"),
                    true,
                )
                .await;
        });

        // The terminate task is parked on the isolate lock. Hold it for
        // 800 ms: past the ack deadline (grace 0 + 500 ms margin) and far
        // under the 5 s lock timeout.
        tokio::time::sleep(Duration::from_millis(800)).await;
        drop(lock_guard);
        tokio::time::timeout(Duration::from_secs(5), terminate)
            .await
            .expect("the terminate must finish once the lock frees")
            .expect("the terminate task must not panic");

        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            1,
            "the terminate must run once the lock frees"
        );
        // Collect the events once (the helpers below DRAIN non-matching
        // events, so they must not be run twice on the same channel).
        let mut events = Vec::new();
        while let Ok(event) = lifecycle_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .all(|event| event.kind != WorkerLifecycleEventKind::DrainTimedOut),
            "a completed wait must not emit DrainTimedOut"
        );
        let terminated = events
            .into_iter()
            .find(|event| event.kind == WorkerLifecycleEventKind::Terminated);
        assert!(
            terminated.is_some_and(|event| {
                event.reason == "stream_abandoned_recycled" && event.detail == Some("bytes_limit")
            }),
            "the real reason must be preserved (no drain_timeout rewrite)"
        );
    }

    // (EDG-9) With the drain disabled (a `0` limit) the relay wait is
    // `Duration::ZERO`: the termination must start WITHOUT advancing the
    // clock by the 250 ms grace — a pending signal must not hold it. This
    // test never sleeps, so a 250 ms (or larger) wait would never elapse
    // and the assertion below would fail.
    #[tokio::test(flavor = "current_thread")]
    async fn disabled_drain_recycles_without_waiting_for_the_signal() {
        let (lifecycle_tx, mut lifecycle_rx) = tokio::sync::mpsc::channel(16);
        let fixture = Arc::new(DropAbandonFixture::default());
        let (pool, worker_ref) = abandon_pool_limits(
            &fixture,
            "edg9-disabled-wait",
            Some(lifecycle_tx),
            AbandonDrainLimits {
                max_bytes: edger_core::STREAM_ABANDON_DRAIN_MAX_BYTES_DEFAULT,
                max_ms: 0,
            },
        );

        let _streamed = fetch_stream_body(&pool, &worker_ref).await;
        drop(_streamed.body);
        // NO sleep: only yields. The clock has not advanced by the grace.
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }

        assert_eq!(
            fixture.terminated.load(Ordering::SeqCst),
            1,
            "a disabled drain must recycle immediately, without the relay wait"
        );
        // The pool knows the sub-cause up front: the reader abandoned the
        // socket (drain disabled ⇒ poisoned).
        let terminated = terminated_event(&mut lifecycle_rx);
        assert!(
            terminated.is_some_and(|event| {
                event.reason == "stream_abandoned_recycled"
                    && event.detail == Some("socket_poisoned")
            }),
            "a disabled-drain recycle must carry the socket_poisoned sub-cause"
        );
    }
}
