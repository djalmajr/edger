//! Native cron scheduler for manifest `cron[]` jobs.
//!
//! (EDG-26) The scheduler no longer holds a fixed registration list built
//! once at boot. A supervisor task reconciles the desired set of jobs
//! against the `ManifestIndex` every `reconcile_interval`: for each worker
//! name only the version the unversioned route `/<name>/` resolves to — the
//! promoted default pointer, or the highest enabled, non-staged semver when
//! there is no pointer, through the same `resolve_worker` the pipeline
//! uses — contributes its `cron` entries. Rollback and staged versions never
//! contribute.
//!
//! Jobs are keyed by (worker name, schedule, path, method) WITHOUT the
//! version, so a promote to a version with the same `cron` entries does not
//! restart the running task. A tick dispatches the unversioned route
//! `/<name>/<path>` through the pipeline, resolved at tick time, so every
//! dispatch follows the current default version and the job keeps running
//! across promotes.
//!
//! All job-task state is owned by the supervisor task alone (single
//! owner): the `tokio::task::JoinSet` of job tasks plus, per job key, a
//! LIST of live generations (stop sender, done receiver, generation id).
//! There is no lock shared with `CronScheduler::shutdown`, so
//! reconciliation, reaping and shutdown all happen inside the supervisor
//! loop, in sequence, and no handle is ever transferred between owners.
//! If the supervisor is aborted or dropped, the `JoinSet` drop repeats
//! the abort request on the child tasks (a task that does not yield to
//! the cancellation keeps running until it finishes on its own).
//!
//! Removed jobs (disable, delete, promote) are DRAINED, not aborted: the
//! newest generation of the key is signaled — it stops scheduling — and an
//! in-flight dispatch keeps running to completion. Repeated removals only
//! append to the key's generation list; the reaping removes the finished
//! generations. Reconciliation never waits on a removed task (it cannot
//! block the other jobs). A task created for a key that still has live
//! generations waits for ALL of them to end before its first dispatch, so
//! a job key never has more than one dispatch in flight, even across
//! repeated removals and re-adds. `active_job_count` and
//! `draining_job_count` read atomics published by the supervisor. Only a
//! global `shutdown` may force-stop a task: the supervisor stops admitting
//! jobs, signals every generation (an in-flight dispatch is never aborted
//! mid-flight), lets the in-flight dispatches finish until the grace
//! deadline (config `with_shutdown_grace`, default 5 s), then aborts the
//! stragglers and waits for their end — confirmed through the supervisor's
//! own `JoinSet` — until the abort-wait deadline (config
//! `with_shutdown_abort_wait`, default 1 s). The counters change only
//! through the actual reaping (never `store(0)`): after `shutdown`
//! returns, `active_job_count() + draining_job_count()` is the number of
//! tasks still in the `JoinSet`, and a task that does not yield by the
//! deadlines is reported (`cron shutdown incomplete` warn with the
//! remaining count) instead of being declared stopped.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use chrono::{DateTime, Utc};
use edger_core::{
    parse_duration_string_to_ms, CoreError, CronJob, WorkerRef, INTERNAL_REQUEST_HEADER,
};
use saffron::Cron;
use tokio::sync::{oneshot, watch};
use tokio::task::{Id, JoinHandle, JoinSet};
use tower::ServiceExt;
use uuid::Uuid;

use crate::manifest_index_stub::ManifestIndex;

const DEFAULT_METHOD: &str = "GET";
/// How long a shutdown lets in-flight dispatches finish before aborting
/// the stragglers (config `with_shutdown_grace`).
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// How long a shutdown waits for the aborted tasks to confirm their end
/// through the `JoinSet` (config `with_shutdown_abort_wait`).
const DEFAULT_SHUTDOWN_ABORT_WAIT: Duration = Duration::from_secs(1);
/// How much longer `CronScheduler::shutdown` waits for the supervisor than
/// the supervisor's internal shutdown budget (grace + abort-wait) before
/// aborting it (the supervisor's `JoinSet` drop then repeats the abort
/// request on the job tasks).
const SUPERVISOR_SHUTDOWN_MARGIN: Duration = Duration::from_secs(1);
const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default)]
pub struct CronMetrics {
    inner: Arc<CronMetricsInner>,
}

#[derive(Debug, Default)]
struct CronMetricsInner {
    executions_total: AtomicU64,
    failures_total: AtomicU64,
}

impl CronMetrics {
    pub fn record_execution(&self) {
        self.inner.executions_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        self.inner.failures_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn executions_total(&self) -> u64 {
        self.inner.executions_total.load(Ordering::Relaxed)
    }

    pub fn failures_total(&self) -> u64 {
        self.inner.failures_total.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug)]
pub struct CronSchedulerConfig {
    root_api_key: Option<String>,
    reconcile_interval: Duration,
    shutdown_grace: Duration,
    shutdown_abort_wait: Duration,
}

impl CronSchedulerConfig {
    pub fn new(root_api_key: Option<String>) -> Self {
        Self {
            root_api_key,
            reconcile_interval: DEFAULT_RECONCILE_INTERVAL,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            shutdown_abort_wait: DEFAULT_SHUTDOWN_ABORT_WAIT,
        }
    }

    /// Cadence of the index reconciliation (default 5 s).
    pub fn with_reconcile_interval(mut self, reconcile_interval: Duration) -> Self {
        self.reconcile_interval = reconcile_interval;
        self
    }

    /// How long a shutdown lets in-flight dispatches finish before aborting
    /// the stragglers (default 5 s).
    pub fn with_shutdown_grace(mut self, shutdown_grace: Duration) -> Self {
        self.shutdown_grace = shutdown_grace;
        self
    }

    /// How long a shutdown waits for the aborted tasks to confirm their end
    /// through the `JoinSet` (default 1 s).
    pub fn with_shutdown_abort_wait(mut self, shutdown_abort_wait: Duration) -> Self {
        self.shutdown_abort_wait = shutdown_abort_wait;
        self
    }
}

/// Identity of a reconciled cron job. The version is deliberately absent:
/// the unversioned route decides the serving version at tick time, so a
/// promote with an unchanged `cron` entry set keeps the running task.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CronJobKey {
    worker: String,
    schedule: String,
    path: String,
    method: String,
}

#[derive(Clone, Debug)]
enum CronSchedule {
    Every(Duration),
    WallClock { expression: Cron, source: String },
}

impl CronSchedule {
    fn parse(schedule: &str) -> Result<Self, CoreError> {
        let schedule = schedule.trim();
        if let Some(duration) = schedule.strip_prefix("@every ") {
            let Some(ms) = parse_duration_string_to_ms(duration) else {
                return Err(invalid_schedule(schedule));
            };
            if ms == 0 {
                return Err(invalid_schedule(schedule));
            }
            return Ok(Self::Every(Duration::from_millis(ms)));
        }

        let normalized = normalize_standard_cron(schedule)?;
        let expression = normalized
            .parse::<Cron>()
            .map_err(|_| invalid_schedule(schedule))?;
        if !expression.any() || expression.next_after(Utc::now()).is_none() {
            return Err(invalid_schedule(schedule));
        }

        Ok(Self::WallClock {
            expression,
            source: schedule.to_owned(),
        })
    }

    fn delay_after(&self, now: DateTime<Utc>) -> Result<Duration, CoreError> {
        match self {
            Self::Every(interval) => Ok(*interval),
            Self::WallClock { .. } => self
                .next_fire_after(now)?
                .signed_duration_since(now)
                .to_std()
                .map_err(|_| CoreError::new("CRON_SCHEDULE_INVALID", "cron fired in the past")),
        }
    }

    fn next_fire_after(&self, now: DateTime<Utc>) -> Result<DateTime<Utc>, CoreError> {
        match self {
            Self::Every(interval) => {
                let delta = chrono::Duration::from_std(*interval).map_err(|_| {
                    CoreError::new("CRON_SCHEDULE_INVALID", "cron interval is too large")
                })?;
                Ok(now + delta)
            }
            Self::WallClock { expression, source } => expression
                .next_after(now)
                .ok_or_else(|| invalid_schedule(source)),
        }
    }
}

struct DesiredJob {
    schedule: CronSchedule,
}

#[derive(Clone)]
struct CronJobTask {
    key: CronJobKey,
    schedule: CronSchedule,
}

/// One live generation of a job key: one spawned task. A key keeps a LIST
/// of live generations (never a single overwritten handle): removals only
/// append (a removed key's newest generation stops and drains), re-adding
/// appends the new generation, and the reaping removes the finished ones.
struct Generation {
    /// Generation id (monotonic, supervisor-local), returned by the task
    /// so a reaped task can be matched to its entry.
    id: u64,
    /// Signals the task to stop scheduling. A stop never aborts an
    /// in-flight dispatch; it is observed at the task's select boundary.
    stop_tx: watch::Sender<bool>,
    /// Set to `true` when the task's future ends, however it ends (the
    /// `MarkDone` guard in `run_job` sends it on drop, including abort).
    /// A task created for the same key while this generation is still live
    /// awaits this signal before its first dispatch (identity exclusion).
    done_rx: watch::Receiver<bool>,
    /// Whether the supervisor already signaled a stop.
    stopped: bool,
}

/// Sends the job's "done" signal when the task's future is dropped, i.e.
/// on every exit path, including abort.
struct MarkDone(watch::Sender<bool>);

impl Drop for MarkDone {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

/// The result of joining one job task from the supervisor's `JoinSet`: the
/// task id + the (key, generation id) output, or a `JoinError` (whose id
/// identifies the `task_ids` entry to drop).
type ReapOutcome = Result<(Id, (CronJobKey, u64)), tokio::task::JoinError>;

/// All job-task state, owned by the supervisor task alone. Reconciliation,
/// reaping and shutdown all run inside the supervisor loop (in sequence),
/// so no state is shared with `CronScheduler::shutdown` and no handle is
/// ever transferred between owners. If the supervisor is dropped or
/// aborted, the `JoinSet` drop aborts the child tasks: no task is left
/// detached.
struct JobState {
    /// Every job task, born here; drop/`shutdown` aborts and joins them.
    jobs: JoinSet<(CronJobKey, u64)>,
    /// Live generations per key, in spawn order (last = newest).
    generations: HashMap<CronJobKey, Vec<Generation>>,
    /// Task id → (key, generation id), for every spawned task: the entry
    /// is removed when the task is reaped, on success and on failure
    /// (the `Err` result does not carry the output, so the mapping names
    /// the generation to drop there).
    task_ids: HashMap<Id, (CronJobKey, u64)>,
    next_id: u64,
    /// Keys skipped because their schedule is invalid (counted once each).
    invalid: BTreeSet<CronJobKey>,
    /// The missing-ROOT_API_KEY warning fires once, when the desired set
    /// leaves empty.
    root_key_warned: bool,
    active: Arc<AtomicUsize>,
    draining: Arc<AtomicUsize>,
}

impl JobState {
    fn new(active: Arc<AtomicUsize>, draining: Arc<AtomicUsize>) -> Self {
        Self {
            jobs: JoinSet::new(),
            generations: HashMap::new(),
            task_ids: HashMap::new(),
            next_id: 0,
            invalid: BTreeSet::new(),
            root_key_warned: false,
            active,
            draining,
        }
    }

    /// Remove the finished generation (and update the counters).
    fn remove_generation(&mut self, key: &CronJobKey, id: u64) {
        if let Some(gens) = self.generations.get_mut(key) {
            if let Some(pos) = gens.iter().position(|gen| gen.id == id) {
                let stopped = gens.remove(pos).stopped;
                if gens.is_empty() {
                    self.generations.remove(key);
                }
                if stopped {
                    self.draining.fetch_sub(1, Ordering::Relaxed);
                } else {
                    self.active.fetch_sub(1, Ordering::Relaxed);
                }
                tracing::debug!(
                    worker = %key.worker,
                    schedule = %key.schedule,
                    path = %key.path,
                    stopped,
                    "cron job drained"
                );
            }
        }
    }

    /// Handle one reaped task (a `JoinSet` result): remove its `task_ids`
    /// entry — on success AND on failure: a task that finished normally
    /// must not retain its mapping — and drop its generation entry, which
    /// updates the counters.
    fn reap_result(&mut self, result: ReapOutcome) {
        let id = match &result {
            Ok((id, _)) => *id,
            Err(err) => err.id(),
        };
        let mapped = self.task_ids.remove(&id);
        match result {
            Ok((_, (key, gen_id))) => {
                self.remove_generation(&key, gen_id);
            }
            Err(err) => {
                // Aborted or panicked: the `MarkDone` guard still fired, so
                // identity exclusion is safe; the mapping (if still
                // present) names the generation to drop.
                if let Some((key, gen_id)) = mapped {
                    self.remove_generation(&key, gen_id);
                }
                if err.is_panic() {
                    tracing::warn!("cron job task panicked: {err}");
                }
            }
        }
    }

    /// Reap every task that already finished (non-blocking).
    fn reap_finished(&mut self) {
        while let Some(result) = self.jobs.try_join_next_with_id() {
            self.reap_result(result);
        }
    }

    /// Reap every generation whose task already ended, as reported by the
    /// `done` channel: a safety net that keeps the generation list and the
    /// counters in sync with an end the `JoinSet` has not reported yet (a
    /// task that ends between polls). The `MarkDone` guard sets it on
    /// every exit (including abort); it is also the inter-generation
    /// exclusion signal (a successor waits for its predecessors). During
    /// shutdown the `JoinSet` is the completion confirmation, not this
    /// channel, so this pass is not run there.
    fn reap_by_done(&mut self) {
        let mut done: Vec<(CronJobKey, u64)> = Vec::new();
        for (key, gens) in self.generations.iter() {
            for gen in gens {
                if *gen.done_rx.borrow() {
                    done.push((key.clone(), gen.id));
                }
            }
        }
        for (key, id) in done {
            self.remove_generation(&key, id);
        }
    }

    /// Total number of live generation entries (the `JoinSet` may also
    /// hold finished-but-unpopped tasks, so it is NOT the source of truth
    /// for liveness).
    fn live_gen_count(&self) -> usize {
        self.generations.values().map(Vec::len).sum()
    }

    /// Stop the generations of the keys that left the desired set: signal
    /// each one (it stops scheduling; an in-flight dispatch keeps running)
    /// WITHOUT waiting — reconciliation can never be blocked by a removed
    /// task.
    fn stop_removed(&mut self, desired: &BTreeMap<CronJobKey, DesiredJob>) {
        for (key, gens) in self.generations.iter_mut() {
            if desired.contains_key(key) {
                continue;
            }
            for gen in gens.iter_mut() {
                if gen.stopped {
                    continue;
                }
                gen.stopped = true;
                let _ = gen.stop_tx.send(true);
                self.active.fetch_sub(1, Ordering::Relaxed);
                self.draining.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    worker = %key.worker,
                    schedule = %key.schedule,
                    path = %key.path,
                    "cron job stopped"
                );
            }
        }
    }

    /// Start the generations for the desired keys that have none active.
    /// A new generation waits for every still-live generation of the same
    /// key (they are all stopped, so each ends once its in-flight dispatch
    /// finishes) before its first dispatch — the key never has two
    /// dispatches in flight, even across repeated removals and re-adds.
    fn start_missing(
        &mut self,
        desired: &BTreeMap<CronJobKey, DesiredJob>,
        config: &CronSchedulerConfig,
        index: &ManifestIndex,
        app: &Router,
        metrics: &CronMetrics,
    ) {
        for (key, job) in desired {
            let has_active = self
                .generations
                .get(key)
                .is_some_and(|gens| gens.iter().any(|gen| !gen.stopped));
            if has_active {
                continue;
            }
            let predecessors: Vec<watch::Receiver<bool>> = self
                .generations
                .get(key)
                .map(|gens| gens.iter().map(|gen| gen.done_rx.clone()).collect())
                .unwrap_or_default();
            let (stop_tx, stop_rx) = watch::channel(false);
            let (done_tx, done_rx) = watch::channel(false);
            let id = self.next_id;
            self.next_id += 1;
            let handle = self.jobs.spawn(run_job(
                CronJobTask {
                    key: key.clone(),
                    schedule: job.schedule.clone(),
                },
                app.clone(),
                index.clone(),
                config.root_api_key.clone(),
                metrics.clone(),
                id,
                stop_rx,
                done_tx,
                predecessors,
            ));
            self.task_ids.insert(handle.id(), (key.clone(), id));
            self.generations
                .entry(key.clone())
                .or_default()
                .push(Generation {
                    id,
                    stop_tx,
                    done_rx,
                    stopped: false,
                });
            self.active.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                worker = %key.worker,
                schedule = %key.schedule,
                path = %key.path,
                method = %key.method,
                "cron job started"
            );
        }
    }
}

pub struct CronScheduler {
    shutdown_tx: watch::Sender<bool>,
    supervisor: Option<JoinHandle<()>>,
    /// Cloned before the config is moved into the supervisor: `shutdown`
    /// derives its external wait (grace + abort-wait + margin) from it.
    config: CronSchedulerConfig,
    /// Published by the supervisor (single owner of the job tasks); read
    /// here without touching the internal state.
    active: Arc<AtomicUsize>,
    draining: Arc<AtomicUsize>,
}

impl CronScheduler {
    /// Start the scheduler. The first reconciliation runs (inside the
    /// supervisor) before returning, so boot-time workers already have
    /// their jobs running; the supervisor then re-reconciles from the index
    /// every `reconcile_interval`, adding jobs for newly installed/enabled
    /// workers and stopping the ones that left the desired set.
    pub async fn start(
        config: CronSchedulerConfig,
        index: ManifestIndex,
        app: Router,
        metrics: CronMetrics,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        let active = Arc::new(AtomicUsize::new(0));
        let draining = Arc::new(AtomicUsize::new(0));
        let supervisor = tokio::spawn(supervise(
            config.clone(),
            index,
            app,
            metrics,
            shutdown_rx,
            ready_tx,
            active.clone(),
            draining.clone(),
        ));
        // The supervisor does the first reconciliation before signaling
        // ready, so `start` keeps returning only with the boot jobs running.
        let _ = ready_rx.await;
        Self {
            shutdown_tx,
            supervisor: Some(supervisor),
            config,
            active,
            draining,
        }
    }

    /// Number of jobs actively scheduling dispatches. Removed jobs that
    /// are still draining an in-flight dispatch are not counted (see
    /// [`CronScheduler::draining_job_count`]).
    pub fn active_job_count(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    /// Number of removed jobs whose task is still draining (an in-flight
    /// dispatch may still be running).
    pub fn draining_job_count(&self) -> usize {
        self.draining.load(Ordering::Relaxed)
    }

    /// Stop the scheduler. Sends the shutdown signal to the supervisor and
    /// waits for it (grace + abort-wait + margin, from the config). On the
    /// signal the supervisor stops admitting jobs, signals the stop of
    /// every generation (an in-flight dispatch is never aborted mid-flight),
    /// lets them finish until the grace deadline, then aborts the rest and
    /// waits for their end until the abort-wait deadline — the `JoinSet`
    /// reaping is the completion confirmation and the published counters
    /// change only through the actual reaping. If the budgets run out with
    /// tasks still pending the supervisor reports `cron shutdown
    /// incomplete` (the counters keep the remaining count) instead of
    /// declaring zero. If the supervisor itself passes the external limit
    /// it is aborted and joined, and the counters are left as the
    /// supervisor last published them.
    pub async fn shutdown(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(mut supervisor) = self.supervisor.take() {
            let limit = self.config.shutdown_grace
                + self.config.shutdown_abort_wait
                + SUPERVISOR_SHUTDOWN_MARGIN;
            tokio::select! {
                _ = &mut supervisor => {}
                _ = tokio::time::sleep(limit) => {
                    tracing::warn!("cron supervisor did not stop");
                    supervisor.abort();
                    let _ = supervisor.await;
                }
            }
        }
    }
}

/// The supervisor: single owner of all job-task state. The first
/// reconciliation runs before the ready signal (boot workers), then the
/// loop reaps the finished tasks (non-blocking), re-reconciles on the
/// (absolute) deadline and waits for the next finished task, the next
/// deadline or the shutdown signal. Reconciliation, reaping and shutdown
/// all run here, in sequence, with no shared lock — and the whole pass is
/// synchronous, so a shutdown signal is only ever observed at the select
/// boundary, never in the middle of a transfer.
#[allow(clippy::too_many_arguments)]
async fn supervise(
    config: CronSchedulerConfig,
    index: ManifestIndex,
    app: Router,
    metrics: CronMetrics,
    mut shutdown_rx: watch::Receiver<bool>,
    ready_tx: oneshot::Sender<()>,
    active: Arc<AtomicUsize>,
    draining: Arc<AtomicUsize>,
) {
    let mut state = JobState::new(active, draining);

    reconcile_pass(&mut state, &config, &index, &app, &metrics);
    let _ = ready_tx.send(());

    let mut next_reconcile = tokio::time::Instant::now() + config.reconcile_interval;
    loop {
        state.reap_finished();
        state.reap_by_done();
        // The reconcile deadline is absolute: a wake for a finished task
        // never starves the periodic pass.
        if tokio::time::Instant::now() >= next_reconcile {
            reconcile_pass(&mut state, &config, &index, &app, &metrics);
            next_reconcile = tokio::time::Instant::now() + config.reconcile_interval;
        }
        if state.live_gen_count() == 0 {
            // No live generations: wait for the next reconcile (or the
            // shutdown signal) instead of spinning on the `JoinSet`.
            tokio::select! {
                _ = shutdown_rx.changed() => break,
                _ = tokio::time::sleep_until(next_reconcile) => {}
            }
            continue;
        }
        // Wait for a finished task, the next deadline or the shutdown
        // signal; a finished task is reaped HERE (the `join_next` result is
        // the only place its (key, generation id) is observable). The
        // `reap_by_done` pass keeps the counters correct even for a
        // completion the `JoinSet` does not report promptly.
        let woken = tokio::select! {
            _ = shutdown_rx.changed() => SupervisorWake::Shutdown,
            result = state.jobs.join_next_with_id() => SupervisorWake::Reaped(result),
            _ = tokio::time::sleep_until(next_reconcile) => SupervisorWake::Deadline,
        };
        match woken {
            SupervisorWake::Shutdown => break,
            SupervisorWake::Reaped(result) => {
                if let Some(result) = result {
                    state.reap_result(result);
                }
            }
            SupervisorWake::Deadline => {}
        }
    }

    // Shutdown: no new admissions, signal every generation, let the
    // in-flight dispatches finish until the grace deadline, then abort the
    // stragglers and wait for their end — through the `JoinSet` — until the
    // abort-wait deadline; a task that does not yield is reported (`cron
    // shutdown incomplete`) instead of being declared stopped.
    shutdown_pass(&mut state, &config).await;
}

/// Which event woke the supervisor loop.
enum SupervisorWake {
    Shutdown,
    Reaped(Option<ReapOutcome>),
    Deadline,
}

/// One reconciliation pass (synchronous): compute the desired set from the
/// index, reap the finished tasks, stop the generations that left the set
/// (signaling them WITHOUT waiting — an in-flight dispatch keeps running,
/// so a slow dispatch can never block the other jobs) and start the
/// generations that entered it (a new generation waits for every still-live
/// generation of the same key before its first dispatch, so a job key never
/// has two dispatches in flight, even across repeated removals and re-adds).
fn reconcile_pass(
    state: &mut JobState,
    config: &CronSchedulerConfig,
    index: &ManifestIndex,
    app: &Router,
    metrics: &CronMetrics,
) {
    let desired = scan_desired_jobs(index, metrics, &mut state.invalid);

    if !desired.is_empty() && config.root_api_key.is_none() && !state.root_key_warned {
        state.root_key_warned = true;
        tracing::warn!(
            "manifest cron jobs are enabled without ROOT_API_KEY or EDGER_ROOT_KEY_FILE; jobs will dispatch without Authorization and x-edger-internal will be discarded by the pipeline"
        );
    }

    state.reap_finished();
    state.stop_removed(&desired);
    state.start_missing(&desired, config, index, app, metrics);
}

/// The shutdown phase (runs inside the supervisor, after the loop):
/// signal every generation — a stop never aborts an in-flight dispatch —
/// let them finish until the grace deadline, then abort the stragglers and
/// wait for their end until the abort-wait deadline. The `JoinSet` is the
/// completion confirmation: the counters change only through the actual
/// reaping (no `store(0)`), so after this function returns
/// `active + draining` is the number of tasks still in the `JoinSet`; if
/// the abort-wait deadline runs out with tasks still pending, the shortfall
/// is reported as a warning (`cron shutdown incomplete` with the remaining
/// count) instead of being declared zero, and the `JoinSet` drop (when the
/// supervisor returns) repeats the abort request on whatever is left. The
/// `done` channel is NOT used here as proof of completion — it only feeds
/// the inter-generation exclusion (a successor waits for its predecessors).
async fn shutdown_pass(state: &mut JobState, config: &CronSchedulerConfig) {
    for gens in state.generations.values_mut() {
        for gen in gens.iter_mut() {
            if gen.stopped {
                continue;
            }
            gen.stopped = true;
            let _ = gen.stop_tx.send(true);
            state.active.fetch_sub(1, Ordering::Relaxed);
            state.draining.fetch_add(1, Ordering::Relaxed);
        }
    }

    // Let the tasks finish until the grace deadline, reaping as we go.
    let grace_deadline = tokio::time::Instant::now() + config.shutdown_grace;
    while !state.jobs.is_empty() && tokio::time::Instant::now() < grace_deadline {
        tokio::select! {
            result = state.jobs.join_next_with_id() => {
                if let Some(result) = result {
                    state.reap_result(result);
                }
            }
            _ = tokio::time::sleep_until(grace_deadline) => {}
        }
        state.reap_finished();
    }

    // Abort the tasks that passed the grace deadline and wait for their end
    // until the abort-wait deadline. The `JoinSet` entry completes only
    // when the task actually ends, so a poll that does not yield to the
    // cancellation keeps its entry (and its counter) here until it
    // finishes.
    state.jobs.abort_all();
    let abort_deadline = tokio::time::Instant::now() + config.shutdown_abort_wait;
    while !state.jobs.is_empty() && tokio::time::Instant::now() < abort_deadline {
        tokio::select! {
            result = state.jobs.join_next_with_id() => {
                if let Some(result) = result {
                    state.reap_result(result);
                }
            }
            _ = tokio::time::sleep_until(abort_deadline) => {}
        }
    }

    state.reap_finished();
    let remaining = state.jobs.len();
    if remaining > 0 {
        tracing::warn!(remaining, "cron shutdown incomplete");
    }
}

fn scan_desired_jobs(
    index: &ManifestIndex,
    metrics: &CronMetrics,
    invalid: &mut BTreeSet<CronJobKey>,
) -> BTreeMap<CronJobKey, DesiredJob> {
    let names: BTreeSet<String> = index
        .worker_refs()
        .into_iter()
        .map(|worker| worker.name)
        .collect();
    let mut desired = BTreeMap::new();
    let mut seen = BTreeSet::new();

    for name in &names {
        // The same resolver the pipeline uses for the unversioned route:
        // the promoted default pointer when it still points to an enabled,
        // non-staged version, otherwise the highest enabled, non-staged
        // semver. Rollback and staged versions never resolve here.
        let Ok(worker) = index.resolve_worker(name, None) else {
            continue;
        };
        if !worker.config.enabled || worker.config.cron.is_empty() {
            continue;
        }
        for job in &worker.config.cron {
            let method = job.method.as_deref().unwrap_or(DEFAULT_METHOD);
            let key = CronJobKey {
                worker: name.clone(),
                schedule: job.schedule.clone(),
                path: job.path.clone(),
                method: method.to_owned(),
            };
            seen.insert(key.clone());
            if let Err(err) = validate_job(&worker, job) {
                note_invalid(&key, &worker.name, &job.path, err, metrics, invalid);
                continue;
            }
            match CronSchedule::parse(&job.schedule) {
                Ok(schedule) => {
                    desired.insert(key, DesiredJob { schedule });
                }
                Err(err) => note_invalid(&key, &worker.name, &job.path, err, metrics, invalid),
            }
        }
    }

    // A suppressed key is released when the job leaves the index (fixed or
    // deleted) so a later re-appearance warns again.
    invalid.retain(|key| seen.contains(key));
    desired
}

fn note_invalid(
    key: &CronJobKey,
    worker: &str,
    path: &str,
    err: CoreError,
    metrics: &CronMetrics,
    invalid: &mut BTreeSet<CronJobKey>,
) {
    if invalid.insert(key.clone()) {
        metrics.record_failure();
        tracing::warn!(
            worker = %worker,
            path = %path,
            code = %err.code,
            "cron schedule is invalid; the job is skipped until the manifest changes: {}",
            err.message
        );
    }
}

/// One tick task per job. Sequential by design (EDG-26): the dispatch is
/// awaited inside the `sleep` branch, so the next delay is only computed
/// after the current dispatch finished — a job never has more than one
/// dispatch in flight, even when a dispatch takes longer than the schedule
/// interval. A stop (reconcile removed the job, or the shutdown) is
/// observed only at the select boundary: a dispatch already in flight
/// finishes normally and is never aborted mid-flight; only the next one is
/// not scheduled.
#[allow(clippy::too_many_arguments)]
async fn run_job(
    task: CronJobTask,
    app: Router,
    index: ManifestIndex,
    root_api_key: Option<String>,
    metrics: CronMetrics,
    gen_id: u64,
    mut job_stop_rx: watch::Receiver<bool>,
    done_tx: watch::Sender<bool>,
    predecessors: Vec<watch::Receiver<bool>>,
) -> (CronJobKey, u64) {
    // Signals the job's "done" watch whenever the task ends, however it
    // ends (the guard is dropped with the future, including on abort).
    let _done = MarkDone(done_tx);

    // Identity exclusion: every still-live generation of the same key may
    // be draining an in-flight dispatch. Wait for all of them to end
    // before the first dispatch — a generation that exits early (stopped
    // before its first dispatch) is harmless, because this task waits for
    // the oldest (in-flight) one directly. A stop of this generation wins
    // immediately at any wait. A clone may already carry the done value
    // (the predecessor finished between the reconciliation's reap and this
    // wait), so check it first: a clone that has already seen the value
    // would wait for a change that never comes.
    for mut predecessor in predecessors {
        if !*predecessor.borrow_and_update() {
            tokio::select! {
                _ = job_stop_rx.changed() => return (task.key.clone(), gen_id),
                _ = predecessor.changed() => {}
            }
        }
        // The predecessor finished (or never was pending); a stop may have
        // raced in.
        if *job_stop_rx.borrow_and_update() {
            return (task.key.clone(), gen_id);
        }
    }

    loop {
        let delay = match task.schedule.delay_after(Utc::now()) {
            Ok(delay) => delay,
            Err(err) => {
                metrics.record_failure();
                tracing::warn!(
                    worker = %task.key.worker,
                    path = %task.key.path,
                    code = %err.code,
                    "cron schedule failed"
                );
                break;
            }
        };

        tokio::select! {
            _ = job_stop_rx.changed() => {
                break;
            }
            _ = tokio::time::sleep(delay) => {
                if let Err(err) =
                    dispatch_cron_request(&app, &index, &task, root_api_key.as_deref()).await
                {
                    metrics.record_failure();
                    tracing::warn!(
                        worker = %task.key.worker,
                        path = %task.key.path,
                        code = %err.code,
                        "cron dispatch failed"
                    );
                } else {
                    metrics.record_execution();
                }
            }
        }
    }
    (task.key, gen_id)
}

async fn dispatch_cron_request(
    app: &Router,
    index: &ManifestIndex,
    task: &CronJobTask,
    root_api_key: Option<&str>,
) -> Result<(), CoreError> {
    // The version is resolved at tick time with the same resolver the
    // reconciliation uses, for the dispatch log only: the request itself
    // goes to the unversioned route and the pipeline resolves it again.
    // Logged as the plain value (no Option debug formatting); `unresolved`
    // when the worker no longer resolves at tick time.
    let resolved = index.resolve_worker(&task.key.worker, None).ok();
    let version = resolved
        .as_ref()
        .map(|worker| worker.version.as_str())
        .unwrap_or("unresolved");

    let request_id = format!("cron-{}", Uuid::new_v4());
    let uri = versionless_route_path(&task.key.worker, &task.key.path);
    let mut request_builder = Request::builder()
        .method(task.key.method.as_str())
        .uri(uri)
        .header(INTERNAL_REQUEST_HEADER, "true")
        .header("x-request-id", &request_id);
    if let Some(root_api_key) = root_api_key {
        request_builder =
            request_builder.header(header::AUTHORIZATION, format!("Bearer {root_api_key}"));
    }
    let request = request_builder
        .body(Body::empty())
        .map_err(|err| CoreError::new("CRON_REQUEST_INVALID", err.to_string()))?;

    let started = tokio::time::Instant::now();
    let response = app
        .clone()
        .oneshot(request)
        .await
        .map_err(|err| CoreError::new("CRON_DISPATCH_ERROR", err.to_string()))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    tracing::info!(
        worker = %task.key.worker,
        version = %version,
        path = %task.key.path,
        method = %task.key.method,
        request_id = %request_id,
        status = response.status().as_u16(),
        duration_ms,
        "cron dispatched"
    );

    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        Ok(())
    } else {
        Err(CoreError::new(
            "CRON_DISPATCH_STATUS",
            format!("cron request returned {}", response.status()),
        ))
    }
}

fn versionless_route_path(worker: &str, job_path: &str) -> String {
    let mut route = format!("/{worker}");
    if job_path != "/" {
        route.push('/');
        route.push_str(job_path.trim_start_matches('/'));
    }
    route
}

fn validate_job(worker: &WorkerRef, job: &CronJob) -> Result<(), CoreError> {
    if job.schedule.trim().is_empty() {
        let field = format!("{}.cron.schedule", worker.name);
        return Err(CoreError::validation(&field, "schedule is required"));
    }
    if !job.path.starts_with('/') {
        let field = format!("{}.cron.path", worker.name);
        return Err(CoreError::validation(&field, "path must start with /"));
    }
    let method = job.method.as_deref().unwrap_or(DEFAULT_METHOD);
    Method::from_bytes(method.as_bytes()).map_err(|_| {
        let field = format!("{}.cron.method", worker.name);
        CoreError::validation(&field, format!("invalid HTTP method: {method}"))
    })?;
    Ok(())
}

fn normalize_standard_cron(schedule: &str) -> Result<String, CoreError> {
    let parts = schedule.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 5 {
        return Err(invalid_schedule(schedule));
    }

    let day_of_week =
        normalize_day_of_week_field(parts[4]).ok_or_else(|| invalid_schedule(schedule))?;
    Ok(format!(
        "{} {} {} {} {}",
        parts[0], parts[1], parts[2], parts[3], day_of_week
    ))
}

fn normalize_day_of_week_field(field: &str) -> Option<String> {
    field
        .split(',')
        .map(normalize_day_of_week_part)
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join(","))
}

fn normalize_day_of_week_part(part: &str) -> Option<String> {
    if part.is_empty() || part == "*" || part.starts_with("*/") {
        return Some(part.to_owned());
    }
    if part.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return Some(part.to_owned());
    }

    let (base, step) = part.split_once('/').unwrap_or((part, ""));
    if step.is_empty() {
        return normalize_day_of_week_base(base);
    }

    let step = step.parse::<usize>().ok().filter(|value| *value > 0)?;
    if let Some((start, end)) = parse_day_of_week_range(base) {
        let values = (start..=end)
            .step_by(step)
            .filter_map(map_standard_day_of_week)
            .collect::<BTreeSet<_>>();
        return Some(join_day_of_week_values(values));
    }

    parse_day_of_week_number(base)
        .and_then(|value| map_standard_day_of_week(value).map(|mapped| format!("{mapped}/{step}")))
}

fn normalize_day_of_week_base(base: &str) -> Option<String> {
    if let Some((start, end)) = parse_day_of_week_range(base) {
        let values = (start..=end)
            .filter_map(map_standard_day_of_week)
            .collect::<BTreeSet<_>>();
        return Some(join_day_of_week_values(values));
    }

    parse_day_of_week_number(base)
        .and_then(map_standard_day_of_week)
        .map(|value| value.to_string())
}

fn parse_day_of_week_range(base: &str) -> Option<(u32, u32)> {
    let (start, end) = base.split_once('-')?;
    let start = parse_day_of_week_number(start)?;
    let end = parse_day_of_week_number(end)?;
    (start <= end).then_some((start, end))
}

fn parse_day_of_week_number(value: &str) -> Option<u32> {
    value.parse::<u32>().ok().filter(|value| *value <= 7)
}

fn map_standard_day_of_week(value: u32) -> Option<u32> {
    match value {
        0 | 7 => Some(1),
        1..=6 => Some(value + 1),
        _ => None,
    }
}

fn join_day_of_week_values(values: BTreeSet<u32>) -> String {
    values
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn invalid_schedule(schedule: &str) -> CoreError {
    CoreError::new(
        "CRON_SCHEDULE_INVALID",
        format!("unsupported cron schedule: {schedule}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::TimeZone;

    fn datetime(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
            .unwrap()
    }

    #[test]
    fn schedule_parser_accepts_every_interval() {
        let schedule = CronSchedule::parse("@every 50ms").unwrap();

        assert!(matches!(
            schedule,
            CronSchedule::Every(d) if d == Duration::from_millis(50)
        ));
    }

    #[test]
    fn schedule_parser_accepts_standard_midnight_cron() {
        let schedule = CronSchedule::parse("0 0 * * *").unwrap();
        let next = schedule
            .next_fire_after(datetime(2024, 1, 1, 23, 59, 30))
            .unwrap();

        assert_eq!(next, datetime(2024, 1, 2, 0, 0, 0));
    }

    #[test]
    fn schedule_parser_accepts_standard_monday_day_of_week() {
        let schedule = CronSchedule::parse("0 9 * * 1").unwrap();
        let next = schedule
            .next_fire_after(datetime(2024, 1, 7, 8, 0, 0))
            .unwrap();

        assert_eq!(next, datetime(2024, 1, 8, 9, 0, 0));
    }

    #[test]
    fn schedule_parser_accepts_five_minute_step_cron() {
        let schedule = CronSchedule::parse("*/5 * * * *").unwrap();
        let next = schedule
            .next_fire_after(datetime(2024, 1, 1, 0, 2, 30))
            .unwrap();

        assert_eq!(next, datetime(2024, 1, 1, 0, 5, 0));
    }

    #[test]
    fn schedule_parser_rejects_invalid_cron_expression() {
        let err = CronSchedule::parse("not a cron").unwrap_err();

        assert_eq!(err.code, "CRON_SCHEDULE_INVALID");
        assert!(err.message.contains("not a cron"));
    }

    #[test]
    fn versionless_route_omits_the_version_and_keeps_the_job_path() {
        assert_eq!(versionless_route_path("demo", "/tick"), "/demo/tick");
        assert_eq!(versionless_route_path("demo", "/"), "/demo");
        assert_eq!(
            versionless_route_path("@scope/demo", "/tick"),
            "/@scope/demo/tick"
        );
    }

    #[test]
    fn job_key_normalizes_the_default_method_and_ignores_the_version() {
        // The key is (worker name, schedule, path, method) WITHOUT the
        // version: a promote with the same `cron` entries produces the same
        // key, so the running task survives. The method falls back to GET,
        // so a manifest with no method and one with an explicit GET share a
        // key.
        let implicit = CronJob {
            schedule: "@every 5s".into(),
            path: "/tick".into(),
            method: None,
        };
        let explicit = CronJob {
            schedule: "@every 5s".into(),
            path: "/tick".into(),
            method: Some("GET".into()),
        };
        let different_method = CronJob {
            schedule: "@every 5s".into(),
            path: "/tick".into(),
            method: Some("POST".into()),
        };
        let different_schedule = CronJob {
            schedule: "@every 10s".into(),
            path: "/tick".into(),
            method: None,
        };

        fn key(worker: &str, job: &CronJob) -> CronJobKey {
            CronJobKey {
                worker: worker.into(),
                schedule: job.schedule.clone(),
                path: job.path.clone(),
                method: job.method.clone().unwrap_or_else(|| DEFAULT_METHOD.into()),
            }
        }

        assert_eq!(key("demo", &implicit), key("demo", &explicit));
        assert_ne!(key("demo", &implicit), key("demo", &different_method));
        assert_ne!(key("demo", &implicit), key("demo", &different_schedule));
        assert_ne!(key("demo", &implicit), key("other", &implicit));
    }

    /// A task that finished normally must release its `task_ids` entry on
    /// reaping (review probe, now permanent): the mapping is removed on the
    /// success path, not only on the `Err` (abort/panic) path — otherwise
    /// enable/disable and cron changes would accumulate entries for the
    /// life of the supervisor.
    #[tokio::test]
    async fn successful_reap_releases_task_id() {
        tokio::time::pause();
        let mut state = JobState::new(Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let key = CronJobKey {
            worker: "probe".into(),
            schedule: "@every 60s".into(),
            path: "/tick".into(),
            method: "GET".into(),
        };
        let desired = BTreeMap::from([(
            key,
            DesiredJob {
                schedule: CronSchedule::Every(Duration::from_secs(60)),
            },
        )]);
        state.start_missing(
            &desired,
            &CronSchedulerConfig::new(Some("test".into())),
            &ManifestIndex::new(),
            &Router::new(),
            &CronMetrics::default(),
        );
        // Give the spawned task a chance to register its stop subscription.
        tokio::task::yield_now().await;
        state.stop_removed(&BTreeMap::new());
        while let Some(result) = state.jobs.join_next_with_id().await {
            state.reap_result(result);
        }
        assert_eq!(state.live_gen_count(), 0);
        assert_eq!(state.jobs.len(), 0);
        assert_eq!(
            state.task_ids.len(),
            0,
            "finished successful task retained in supervisor task_ids"
        );
    }
}
