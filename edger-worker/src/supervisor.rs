//! Worker supervisor — spawn, TTL timers, request lifecycle hooks.

use std::sync::Arc;
use std::time::Duration;

use edger_core::WorkerConfig;

use crate::error::WorkerError;
use crate::instance::WorkerInstance;
use crate::pool::WorkerPool;
use crate::state::{accepts_dispatch, transition, WorkerEvent, WorkerState};

/// Lifecycle orchestration for a single worker instance.
pub struct Supervisor;

impl Supervisor {
    /// Mock entrypoint load — transitions `Creating` → `Ready`.
    pub async fn spawn(instance: &WorkerInstance) -> Result<(), WorkerError> {
        if instance.state() != WorkerState::Creating {
            return Err(WorkerError::InvalidTransition {
                from: instance.state(),
                event: WorkerEvent::ReadySignal,
            });
        }

        let isolate = instance.isolate();
        let mut guard = isolate.lock().await;
        guard.prepare(&instance.worker_ref.config).await?;
        drop(guard);

        let mut state = instance.state_lock();
        if *state != WorkerState::Creating {
            return Err(WorkerError::InvalidTransition {
                from: *state,
                event: WorkerEvent::ReadySignal,
            });
        }
        *state = transition(WorkerState::Creating, WorkerEvent::ReadySignal)?;
        Ok(())
    }

    /// `Ready`/`Idle` → `Active` before dispatch.
    pub async fn on_request_start(instance: &WorkerInstance) -> Result<(), WorkerError> {
        instance.cancel_ttl_timer();
        let mut state = instance.state_lock();
        if !accepts_dispatch(*state) {
            return Err(WorkerError::NotReady);
        }
        *state = transition(*state, WorkerEvent::Dispatch)?;
        Ok(())
    }

    /// `Active` → `Idle` or `EphemeralTerm`; handles max_requests and notify_idle.
    pub async fn on_request_complete(
        instance: Arc<WorkerInstance>,
        config: &WorkerConfig,
        pool: &WorkerPool,
    ) -> Result<(), WorkerError> {
        let count = instance.increment_request_count();
        let ttl_ms = config.ttl_ms;

        let next = {
            let state = instance.state_lock();
            if *state != WorkerState::Active {
                return Err(WorkerError::InvalidTransition {
                    from: *state,
                    event: WorkerEvent::RequestComplete { ttl_ms },
                });
            }
            transition(*state, WorkerEvent::RequestComplete { ttl_ms })?
        };

        instance.set_state(next);

        if next == WorkerState::Idle {
            instance.cancel_ttl_timer();

            if config.max_requests > 0 && count >= config.max_requests {
                Self::retire_for_max_requests(&instance, pool).await?;
                return Ok(());
            }

            let isolate = instance.isolate();
            let mut guard = isolate.lock().await;
            let _ = guard.notify_idle().await;
            instance.record_idle_notification();

            if ttl_ms > 0 {
                Self::schedule_ttl_timer(instance, pool.clone(), ttl_ms);
            }
        } else if next == WorkerState::EphemeralTerm {
            Self::finish_ephemeral(&instance, pool).await?;
        }

        Ok(())
    }

    pub async fn on_critical_error(
        instance: &WorkerInstance,
        pool: &WorkerPool,
    ) -> Result<(), WorkerError> {
        instance.mark_unhealthy();
        instance.cancel_ttl_timer();
        {
            let mut state = instance.state_lock();
            *state = transition(*state, WorkerEvent::CriticalError)?;
        }
        Self::cleanup(instance, pool, "critical_error").await?;
        Ok(())
    }

    /// Invoked by TTL timer when sliding window expires (also used in tests).
    pub async fn on_ttl_expired(
        instance: &WorkerInstance,
        pool: &WorkerPool,
    ) -> Result<(), WorkerError> {
        if instance.state() != WorkerState::Idle {
            return Ok(());
        }
        Self::begin_termination(instance, pool, WorkerEvent::TtlExpired).await
    }

    async fn retire_for_max_requests(
        instance: &WorkerInstance,
        pool: &WorkerPool,
    ) -> Result<(), WorkerError> {
        instance.set_state(WorkerState::Terminating);
        pool.terminate_isolate_with_lifecycle(instance, "max_requests")
            .await;
        instance.set_state(WorkerState::Terminated);
        pool.remove_instance(instance);
        Ok(())
    }

    async fn begin_termination(
        instance: &WorkerInstance,
        pool: &WorkerPool,
        event: WorkerEvent,
    ) -> Result<(), WorkerError> {
        // Detach (do NOT abort): this runs inside the fired TTL timer task, so
        // aborting its own handle here would cancel the termination before
        // `cleanup()` -> `Terminated` -> `remove_instance` complete, leaving the
        // instance wedged in `Terminating` and permanently `WorkerError::Retired`.
        instance.clear_ttl_timer();
        {
            let mut state = instance.state_lock();
            *state = transition(*state, event)?;
        }
        Self::cleanup(instance, pool, "ttl_expired").await?;
        pool.remove_instance(instance);
        Ok(())
    }

    async fn finish_ephemeral(
        instance: &WorkerInstance,
        pool: &WorkerPool,
    ) -> Result<(), WorkerError> {
        pool.terminate_isolate_with_lifecycle(instance, "ephemeral_complete")
            .await;

        instance.set_state(transition(
            WorkerState::EphemeralTerm,
            WorkerEvent::EphemeralComplete,
        )?);
        pool.remove_instance(instance);
        Ok(())
    }

    async fn cleanup(
        instance: &WorkerInstance,
        pool: &WorkerPool,
        reason: &'static str,
    ) -> Result<(), WorkerError> {
        {
            let mut state = instance.state_lock();
            if *state != WorkerState::Terminating {
                *state = WorkerState::Terminating;
            }
        }

        pool.terminate_isolate_with_lifecycle(instance, reason)
            .await;

        instance.set_state(transition(
            WorkerState::Terminating,
            WorkerEvent::CleanupComplete,
        )?);
        Ok(())
    }

    fn schedule_ttl_timer(instance: Arc<WorkerInstance>, pool: WorkerPool, ttl_ms: u64) {
        let timer_instance = Arc::clone(&instance);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ttl_ms)).await;
            // Detach our own handle BEFORE running termination. `sleep` returned
            // and no `.await` precedes this `clear`, so it runs atomically: from
            // here on neither `begin_termination` nor a racing request's
            // `cancel_ttl_timer()` can `abort()` this task mid-`cleanup()`.
            timer_instance.clear_ttl_timer();
            let _ = Supervisor::on_ttl_expired(&timer_instance, &pool).await;
        });
        instance.set_ttl_handle(handle);
    }
}
