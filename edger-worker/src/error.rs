//! Worker pool errors.

use crate::state::{WorkerEvent, WorkerState};

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("pool is shut down")]
    Shutdown,
    #[error("worker collision for key {key}: {detail}")]
    Collision { key: String, detail: String },
    #[error("worker evicted from pool (LRU full)")]
    Evicted,
    #[error("worker not ready for dispatch")]
    NotReady,
    #[error("invalid transition from {from:?} on event {event:?}")]
    InvalidTransition {
        from: WorkerState,
        event: WorkerEvent,
    },
    #[error("ephemeral queue full (concurrency limit reached)")]
    EphemeralQueueFull,
    #[error("worker queue full (all processes busy)")]
    WorkerQueueFull,
    #[error("worker queue timeout (all processes busy)")]
    WorkerQueueTimeout,
    #[error("worker circuit breaker open; retry after {retry_after_ms}ms")]
    CircuitOpen { retry_after_ms: u64 },
    // NOTE: despite the historical name, this is returned for *any* unavailable
    // dispatch slot (`ReservedSlot::Unavailable`), never for a real max_requests
    // retirement (that path terminates + removes without surfacing an error).
    #[error("worker unavailable (no idle instance and process capacity reached)")]
    Retired,
    #[error("isolation error: {0}")]
    Isolation(#[from] edger_core::IsolationError),
}
