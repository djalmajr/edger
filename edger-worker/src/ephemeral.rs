//! Ephemeral worker concurrency gate (ttl_ms == 0).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::error::WorkerError;
use crate::metrics::MetricsCollector;

/// Limits parallel ephemeral workers and optional wait queue depth.
#[derive(Debug)]
pub struct EphemeralGate {
    semaphore: Arc<Semaphore>,
    concurrency: usize,
    queue_limit: usize,
    queued: AtomicUsize,
    metrics: Arc<MetricsCollector>,
}

impl EphemeralGate {
    pub fn new(concurrency: usize, queue_limit: usize, metrics: Arc<MetricsCollector>) -> Self {
        let permits = concurrency.max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(permits)),
            concurrency: permits,
            queue_limit,
            queued: AtomicUsize::new(0),
            metrics,
        }
    }

    fn sync_ephemeral_metrics(&self) {
        let available = self.semaphore.available_permits();
        let inflight = self.concurrency.saturating_sub(available) as u64;
        self.metrics.set_ephemeral_inflight(inflight);
        self.metrics
            .set_ephemeral_queued(self.queued.load(Ordering::Relaxed) as u64);
    }

    /// Acquire a slot for an ephemeral fetch; rejects when wait queue is full.
    pub async fn acquire(&self) -> Result<EphemeralPermit<'_>, WorkerError> {
        if let Ok(permit) = self.semaphore.clone().try_acquire_owned() {
            self.sync_ephemeral_metrics();
            return Ok(EphemeralPermit {
                _permit: permit,
                gate: self,
            });
        }

        let waiting = self.queued.fetch_add(1, Ordering::SeqCst);
        self.sync_ephemeral_metrics();

        if waiting >= self.queue_limit {
            self.queued.fetch_sub(1, Ordering::SeqCst);
            self.metrics.record_ephemeral_rejected();
            self.sync_ephemeral_metrics();
            return Err(WorkerError::EphemeralQueueFull);
        }

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| WorkerError::Shutdown)?;

        self.queued.fetch_sub(1, Ordering::SeqCst);
        self.sync_ephemeral_metrics();

        Ok(EphemeralPermit {
            _permit: permit,
            gate: self,
        })
    }
}

/// RAII release of an ephemeral concurrency slot.
pub struct EphemeralPermit<'a> {
    _permit: OwnedSemaphorePermit,
    gate: &'a EphemeralGate,
}

impl Drop for EphemeralPermit<'_> {
    fn drop(&mut self) {
        self.gate.sync_ephemeral_metrics();
    }
}
