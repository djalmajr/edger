//! Worker supervisor — spawn, TTL timers, request lifecycle hooks.

use std::sync::Arc;
use std::time::Duration;

use edger_core::WorkerConfig;

use crate::error::WorkerError;
use crate::instance::{TtlArm, WorkerInstance};
use crate::pool::WorkerPool;
use crate::state::{accepts_dispatch, transition, WorkerEvent, WorkerState};

/// Outcome of the atomic TTL-floor decision made by the pool (EDG-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlFloorDecision {
    /// The group's `min_processes` floor keeps the instance alive: it stays
    /// `Idle` and its TTL timer is re-armed with the same `ttl_ms`.
    Keep,
    /// The instance was atomically transitioned `Idle -> Terminating` by the
    /// decision and must be cleaned up and removed from the pool.
    Terminate,
    /// The instance is no longer `Idle` (a dispatch won the race): nothing
    /// to do.
    NotIdle,
}

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
                Self::schedule_ttl_timer(&instance, pool, ttl_ms);
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
    ///
    /// (EDG-10) `min_processes` is a MAINTAINED floor, not a one-shot
    /// prewarm: the terminate-or-keep decision is made ATOMICALLY with the
    /// group's living count inside the pool (`reserve_ttl_termination`).
    /// When terminating this instance would drop the group below
    /// `min_processes`, the instance stays `Idle` and the timer is re-armed
    /// with the same `ttl_ms` (if the group grows later, the surplus expires
    /// normally); otherwise the pre-transitioned instance is cleaned up and
    /// removed, which triggers the pool's min-processes replenishment.
    pub async fn on_ttl_expired(
        instance: &Arc<WorkerInstance>,
        pool: &WorkerPool,
    ) -> Result<TtlFloorDecision, WorkerError> {
        match pool.reserve_ttl_termination(instance) {
            TtlFloorDecision::NotIdle => Ok(TtlFloorDecision::NotIdle),
            TtlFloorDecision::Keep => {
                pool.record_ttl_kept(&instance.worker_ref);
                // The re-arm belongs to the FIRED timer task (it alone
                // carries the generation it claimed — rev2 P2 #1): after
                // the 1ms barrier it re-arms with its own generation. A
                // direct call (no fired timer task exists) records the
                // decision only: there is no claimed generation whose
                // window to re-arm.
                Ok(TtlFloorDecision::Keep)
            }
            TtlFloorDecision::Terminate => {
                // Already `Terminating` (the decision took the transition
                // atomically): run the cleanup + removal. Detach semantics
                // are the same as before — this runs inside the fired timer
                // task, so its own handle was already cleared by the timer.
                Self::cleanup(instance, pool, "ttl_expired").await?;
                pool.remove_instance(instance);
                Ok(TtlFloorDecision::Terminate)
            }
        }
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

    /// (EDG-10 review P2 #3 / rev2 P2 #1) Arm the TTL timer for a FRESH idle
    /// window (request completed). The `Idle` validation, the generation
    /// bump, the abort of the previous sleeping timer, the task spawn and
    /// the handle install are ONE critical section
    /// (`arm_and_install`): no firing can observe a half-armed slot.
    fn schedule_ttl_timer(instance: &Arc<WorkerInstance>, pool: &WorkerPool, ttl_ms: u64) {
        if ttl_ms == 0 {
            return;
        }
        let _ = instance.arm_and_install(TtlArm::New, |gen| {
            Self::spawn_timer_task(instance, pool, ttl_ms, gen)
        });
    }

    /// (EDG-10 review P2 #3 / rev2 P2 #1) The Keep path of a FIRED timer
    /// re-arms, carrying the task's OWN generation (the one it was armed
    /// with and claimed): the re-arm is valid only while `slot.gen` AND
    /// `slot.claimed` both still equal it — a newer task's claim (which
    /// overwrote `claimed`) or a request's fresh arm (which bumped
    /// `slot.gen`) drops the stale re-arm, and the validation + spawn +
    /// install happen in one critical section.
    fn rearm_keep_ttl_timer(
        instance: &Arc<WorkerInstance>,
        pool: &WorkerPool,
        ttl_ms: u64,
        task_gen: u64,
    ) {
        if ttl_ms == 0 {
            return;
        }
        let _ = instance.arm_and_install(TtlArm::KeepRearm(task_gen), |gen| {
            Self::spawn_timer_task(instance, pool, ttl_ms, gen)
        });
    }

    /// The TTL timer task body: sleep for the window, claim OWN generation
    /// before any further await, run the floor decision, and — on Keep —
    /// re-arm carrying that same generation.
    fn spawn_timer_task(
        instance: &Arc<WorkerInstance>,
        pool: &WorkerPool,
        ttl_ms: u64,
        gen: u64,
    ) -> tokio::task::JoinHandle<()> {
        let timer_instance = Arc::clone(instance);
        let pool = pool.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ttl_ms)).await;
            // (EDG-10 review P2 #3 / rev2 P2 #1) Claim our generation BEFORE
            // any further await: only the CURRENT timer acts. A stale task
            // (its window was superseded) does nothing — no decision, no
            // re-arm — and it never clears a newer timer's handle.
            if !timer_instance.claim_ttl_timer(gen) {
                return;
            }
            let Ok(decision) = Supervisor::on_ttl_expired(&timer_instance, &pool).await else {
                return;
            };
            if decision != TtlFloorDecision::Keep {
                return;
            }
            // Real preemption window (EDG-10 review P2 #3): the decision and
            // the re-arm install are separated by a 1ms timer sleep, so a
            // request that completes in the window ALWAYS runs to completion
            // before this (suspended) task can re-arm: it cancels the
            // (already claimed) timer and installs its own. A bare
            // `yield_now` would not be enough: the re-arm could win the race
            // to the first internal yield of the request.
            tokio::time::sleep(Duration::from_millis(1)).await;
            // Re-arm carrying THIS task's generation: the re-arm compares
            // `slot.gen` AND `slot.claimed` against it, so a newer task that
            // fired and claimed in the meantime owns the window — this stale
            // re-arm is dropped.
            Self::rearm_keep_ttl_timer(&timer_instance, &pool, ttl_ms, gen);
        })
    }
}
