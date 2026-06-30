//! Isolate factory injection — production code depends only on `edger-core` trait.

use edger_core::{Isolate, WorkerRef};

/// Creates isolate instances for new worker entries (injected by orchestrator or tests).
pub trait IsolateFactory: Send + Sync {
    fn create_isolate(&self, worker_ref: &WorkerRef) -> Box<dyn Isolate>;
}
