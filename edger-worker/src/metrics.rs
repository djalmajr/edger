//! Pool metrics collector and per-worker stats snapshots.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use uuid::Uuid;

use crate::state::WorkerState;

const SPAWN_LATENCY_SAMPLES: usize = 16;

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
    /// Last request duration (milliseconds) — histogram stub.
    pub request_duration_ms_last: u64,
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
            request_duration_ms_last: self.request_duration_ms_last.load(Ordering::Relaxed),
        }
    }
}

fn percentile_p50(samples: &[u64]) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() / 2]
}
