//! Worker instance with supervisor-managed lifecycle state.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use edger_core::{Isolate, WorkerRef};
use tokio::sync::Mutex as AsyncMutex;

use crate::state::WorkerState;

/// A pooled worker with an injected isolate backend and lifecycle state.
pub struct WorkerInstance {
    pub worker_ref: WorkerRef,
    created_at: Instant,
    dispatch_lock: Arc<AsyncMutex<()>>,
    isolate: Arc<AsyncMutex<Box<dyn Isolate>>>,
    state: Mutex<WorkerState>,
    request_count: Mutex<u32>,
    unhealthy: AtomicBool,
    idle_notifications: AtomicU32,
    ttl_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl WorkerInstance {
    pub fn new(worker_ref: WorkerRef, isolate: Box<dyn Isolate>) -> Self {
        Self {
            worker_ref,
            created_at: Instant::now(),
            dispatch_lock: Arc::new(AsyncMutex::new(())),
            isolate: Arc::new(AsyncMutex::new(isolate)),
            state: Mutex::new(WorkerState::Creating),
            request_count: Mutex::new(0),
            unhealthy: AtomicBool::new(false),
            idle_notifications: AtomicU32::new(0),
            ttl_handle: Mutex::new(None),
        }
    }

    pub fn isolate(&self) -> Arc<AsyncMutex<Box<dyn Isolate>>> {
        Arc::clone(&self.isolate)
    }

    pub fn dispatch_lock(&self) -> Arc<AsyncMutex<()>> {
        Arc::clone(&self.dispatch_lock)
    }

    pub fn state(&self) -> WorkerState {
        *self.state.lock().expect("state lock")
    }

    pub fn set_state(&self, state: WorkerState) {
        *self.state.lock().expect("state lock") = state;
    }

    pub fn state_lock(&self) -> std::sync::MutexGuard<'_, WorkerState> {
        self.state.lock().expect("state lock")
    }

    pub fn request_count(&self) -> u32 {
        *self.request_count.lock().expect("request_count lock")
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.created_at.elapsed().as_secs()
    }

    pub fn increment_request_count(&self) -> u32 {
        let mut count = self.request_count.lock().expect("request_count lock");
        *count += 1;
        *count
    }

    pub fn is_unhealthy(&self) -> bool {
        self.unhealthy.load(Ordering::SeqCst)
    }

    pub fn mark_unhealthy(&self) {
        self.unhealthy.store(true, Ordering::SeqCst);
    }

    pub fn record_idle_notification(&self) {
        self.idle_notifications.fetch_add(1, Ordering::SeqCst);
    }

    pub fn idle_notification_count(&self) -> u32 {
        self.idle_notifications.load(Ordering::SeqCst)
    }

    pub fn set_ttl_handle(&self, handle: tokio::task::JoinHandle<()>) {
        *self.ttl_handle.lock().expect("ttl_handle lock") = Some(handle);
    }

    pub fn cancel_ttl_timer(&self) {
        if let Some(handle) = self.ttl_handle.lock().expect("ttl_handle lock").take() {
            handle.abort();
        }
    }
}
