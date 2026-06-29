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
    #[error("isolation error: {0}")]
    Isolation(#[from] edger_core::IsolationError),
}
