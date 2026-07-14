//! Pool metrics collector and per-worker stats snapshots.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use edger_core::WorkerRef;
use uuid::Uuid;

use crate::state::WorkerState;

const SPAWN_LATENCY_SAMPLES: usize = 16;
const WORKER_WAIT_SAMPLES: usize = 16;
const WORKER_DURATION_SAMPLES: usize = 64;
pub const PASSIVE_HEALTH_WINDOW_MS: u64 = 5 * 60 * 1_000;
const PASSIVE_HEALTH_SAMPLES: usize = 64;

/// Snapshot of pool-level metrics (cloneable, orchestrator-facing).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolMetrics {
    /// Workers currently in the LRU cache.
    pub active_workers: usize,
    /// Workers in `Idle` state.
    pub idle_workers: usize,
    /// Total workers terminated since pool creation.
    pub terminated_total: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// Last observed spawn latency on cache miss (milliseconds).
    pub spawn_latency_ms_last: u64,
    /// Stub p50 over recent spawn samples.
    pub spawn_latency_ms_p50: u64,
    /// Ephemeral workers currently executing.
    pub ephemeral_inflight: u64,
    /// Ephemeral workers waiting for a concurrency slot.
    pub ephemeral_queued: u64,
    /// Ephemeral requests rejected (queue full).
    pub ephemeral_rejected: u64,
    /// Persistent-worker requests currently waiting for a process slot.
    pub worker_queue_queued: u64,
    /// Persistent-worker requests admitted into the bounded wait queue.
    pub worker_queue_enqueued: u64,
    /// Persistent-worker requests rejected because the bounded wait queue was full.
    pub worker_queue_rejected: u64,
    /// Persistent-worker requests that timed out while waiting for a process slot.
    pub worker_queue_timeout: u64,
    /// Last persistent-worker queue wait duration (milliseconds).
    pub worker_queue_wait_ms_last: u64,
    /// Last request duration (milliseconds) — histogram stub.
    pub request_duration_ms_last: u64,
    /// Per-worker-group process, queue, wait, and recycle snapshots.
    pub worker_groups: Vec<WorkerGroupMetrics>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerGroupMetrics {
    pub active_processes: usize,
    pub enqueued_total: u64,
    pub health: WorkerHealthMetrics,
    pub idle_processes: usize,
    pub max_processes: usize,
    pub name: String,
    pub namespace: Option<String>,
    pub processes: Vec<WorkerProcessMetrics>,
    pub queued: u64,
    pub recycle_error_total: u64,
    pub recycle_max_requests_total: u64,
    pub recycle_oom_shutdown_total: u64,
    pub recycle_ttl_total: u64,
    pub rejected_total: u64,
    pub request_duration_ms_last: u64,
    pub request_duration_ms_p95: u64,
    pub request_total: u64,
    pub terminating_processes: usize,
    pub timeout_total: u64,
    pub total_processes: usize,
    pub version: String,
    pub wait_ms_last: u64,
    pub wait_ms_p50: u64,
    pub wait_ms_p95: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkerHealthStatus {
    #[default]
    Unobserved,
    Healthy,
    Degraded,
    Failing,
}

impl WorkerHealthStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unobserved => "unobserved",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Failing => "failing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerHealthMetrics {
    pub consecutive_failures: u64,
    pub failure_count: u64,
    pub last_failure_at_ms: Option<u64>,
    pub last_failure_code: Option<String>,
    pub last_success_at_ms: Option<u64>,
    pub observed_at_ms: Option<u64>,
    pub sample_count: u64,
    pub status: WorkerHealthStatus,
    pub success_count: u64,
    pub window_ms: u64,
}

impl Default for WorkerHealthMetrics {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            failure_count: 0,
            last_failure_at_ms: None,
            last_failure_code: None,
            last_success_at_ms: None,
            observed_at_ms: None,
            sample_count: 0,
            status: WorkerHealthStatus::Unobserved,
            success_count: 0,
            window_ms: PASSIVE_HEALTH_WINDOW_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerRequestOutcome {
    Success,
    Http5xx,
    IsolationError,
    Rejected,
    Timeout,
}

impl WorkerRequestOutcome {
    fn is_failure(self) -> bool {
        !matches!(self, Self::Success)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Http5xx => "http_5xx",
            Self::IsolationError => "isolation_error",
            Self::Rejected => "rejected",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HealthObservation {
    at_ms: u64,
    outcome: WorkerRequestOutcome,
}

impl WorkerGroupMetrics {
    pub fn identity(&self) -> WorkerGroupIdentity {
        WorkerGroupIdentity {
            name: self.name.clone(),
            namespace: self.namespace.clone(),
            version: self.version.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerProcessMetrics {
    pub request_count: u32,
    pub state: WorkerState,
    pub unhealthy: bool,
    pub uptime_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkerGroupIdentity {
    pub name: String,
    pub namespace: Option<String>,
    pub version: String,
}

impl WorkerGroupIdentity {
    pub fn from_worker_ref(worker_ref: &WorkerRef) -> Self {
        Self {
            name: worker_ref.name.clone(),
            namespace: worker_ref.namespace.clone(),
            version: worker_ref.version.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerRecycleCause {
    Error,
    MaxRequests,
    OomShutdown,
    Ttl,
}

impl WorkerRecycleCause {
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::MaxRequests => "max_requests",
            Self::OomShutdown => "oom_shutdown",
            Self::Ttl => "ttl",
        }
    }
}

/// Per-worker stats for observability hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerStats {
    pub app: String,
    pub name: String,
    pub namespace: Option<String>,
    pub request_count: u32,
    pub state: WorkerState,
    pub unhealthy: bool,
    pub uptime_seconds: u64,
    pub version: String,
    pub worker_id: Uuid,
}

/// Thread-safe metrics collector (atomics + small latency ring).
#[derive(Debug)]
pub struct MetricsCollector {
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    terminated_total: AtomicU64,
    active_workers: AtomicU64,
    idle_workers: AtomicU64,
    spawn_latency_ms_last: AtomicU64,
    spawn_samples: Mutex<VecDeque<u64>>,
    request_duration_ms_last: AtomicU64,
    ephemeral_inflight: AtomicU64,
    ephemeral_queued: AtomicU64,
    ephemeral_rejected: AtomicU64,
    worker_queue_queued: AtomicU64,
    worker_queue_enqueued: AtomicU64,
    worker_queue_rejected: AtomicU64,
    worker_queue_timeout: AtomicU64,
    worker_queue_wait_ms_last: AtomicU64,
    worker_groups: Mutex<BTreeMap<WorkerGroupIdentity, WorkerGroupRuntimeMetrics>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerGroupRuntimeMetrics {
    pub enqueued_total: u64,
    pub queued: u64,
    pub recycle_error_total: u64,
    pub recycle_max_requests_total: u64,
    pub recycle_oom_shutdown_total: u64,
    pub recycle_ttl_total: u64,
    pub rejected_total: u64,
    pub request_duration_ms_last: u64,
    pub request_duration_ms_p95: u64,
    pub request_total: u64,
    pub timeout_total: u64,
    pub wait_ms_last: u64,
    pub wait_ms_p50: u64,
    pub wait_ms_p95: u64,
    wait_samples: VecDeque<u64>,
    duration_samples: VecDeque<u64>,
    health_observations: VecDeque<HealthObservation>,
}

impl WorkerGroupRuntimeMetrics {
    fn record_outcome(&mut self, outcome: WorkerRequestOutcome, at_ms: u64) {
        while self.health_observations.len() >= PASSIVE_HEALTH_SAMPLES {
            self.health_observations.pop_front();
        }
        self.health_observations
            .push_back(HealthObservation { at_ms, outcome });
        self.prune_health(at_ms);
    }

    pub(crate) fn health_at(&self, now_ms: u64) -> WorkerHealthMetrics {
        let window_start = now_ms.saturating_sub(PASSIVE_HEALTH_WINDOW_MS);
        let observations = self
            .health_observations
            .iter()
            .filter(|observation| observation.at_ms >= window_start && observation.at_ms <= now_ms)
            .collect::<Vec<_>>();
        if observations.is_empty() {
            return WorkerHealthMetrics {
                status: WorkerHealthStatus::Unobserved,
                window_ms: PASSIVE_HEALTH_WINDOW_MS,
                ..Default::default()
            };
        }

        let success_count = observations
            .iter()
            .filter(|observation| !observation.outcome.is_failure())
            .count() as u64;
        let failure_count = observations.len() as u64 - success_count;
        let consecutive_failures = observations
            .iter()
            .rev()
            .take_while(|observation| observation.outcome.is_failure())
            .count() as u64;
        let last_success_at_ms = observations
            .iter()
            .rev()
            .find(|observation| !observation.outcome.is_failure())
            .map(|observation| observation.at_ms);
        let last_failure = observations
            .iter()
            .rev()
            .find(|observation| observation.outcome.is_failure());
        let status = if consecutive_failures >= 3 || (success_count == 0 && failure_count >= 3) {
            WorkerHealthStatus::Failing
        } else if failure_count > 0 {
            WorkerHealthStatus::Degraded
        } else {
            WorkerHealthStatus::Healthy
        };

        WorkerHealthMetrics {
            consecutive_failures,
            failure_count,
            last_failure_at_ms: last_failure.map(|observation| observation.at_ms),
            last_failure_code: last_failure
                .map(|observation| observation.outcome.label().to_string()),
            last_success_at_ms,
            observed_at_ms: observations.last().map(|observation| observation.at_ms),
            sample_count: observations.len() as u64,
            status,
            success_count,
            window_ms: PASSIVE_HEALTH_WINDOW_MS,
        }
    }

    fn prune_health(&mut self, now_ms: u64) {
        let window_start = now_ms.saturating_sub(PASSIVE_HEALTH_WINDOW_MS);
        while self
            .health_observations
            .front()
            .is_some_and(|observation| observation.at_ms < window_start)
        {
            self.health_observations.pop_front();
        }
    }

    fn record_request(&mut self, ms: u64) {
        self.request_total += 1;
        self.request_duration_ms_last = ms;
        if self.duration_samples.len() >= WORKER_DURATION_SAMPLES {
            self.duration_samples.pop_front();
        }
        self.duration_samples.push_back(ms);
        let samples = self.duration_samples.iter().copied().collect::<Vec<_>>();
        self.request_duration_ms_p95 = percentile(&samples, 95);
    }

    fn record_wait(&mut self, ms: u64) {
        self.wait_ms_last = ms;
        if self.wait_samples.len() >= WORKER_WAIT_SAMPLES {
            self.wait_samples.pop_front();
        }
        self.wait_samples.push_back(ms);
        let samples = self.wait_samples.iter().copied().collect::<Vec<_>>();
        self.wait_ms_p50 = percentile(&samples, 50);
        self.wait_ms_p95 = percentile(&samples, 95);
    }

    fn record_recycle(&mut self, cause: WorkerRecycleCause) {
        match cause {
            WorkerRecycleCause::Error => self.recycle_error_total += 1,
            WorkerRecycleCause::MaxRequests => self.recycle_max_requests_total += 1,
            WorkerRecycleCause::OomShutdown => self.recycle_oom_shutdown_total += 1,
            WorkerRecycleCause::Ttl => self.recycle_ttl_total += 1,
        }
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self {
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            terminated_total: AtomicU64::new(0),
            active_workers: AtomicU64::new(0),
            idle_workers: AtomicU64::new(0),
            spawn_latency_ms_last: AtomicU64::new(0),
            spawn_samples: Mutex::new(VecDeque::with_capacity(SPAWN_LATENCY_SAMPLES)),
            request_duration_ms_last: AtomicU64::new(0),
            ephemeral_inflight: AtomicU64::new(0),
            ephemeral_queued: AtomicU64::new(0),
            ephemeral_rejected: AtomicU64::new(0),
            worker_queue_queued: AtomicU64::new(0),
            worker_queue_enqueued: AtomicU64::new(0),
            worker_queue_rejected: AtomicU64::new(0),
            worker_queue_timeout: AtomicU64::new(0),
            worker_queue_wait_ms_last: AtomicU64::new(0),
            worker_groups: Mutex::new(BTreeMap::new()),
        }
    }
}

impl MetricsCollector {
    pub fn record_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_terminated(&self) {
        self.terminated_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_active_workers(&self, count: usize) {
        self.active_workers.store(count as u64, Ordering::Relaxed);
    }

    pub fn set_idle_workers(&self, count: usize) {
        self.idle_workers.store(count as u64, Ordering::Relaxed);
    }

    pub fn record_spawn_latency(&self, ms: u64) {
        self.spawn_latency_ms_last.store(ms, Ordering::Relaxed);
        let mut samples = self.spawn_samples.lock().expect("spawn_samples lock");
        if samples.len() >= SPAWN_LATENCY_SAMPLES {
            samples.pop_front();
        }
        samples.push_back(ms);
    }

    pub fn record_request_duration(&self, ms: u64) {
        self.request_duration_ms_last.store(ms, Ordering::Relaxed);
    }

    pub fn set_ephemeral_inflight(&self, n: u64) {
        self.ephemeral_inflight.store(n, Ordering::Relaxed);
    }

    pub fn set_ephemeral_queued(&self, n: u64) {
        self.ephemeral_queued.store(n, Ordering::Relaxed);
    }

    pub fn record_ephemeral_rejected(&self) {
        self.ephemeral_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_worker_queue_queued(&self, n: u64) {
        self.worker_queue_queued.store(n, Ordering::Relaxed);
    }

    pub fn record_worker_queue_enqueued(&self) {
        self.worker_queue_enqueued.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_worker_queue_rejected(&self) {
        self.worker_queue_rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_worker_queue_timeout(&self) {
        self.worker_queue_timeout.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_worker_queue_wait(&self, ms: u64) {
        self.worker_queue_wait_ms_last.store(ms, Ordering::Relaxed);
    }

    pub fn record_worker_group_queue_enqueued(&self, worker_ref: &WorkerRef, queued: u64) {
        self.update_worker_group(worker_ref, |metrics| {
            metrics.enqueued_total += 1;
            metrics.queued = queued;
        });
    }

    pub fn set_worker_group_queued(&self, identity: &WorkerGroupIdentity, queued: u64) {
        self.update_worker_group_identity(identity, |metrics| metrics.queued = queued);
    }

    pub fn record_worker_group_queue_rejected(&self, worker_ref: &WorkerRef, queued: u64) {
        self.update_worker_group(worker_ref, |metrics| {
            metrics.rejected_total += 1;
            metrics.queued = queued;
        });
    }

    pub fn record_worker_group_queue_timeout(
        &self,
        worker_ref: &WorkerRef,
        queued: u64,
        wait_ms: u64,
    ) {
        self.update_worker_group(worker_ref, |metrics| {
            metrics.timeout_total += 1;
            metrics.queued = queued;
            metrics.record_wait(wait_ms);
        });
    }

    pub fn record_worker_group_queue_wait(
        &self,
        worker_ref: &WorkerRef,
        queued: u64,
        wait_ms: u64,
    ) {
        self.update_worker_group(worker_ref, |metrics| {
            metrics.queued = queued;
            metrics.record_wait(wait_ms);
        });
    }

    pub fn record_worker_group_recycle(&self, worker_ref: &WorkerRef, cause: WorkerRecycleCause) {
        self.update_worker_group(worker_ref, |metrics| metrics.record_recycle(cause));
    }

    pub fn record_worker_group_request(&self, worker_ref: &WorkerRef, duration_ms: u64) {
        self.update_worker_group(worker_ref, |metrics| {
            metrics.record_request(duration_ms.max(1))
        });
    }

    pub fn record_worker_group_outcome(
        &self,
        worker_ref: &WorkerRef,
        outcome: WorkerRequestOutcome,
    ) {
        self.record_worker_group_outcome_at(worker_ref, outcome, unix_time_ms());
    }

    fn record_worker_group_outcome_at(
        &self,
        worker_ref: &WorkerRef,
        outcome: WorkerRequestOutcome,
        at_ms: u64,
    ) {
        self.update_worker_group(worker_ref, |metrics| metrics.record_outcome(outcome, at_ms));
    }

    #[cfg(test)]
    fn worker_group_health_at(
        &self,
        identity: &WorkerGroupIdentity,
        now_ms: u64,
    ) -> WorkerHealthMetrics {
        self.worker_groups
            .lock()
            .expect("worker_groups lock")
            .get(identity)
            .map(|metrics| metrics.health_at(now_ms))
            .unwrap_or_else(|| WorkerHealthMetrics {
                status: WorkerHealthStatus::Unobserved,
                window_ms: PASSIVE_HEALTH_WINDOW_MS,
                ..Default::default()
            })
    }

    pub fn worker_group_runtime_snapshots(
        &self,
    ) -> BTreeMap<WorkerGroupIdentity, WorkerGroupRuntimeMetrics> {
        self.worker_groups
            .lock()
            .expect("worker_groups lock")
            .clone()
    }

    pub fn snapshot(&self) -> PoolMetrics {
        let p50 = self
            .spawn_samples
            .lock()
            .expect("spawn_samples lock")
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let spawn_latency_ms_p50 = percentile_p50(&p50);

        PoolMetrics {
            active_workers: self.active_workers.load(Ordering::Relaxed) as usize,
            idle_workers: self.idle_workers.load(Ordering::Relaxed) as usize,
            terminated_total: self.terminated_total.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            spawn_latency_ms_last: self.spawn_latency_ms_last.load(Ordering::Relaxed),
            spawn_latency_ms_p50,
            ephemeral_inflight: self.ephemeral_inflight.load(Ordering::Relaxed),
            ephemeral_queued: self.ephemeral_queued.load(Ordering::Relaxed),
            ephemeral_rejected: self.ephemeral_rejected.load(Ordering::Relaxed),
            worker_queue_queued: self.worker_queue_queued.load(Ordering::Relaxed),
            worker_queue_enqueued: self.worker_queue_enqueued.load(Ordering::Relaxed),
            worker_queue_rejected: self.worker_queue_rejected.load(Ordering::Relaxed),
            worker_queue_timeout: self.worker_queue_timeout.load(Ordering::Relaxed),
            worker_queue_wait_ms_last: self.worker_queue_wait_ms_last.load(Ordering::Relaxed),
            request_duration_ms_last: self.request_duration_ms_last.load(Ordering::Relaxed),
            worker_groups: Vec::new(),
        }
    }

    fn update_worker_group<F>(&self, worker_ref: &WorkerRef, update: F)
    where
        F: FnOnce(&mut WorkerGroupRuntimeMetrics),
    {
        let key = WorkerGroupIdentity::from_worker_ref(worker_ref);
        self.update_worker_group_identity(&key, update);
    }

    fn update_worker_group_identity<F>(&self, identity: &WorkerGroupIdentity, update: F)
    where
        F: FnOnce(&mut WorkerGroupRuntimeMetrics),
    {
        let mut groups = self.worker_groups.lock().expect("worker_groups lock");
        update(groups.entry(identity.clone()).or_default());
    }
}

pub(crate) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn percentile_p50(samples: &[u64]) -> u64 {
    percentile(samples, 50)
}

fn percentile(samples: &[u64], percentile: u64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) as u64 * percentile / 100) as usize;
    sorted[index]
}

#[cfg(test)]
mod tests {
    use super::*;
    use edger_core::{create_worker_ref, WorkerManifest};
    use std::path::PathBuf;

    #[test]
    fn worker_group_queue_metrics_keep_low_cardinality_identity() {
        let collector = MetricsCollector::default();
        let worker_ref = create_worker_ref(
            PathBuf::from("/tmp/secret-absolute-worker-path"),
            WorkerManifest {
                name: "@tenant-a/echo".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();

        collector.record_worker_group_queue_enqueued(&worker_ref, 1);
        collector.record_worker_group_queue_wait(&worker_ref, 0, 9);
        collector.record_worker_group_queue_rejected(&worker_ref, 0);
        collector.record_worker_group_queue_timeout(&worker_ref, 0, 11);
        collector.record_worker_group_recycle(&worker_ref, WorkerRecycleCause::MaxRequests);
        collector.record_worker_group_request(&worker_ref, 7);
        collector.record_worker_group_request(&worker_ref, 13);

        let groups = collector.worker_group_runtime_snapshots();
        let (identity, metrics) = groups.iter().next().expect("worker group metrics");

        assert_eq!(identity.name, "@tenant-a/echo");
        assert_eq!(identity.version, "1.0.0");
        assert_eq!(identity.namespace.as_deref(), Some("@tenant-a"));
        assert_eq!(metrics.enqueued_total, 1);
        assert_eq!(metrics.rejected_total, 1);
        assert_eq!(metrics.timeout_total, 1);
        assert_eq!(metrics.wait_ms_last, 11);
        assert_eq!(metrics.recycle_max_requests_total, 1);
        assert_eq!(metrics.request_total, 2);
        assert_eq!(metrics.request_duration_ms_last, 13);
        assert_eq!(metrics.request_duration_ms_p95, 7);
        assert!(!format!("{identity:?}").contains("/tmp/secret-absolute-worker-path"));
    }

    #[test]
    fn passive_health_uses_a_bounded_window_and_real_outcomes() {
        let collector = MetricsCollector::default();
        let worker_ref = create_worker_ref(
            PathBuf::from("/tmp/health-worker"),
            WorkerManifest {
                name: "health-worker".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let identity = WorkerGroupIdentity::from_worker_ref(&worker_ref);

        assert_eq!(
            collector.worker_group_health_at(&identity, 1_000).status,
            WorkerHealthStatus::Unobserved
        );

        collector.record_worker_group_outcome_at(&worker_ref, WorkerRequestOutcome::Success, 1_000);
        let healthy = collector.worker_group_health_at(&identity, 1_000);
        assert_eq!(healthy.status, WorkerHealthStatus::Healthy);
        assert_eq!(healthy.sample_count, 1);
        assert_eq!(healthy.success_count, 1);

        collector.record_worker_group_outcome_at(&worker_ref, WorkerRequestOutcome::Http5xx, 2_000);
        let degraded = collector.worker_group_health_at(&identity, 2_000);
        assert_eq!(degraded.status, WorkerHealthStatus::Degraded);
        assert_eq!(degraded.failure_count, 1);
        assert_eq!(degraded.last_failure_code.as_deref(), Some("http_5xx"));

        collector.record_worker_group_outcome_at(&worker_ref, WorkerRequestOutcome::Timeout, 3_000);
        collector.record_worker_group_outcome_at(
            &worker_ref,
            WorkerRequestOutcome::IsolationError,
            4_000,
        );
        let failing = collector.worker_group_health_at(&identity, 4_000);
        assert_eq!(failing.status, WorkerHealthStatus::Failing);
        assert_eq!(failing.consecutive_failures, 3);

        let expired =
            collector.worker_group_health_at(&identity, 4_000 + PASSIVE_HEALTH_WINDOW_MS + 1);
        assert_eq!(expired.status, WorkerHealthStatus::Unobserved);
        assert_eq!(expired.sample_count, 0);
    }
}
