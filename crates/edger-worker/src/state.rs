//! Worker lifecycle state machine (design.md stateDiagram-v2).

use crate::error::WorkerError;

/// Supervisor-managed worker lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkerState {
    Creating,
    Ready,
    Active,
    Idle,
    Terminating,
    Terminated,
    EphemeralTerm,
}

/// Events that drive state transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerEvent {
    ReadySignal,
    Dispatch,
    RequestComplete { ttl_ms: u64 },
    TtlExpired,
    MaxRequestsReached,
    CriticalError,
    CleanupComplete,
    EphemeralComplete,
}

/// Pure transition table — illegal moves return `WorkerError::InvalidTransition`.
pub fn transition(state: WorkerState, event: WorkerEvent) -> Result<WorkerState, WorkerError> {
    use WorkerEvent::*;
    use WorkerState::*;

    let next = match (state, event) {
        (Creating, ReadySignal) => Ready,
        (Ready, Dispatch) => Active,
        (Ready, RequestComplete { ttl_ms: 0 }) => EphemeralTerm,
        (Idle, Dispatch) => Active,
        (Active, RequestComplete { ttl_ms: 0 }) => EphemeralTerm,
        (Active, RequestComplete { .. }) => Idle,
        (Active, CriticalError) | (Idle, CriticalError) => Terminating,
        (Idle, TtlExpired) | (Idle, MaxRequestsReached) => Terminating,
        (Terminating, CleanupComplete) => Terminated,
        (EphemeralTerm, EphemeralComplete) => Terminated,
        (Terminated, _) | (EphemeralTerm, _) if !matches!(event, EphemeralComplete) => {
            return Err(WorkerError::InvalidTransition { from: state, event });
        }
        _ => {
            return Err(WorkerError::InvalidTransition { from: state, event });
        }
    };
    Ok(next)
}

/// Whether a worker in this state may accept a new dispatch.
pub fn accepts_dispatch(state: WorkerState) -> bool {
    matches!(state, WorkerState::Ready | WorkerState::Idle)
}
