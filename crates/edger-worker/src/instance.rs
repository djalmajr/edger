//! Worker instance with supervisor-managed lifecycle state.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use edger_core::{Isolate, WorkerRef};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::lru::WorkerGroup;
use crate::metrics::ActiveRequestMetrics;
use crate::state::WorkerState;

/// (EDG-10 review P2 #3 / rev2 P2 #1) How a TTL timer arm was requested.
/// See [`WorkerInstance::arm_and_install`] for the semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlArm {
    /// A fresh idle window (request completed): bump the generation and
    /// abort the previous sleeping timer.
    New,
    /// A fired timer's Keep decision re-arms, carrying the task's OWN
    /// generation (the one it was armed with): the re-arm is valid only
    /// while `slot.gen` AND `slot.claimed` both still equal that
    /// generation. `claimed` alone is shared state — a newer task's claim
    /// overwrites it, so comparing it to `slot.gen` without the task's own
    /// generation would let a stale task re-arm a window it no longer owns.
    KeepRearm(u64),
}

/// (EDG-10 review P2 #3) The single TTL timer slot of a
/// [`WorkerInstance`]: exactly one timer task is ever current.
#[derive(Default)]
struct TtlTimerSlot {
    /// Generation of the CURRENT timer (bumped by every arm and by every
    /// cancel). 0 means "no timer has ever been armed".
    gen: u64,
    /// The current timer task's handle (taken by the task itself at fire —
    /// it runs on that handle, so it must not be aborted mid-termination).
    handle: Option<tokio::task::JoinHandle<()>>,
    /// The generation a FIRED task claimed: its Keep re-arm is only valid
    /// while `slot.gen` AND `slot.claimed` both still equal the task's own
    /// generation (a request completing in the decision→install window
    /// bumps the generation and takes the slot with its own timer; a newer
    /// task's claim overwrites this field, which is exactly why the re-arm
    /// compares both against the task's own generation — rev2 P2 #1).
    claimed: Option<u64>,
}

/// A pooled worker with an injected isolate backend and lifecycle state.
pub struct WorkerInstance {
    pub worker_ref: WorkerRef,
    id: Uuid,
    created_at: Instant,
    dispatch_lock: Arc<AsyncMutex<()>>,
    isolate: Arc<AsyncMutex<Box<dyn Isolate>>>,
    state: Mutex<WorkerState>,
    request_count: Mutex<u32>,
    active_request: Mutex<Option<ActiveRequestState>>,
    unhealthy: AtomicBool,
    idle_notifications: AtomicU32,
    /// (EDG-10 review P2 #3) The TTL timer slot: the generation of the
    /// CURRENT timer and its handle. Every arm bumps the generation and
    /// aborts the previous task; `cancel_ttl_timer` bumps it too. A fired
    /// task may only act while the generation it armed with is still
    /// current (`claim_ttl_timer`), and only its own generation's handle is
    /// ever cleared — a re-armed or request-scheduled timer invalidates the
    /// older task instead of being overwritten by it.
    ttl_timer: Mutex<TtlTimerSlot>,
    /// The group this instance belongs to (EDG-10). `Weak` so the instance
    /// never keeps the group (its queue and max-process budget) alive outside
    /// the LRU; the TTL-floor decision and the min-processes replenishment
    /// use it to see the eviction/closed state of the instance's OWN group
    /// (a re-admitted generation is a different group, not a floor source).
    group: Mutex<Option<Weak<WorkerGroup>>>,
}

struct ActiveRequestState {
    request_id: String,
    started: Instant,
    streaming: bool,
}

impl WorkerInstance {
    pub fn new(worker_ref: WorkerRef, isolate: Box<dyn Isolate>) -> Self {
        Self {
            worker_ref,
            id: Uuid::new_v4(),
            created_at: Instant::now(),
            dispatch_lock: Arc::new(AsyncMutex::new(())),
            isolate: Arc::new(AsyncMutex::new(isolate)),
            state: Mutex::new(WorkerState::Creating),
            request_count: Mutex::new(0),
            active_request: Mutex::new(None),
            unhealthy: AtomicBool::new(false),
            idle_notifications: AtomicU32::new(0),
            ttl_timer: Mutex::new(TtlTimerSlot {
                gen: 0,
                handle: None,
                claimed: None,
            }),
            group: Mutex::new(None),
        }
    }

    /// Attach the owning group. Called by the pool right after the instance
    /// is created for a group (before or as it is pushed into the group's
    /// set) so the TTL decision and replenishment can resolve the group this
    /// instance actually belongs to (EDG-10).
    pub fn attach_group(&self, group: &Arc<WorkerGroup>) {
        *self.group.lock().expect("group lock") = Some(Arc::downgrade(group));
    }

    /// Upgrade to the owning group while it is still alive.
    pub fn own_group(&self) -> Option<Arc<WorkerGroup>> {
        self.group
            .lock()
            .expect("group lock")
            .as_ref()
            .and_then(Weak::upgrade)
    }

    pub fn id(&self) -> Uuid {
        self.id
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
        if state != WorkerState::Active {
            self.clear_active_request();
        }
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

    pub fn start_active_request(&self, request_id: String) {
        *self.active_request.lock().expect("active request lock") = Some(ActiveRequestState {
            request_id,
            started: Instant::now(),
            streaming: false,
        });
    }

    pub fn mark_active_request_streaming(&self) {
        if let Some(request) = self
            .active_request
            .lock()
            .expect("active request lock")
            .as_mut()
        {
            request.streaming = true;
        }
    }

    pub fn clear_active_request(&self) {
        self.active_request
            .lock()
            .expect("active request lock")
            .take();
    }

    pub fn active_request_metrics(&self) -> Option<ActiveRequestMetrics> {
        self.active_request
            .lock()
            .expect("active request lock")
            .as_ref()
            .map(|request| ActiveRequestMetrics {
                request_id: request.request_id.clone(),
                age_ms: request.started.elapsed().as_millis() as u64,
                streaming: request.streaming,
            })
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

    pub fn cancel_ttl_timer(&self) {
        let mut slot = self.ttl_timer.lock().expect("ttl timer lock");
        if let Some(handle) = slot.handle.take() {
            handle.abort();
        }
        // Invalidate the current generation too: a task that already fired
        // (claimed, handle taken) must not re-arm after this cancel.
        slot.gen = slot.gen.wrapping_add(1);
    }

    /// (EDG-10 review P2 #3) The fired timer task claims its generation:
    /// returns `true` only while this task's timer is still the CURRENT one
    /// (no newer timer was installed and no cancel bumped the generation),
    /// and clears exactly this task's own handle. A stale task (its window
    /// was superseded by a re-arm or a request's timer) gets `false` and
    /// must do nothing — no decision, no re-arm, and it never touches a
    /// newer timer's handle.
    pub fn claim_ttl_timer(&self, gen: u64) -> bool {
        let mut slot = self.ttl_timer.lock().expect("ttl timer lock");
        // (rev2 P2 #1) The fire claim compares `slot.gen` AND `slot.claimed`
        // with THIS task's generation: a claim made by another task (its
        // generation outstanding) never passes, even if a later arm moved
        // `slot.gen` back onto a value equal to this task's own.
        if slot.gen != gen || matches!(slot.claimed, Some(other) if other != gen) {
            return false;
        }
        slot.handle.take();
        slot.claimed = Some(gen);
        true
    }

    /// (EDG-10 review P2 #3 / rev2 P2 #1) Arm a TTL timer and install its
    /// handle in ONE critical section: the `Idle` validation, the generation
    /// bump, the abort of the previous sleeping timer, the task SPAWN and
    /// the handle install all run while the state lock and the slot lock
    /// are held, so a firing can never observe a half-armed slot and a
    /// racing cancel cannot wedge the handoff between validation and
    /// installation (the spawned task is dropped by the abort and its
    /// fire-claim will fail — it is past no `sleep` boundary it could act
    /// across).
    ///
    /// * `TtlArm::New` — a request just completed (or a fresh idle window
    ///   started): bump the generation and abort any previous SLEEPING timer
    ///   task (a task that already fired claimed its generation and owns its
    ///   own path; aborting it here cannot wedge it).
    /// * `TtlArm::KeepRearm(task_gen)` — a fired timer's Keep decision
    ///   re-arms, carrying the task's OWN generation: allowed ONLY while
    ///   `slot.gen` AND `slot.claimed` both still equal `task_gen` (set by
    ///   `claim_ttl_timer`). A request that completed in the
    ///   decision→install window already bumped the generation and installed
    ///   its own timer, which owns the window; a newer task's claim
    ///   overwrote `claimed`; either way the stale re-arm is dropped (its
    ///   task is abandoned, already past its last `sleep` and holding no
    ///   handle).
    ///
    /// `spawn` receives the generation the new timer task must claim at
    /// fire and returns its handle. Returns that generation, or `None` when
    /// nothing was armed.
    pub fn arm_and_install<F>(&self, mode: TtlArm, spawn: F) -> Option<u64>
    where
        F: FnOnce(u64) -> tokio::task::JoinHandle<()>,
    {
        let state = self.state_lock();
        if *state != WorkerState::Idle {
            return None;
        }
        let mut slot = self.ttl_timer.lock().expect("ttl timer lock");
        let gen = match mode {
            TtlArm::New => {
                let gen = slot.gen.wrapping_add(1);
                slot.gen = gen;
                slot.claimed = None;
                if let Some(old) = slot.handle.take() {
                    old.abort();
                }
                gen
            }
            TtlArm::KeepRearm(task_gen) => {
                if slot.gen != task_gen || slot.claimed != Some(task_gen) {
                    return None;
                }
                let gen = slot.gen.wrapping_add(1);
                slot.gen = gen;
                slot.claimed = None;
                gen
            }
        };
        let handle = spawn(gen);
        slot.handle = Some(handle);
        Some(gen)
    }

    /// The generation of the currently installed TTL timer (the claimed one
    /// for a fired task still running its decision; 0 when none).
    pub fn ttl_timer_generation(&self) -> u64 {
        self.ttl_timer.lock().expect("ttl timer lock").gen
    }
}
