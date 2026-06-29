//! Worker instance skeleton (supervisor states in story 04.02).

use std::sync::Arc;

use edger_core::{Isolate, WorkerRef};
use tokio::sync::Mutex;

/// A pooled worker with an injected isolate backend.
pub struct WorkerInstance {
    pub worker_ref: WorkerRef,
    isolate: Arc<Mutex<Box<dyn Isolate>>>,
}

impl WorkerInstance {
    pub fn new(worker_ref: WorkerRef, isolate: Box<dyn Isolate>) -> Self {
        Self {
            worker_ref,
            isolate: Arc::new(Mutex::new(isolate)),
        }
    }

    pub fn isolate(&self) -> Arc<Mutex<Box<dyn Isolate>>> {
        Arc::clone(&self.isolate)
    }
}
