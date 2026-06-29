//! Isolate factory injection — production code depends only on `edger-core` trait.

use edger_core::Isolate;

/// Creates isolate instances for new worker entries (injected by orchestrator or tests).
pub trait IsolateFactory: Send + Sync {
    fn create_isolate(&self) -> Box<dyn Isolate>;
}
