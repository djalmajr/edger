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
//!
//! (EDG-34) Read-out publication: the counters and a `CronJobsSnapshot`
//! (state + next dispatch instant per tracked job) live inside the shared
//! `CronMetrics` object. The supervisor stays the single writer: it updates
//! the counters and sends the snapshot through a `watch` channel (everyone
//! else reads through the receiver — `CronMetrics::jobs`, the admin
//! endpoint and `/metrics`). Each job task keeps its OWN sleep deadline in
//! a `watch` cell (the task's next-run, not `now + interval` recomputed
//! later) that the supervisor reads when it builds a snapshot; the task
//! additionally signals every new deadline over a bounded task-to-
//! supervisor channel (`try_send`, one signal per sleep) so the snapshot
//! is republished promptly — a dropped signal (full queue) only delays the
//! republish until the next reconciliation, it can never make the snapshot
//! show a deadline the task has already replaced.
//!
//! (EDG-34 emenda 2) The deadline cell holds the MONOTONIC instant the
//! task will wait for (`tokio::time::Instant`), for both `@every` and
//! wall-clock schedules. The conversion to a wall-clock instant happens
//! when the supervisor reads the cell (at snapshot build time), with the
//! CURRENT wall↔monotonic relationship (`wall_now + (deadline -
//! mono_now)`, saturating to the current instant when the deadline has
//! already fired — the snapshot never shows an expired deadline as the
//! next run). The wall clock is injectable (`CronSchedulerConfig::
//! with_wall`, default `Utc::now`), so a civil wall adjustment (NTP step,
//! suspend) is picked up on the next republish without touching the
//! task's committed monotonic deadline. The snapshot is also republished
//! during the shutdown transitions (after every generation is signaled and
//! after each reap during the grace and abort-wait windows), with the
//! counters coming from the actual reaping. The `enabled` read-out is
//! `true` only while the scheduler will actually dispatch — started AND
//! with the Bearer root configured (`CronSchedulerConfig::new(None)` stays
//! `false`) — and a drop guard in the supervisor flips it back to `false`
//! when the supervisor ends, by shutdown, abort or panic. Manifest-derived
//! fields (`worker`, `schedule`, `path`, `method`) are logged with unambiguous
//! Debug escaping (`?field`) on every cron line, and the manifest
//! validation rejects control characters in the `cron[]` `path` and
//! `schedule`.
//!
//! The job-set change lines (`cron job started` / `cron job stopped` /
//! `cron job drained`) are `info`, one line per change (never per
//! reconciliation), each carrying the version the unversioned route
//! resolves to at that moment (`unresolved` when it cannot resolve, like
//! `cron dispatched`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use chrono::{DateTime, Datelike, Utc};
/// An injectable wall clock (EDG-34): returns the current civil instant.
/// The task sleeps are driven by the monotonic clock
/// (`tokio::time::Instant`); the wall clock is consulted only at the read
/// points — the supervisor's deadline→UTC conversion when it builds the
/// snapshot and a wall-clock schedule's next fire — always with the
/// CURRENT wall↔monotonic relationship, never a boot-time anchor. The
/// default is the real civil clock; the paused-clock tests inject a wall
/// that follows the (virtual) monotonic clock, plus a jump offset.
pub type WallClock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
/// The version resolver of the supervisor's per-pass memo (EDG-34 round
/// 4, F1): given the index and a worker name, the version the unversioned
/// route resolves to right now, or `None`. Default:
/// `ManifestIndex::resolve_default_version`; the paused-clock tests inject
/// a COUNTING resolver to assert that the supervisor resolves each worker
/// name at most once per pass — never once per job (F1).
pub type VersionResolver = Arc<dyn Fn(&ManifestIndex, &str) -> Option<String> + Send + Sync>;
use edger_core::{
    parse_duration_string_to_ms, CoreError, CronJob, WorkerRef, INTERNAL_REQUEST_HEADER,
    MIN_CRON_EVERY_MS,
};
use saffron::Cron;
use tokio::sync::{mpsc, oneshot, watch};
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
/// Capacity of the task → supervisor next-run SIGNAL channel (EDG-34):
/// the supervisor wakes on every signal and drains the burst, so the queue
/// stays near empty in steady state; a full queue drops the signal
/// (harmless — the deadline itself lives in the task's `watch` cell, which
/// the supervisor reads at the next snapshot publish).
pub const JOB_EVENTS_CAPACITY: usize = 64;

#[derive(Clone, Debug)]
pub struct CronMetrics {
    inner: Arc<CronMetricsInner>,
}

#[derive(Debug)]
struct CronMetricsInner {
    executions_total: AtomicU64,
    failures_total: AtomicU64,
    /// Tracked-job counters (EDG-34): the supervisor (single owner of the
    /// job tasks) is the only writer; `CronScheduler`, the admin view and
    /// `/metrics` read the same atomics.
    active_jobs: Arc<AtomicUsize>,
    draining_jobs: Arc<AtomicUsize>,
    /// Whether the scheduler is running (set by `CronScheduler::start`;
    /// the admin endpoint reports it as `enabled`, EDG-34).
    scheduler_enabled: AtomicBool,
    /// Snapshot of the tracked jobs (EDG-34): the supervisor is the only
    /// sender; every reader uses the receiver (`CronMetrics::jobs`).
    snapshot_tx: watch::Sender<CronJobsSnapshot>,
    snapshot_rx: watch::Receiver<CronJobsSnapshot>,
}

impl Default for CronMetrics {
    fn default() -> Self {
        Self {
            inner: Arc::new(CronMetricsInner::default()),
        }
    }
}

impl Default for CronMetricsInner {
    fn default() -> Self {
        let (snapshot_tx, snapshot_rx) = watch::channel(CronJobsSnapshot::default());
        Self {
            executions_total: AtomicU64::new(0),
            failures_total: AtomicU64::new(0),
            active_jobs: Arc::new(AtomicUsize::new(0)),
            draining_jobs: Arc::new(AtomicUsize::new(0)),
            scheduler_enabled: AtomicBool::new(false),
            snapshot_tx,
            snapshot_rx,
        }
    }
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

    /// Number of jobs actively scheduling dispatches (EDG-34); removed jobs
    /// that are still draining are counted in [`CronMetrics::draining_jobs`].
    pub fn active_jobs(&self) -> usize {
        self.inner.active_jobs.load(Ordering::Relaxed)
    }

    /// Number of removed jobs whose task is still draining (EDG-34).
    pub fn draining_jobs(&self) -> usize {
        self.inner.draining_jobs.load(Ordering::Relaxed)
    }

    /// Whether the scheduler is running (EDG-34): the admin endpoint
    /// reports it as `enabled` and answers with an empty list when it is
    /// off.
    pub fn scheduler_enabled(&self) -> bool {
        self.inner.scheduler_enabled.load(Ordering::Relaxed)
    }
    /// The current read-only snapshot of the tracked jobs (EDG-34).
    pub fn jobs(&self) -> CronJobsSnapshot {
        self.inner.snapshot_rx.borrow().clone()
    }

    fn set_scheduler_enabled(&self, enabled: bool) {
        self.inner
            .scheduler_enabled
            .store(enabled, Ordering::Relaxed);
    }
}

/// The state of one tracked job in the snapshot and the admin listing
/// (EDG-34).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CronJobState {
    /// The job is scheduling dispatches.
    Active,
    /// The job left the desired set and is draining: its in-flight
    /// dispatch finishes normally and it stops scheduling.
    Draining,
}

impl CronJobState {
    /// The value the admin endpoint renders (`"active"` / `"draining"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
        }
    }
}

/// One job entry in the snapshot published by the supervisor (EDG-34). The
/// version is deliberately absent (the job key is versionless): the admin
/// endpoint resolves the serving version at query time with the same
/// resolver the tick uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronJobInfo {
    pub worker: String,
    pub schedule: String,
    pub path: String,
    pub method: String,
    pub state: CronJobState,
    /// The next dispatch instant the task is waiting for (`null` while
    /// draining, before the task's first sleep, when the committed
    /// deadline cannot be represented as a UTC instant, or when the
    /// deadline falls after 9999-12-31T23:59:59.999Z — an instant that
    /// still fits a `DateTime<Utc>` but would serialize with an RFC 3339
    /// extended year that some clients cannot parse, EDG-34 round 4).
    pub next_run_at: Option<DateTime<Utc>>,
}

/// The read-only snapshot of the jobs the supervisor tracks (EDG-34),
/// ordered by worker, schedule, path and method; a key with two generations
/// (a draining predecessor and a live successor) lists the draining one
/// first.
#[derive(Clone, Debug, Default)]
pub struct CronJobsSnapshot {
    pub jobs: Vec<CronJobInfo>,
}

/// Task → supervisor (EDG-34): a signal that a task committed to a new
/// sleep deadline. The deadline VALUE itself lives in the generation's
/// `deadline_rx` `watch` cell (written by the task before each sleep, read
/// by the supervisor when it builds the snapshot); this bounded channel is
/// only the prompt-wake trigger, so a dropped signal (full queue) degrades
/// the republish latency — never the value. The signal carries no identity
/// (the supervisor republishes the whole list from the watch cells).
#[derive(Clone, Copy, Debug)]
struct JobEvent;

#[derive(Clone)]
pub struct CronSchedulerConfig {
    root_api_key: Option<String>,
    reconcile_interval: Duration,
    shutdown_grace: Duration,
    shutdown_abort_wait: Duration,
    /// The injectable wall clock (EDG-34); default: the real civil clock
    /// (`Utc::now`).
    wall: WallClock,
    /// The injectable version resolver of the supervisor's per-pass memo
    /// (EDG-34 round 4, F1); default:
    /// `ManifestIndex::resolve_default_version`.
    version_resolver: VersionResolver,
}

impl std::fmt::Debug for CronSchedulerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The root key is never rendered (the wall clock and the version
        // resolver are `dyn Fn`s and not `Debug` either).
        f.debug_struct("CronSchedulerConfig")
            .field("root_api_key", &self.root_api_key.is_some())
            .field("reconcile_interval", &self.reconcile_interval)
            .field("shutdown_grace", &self.shutdown_grace)
            .field("shutdown_abort_wait", &self.shutdown_abort_wait)
            .field("version_resolver", &true)
            .finish()
    }
}

impl CronSchedulerConfig {
    pub fn new(root_api_key: Option<String>) -> Self {
        Self {
            root_api_key,
            reconcile_interval: DEFAULT_RECONCILE_INTERVAL,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            shutdown_abort_wait: DEFAULT_SHUTDOWN_ABORT_WAIT,
            wall: Arc::new(Utc::now),
            version_resolver: Arc::new(|index, name| index.resolve_default_version(name)),
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

    /// The injectable wall clock (EDG-34; default `Utc::now`): the deadline
    /// → UTC conversion at snapshot build time and a wall-clock schedule's
    /// next fire read the current instant from it.
    pub fn with_wall(mut self, wall: WallClock) -> Self {
        self.wall = wall;
        self
    }

    /// The injectable version resolver of the supervisor's per-pass memo
    /// (EDG-34 round 4, F1; default `ManifestIndex::resolve_default_version`
    ///): the paused-clock tests inject a counting resolver to assert that
    /// the supervisor resolves each worker name at most once per pass —
    /// not once per job.
    pub fn with_version_resolver(mut self, resolver: VersionResolver) -> Self {
        self.version_resolver = resolver;
        self
    }
}

/// The per-pass version-resolution memo (EDG-34 round 4, F1): each worker
/// name is resolved at most ONCE per supervisor pass (reconcile, reap,
/// shutdown) — the value cached on the first job of the name is reused by
/// every later job of the same name in the same pass — so a manifest with
/// many `cron[]` entries costs O(distinct workers) per pass on the shared
/// supervisor, not O(jobs²). The memo is a local of the pass (the admin
/// request keeps its own equivalent local) and is discarded with it.
pub struct VersionMemo {
    versions: HashMap<String, Option<String>>,
    resolver: VersionResolver,
}

impl VersionMemo {
    pub fn new(resolver: VersionResolver) -> Self {
        Self {
            versions: HashMap::new(),
            resolver,
        }
    }

    /// The version of `worker` for this pass: the cached value on a repeat
    /// (the common case — many jobs, one worker), one resolution through
    /// the resolver otherwise. A `None` (the worker no longer resolves) is
    /// cached too: the `unresolved` line of a pass does not re-resolve.
    pub fn version(&mut self, index: &ManifestIndex, worker: &str) -> Option<String> {
        if let Some(cached) = self.versions.get(worker) {
            return cached.clone();
        }
        let version = (self.resolver)(index, worker);
        self.versions.insert(worker.to_string(), version.clone());
        version
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
            // (EDG-36) defense in depth: the manifest validation already
            // rejects a sub-1 s `@every` (and a `0` interval), but an
            // index built before the limit must not schedule one either.
            if ms < MIN_CRON_EVERY_MS {
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
                // Checked: an interval that does not fit the representable
                // UTC range is an error, not a panic in the supervisor.
                now.checked_add_signed(delta).ok_or_else(|| {
                    CoreError::new("CRON_SCHEDULE_INVALID", "cron interval is too large")
                })
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
    /// The sleep deadline the task is committed to (EDG-34): the task
    /// writes `Some(deadline)` before every sleep; the supervisor reads
    /// this cell (not the signal channel) when it builds the snapshot, so
    /// a dropped signal can never show a replaced deadline. `None` until
    /// the task's first sleep. The cell holds the MONOTONIC instant the
    /// task will wait for (both `@every` and wall-clock schedules); the
    /// deadline → UTC conversion happens when the supervisor reads the
    /// cell, with the current wall↔monotonic relationship (emenda 2).
    deadline_rx: watch::Receiver<Option<tokio::time::Instant>>,
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
    /// Task → supervisor next-run signal senders (EDG-34); each task keeps
    /// a clone, the supervisor is the only reader. The signal only triggers
    /// a prompt republish — the deadline value itself lives in the
    /// generation's `deadline_rx`.
    job_events_tx: mpsc::Sender<JobEvent>,
    /// The injectable wall clock (EDG-34): the supervisor uses it when it
    /// converts a task's committed monotonic deadline to the UTC instant
    /// published in the snapshot.
    wall: WallClock,
}

impl JobState {
    fn new(metrics: &CronMetrics, job_events_tx: mpsc::Sender<JobEvent>, wall: WallClock) -> Self {
        Self {
            jobs: JoinSet::new(),
            generations: HashMap::new(),
            task_ids: HashMap::new(),
            next_id: 0,
            invalid: BTreeSet::new(),
            root_key_warned: false,
            active: metrics.inner.active_jobs.clone(),
            draining: metrics.inner.draining_jobs.clone(),
            job_events_tx,
            wall,
        }
    }

    /// Remove the finished generation (and update the counters).
    fn remove_generation(
        &mut self,
        key: &CronJobKey,
        id: u64,
        index: &ManifestIndex,
        memo: &mut VersionMemo,
    ) {
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
                // Resolved through the per-pass memo, EAGERLY (before the
                // event): `tracing` fields are evaluated lazily, only when
                // the event is dispatched to an interested subscriber —
                // resolving here keeps the resolution independent of the
                // log level (EDG-34 round 4, F1).
                let version = resolved_version(memo, index, &key.worker);
                tracing::info!(
                    worker = ?key.worker,
                    schedule = ?key.schedule,
                    path = ?key.path,
                    method = ?key.method,
                    version = %version,
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
    fn reap_result(&mut self, result: ReapOutcome, index: &ManifestIndex, memo: &mut VersionMemo) {
        let id = match &result {
            Ok((id, _)) => *id,
            Err(err) => err.id(),
        };
        let mapped = self.task_ids.remove(&id);
        match result {
            Ok((_, (key, gen_id))) => {
                self.remove_generation(&key, gen_id, index, memo);
            }
            Err(err) => {
                // Aborted or panicked: the `MarkDone` guard still fired, so
                // identity exclusion is safe; the mapping (if still
                // present) names the generation to drop.
                if let Some((key, gen_id)) = mapped {
                    self.remove_generation(&key, gen_id, index, memo);
                }
                if err.is_panic() {
                    // The `JoinError` Display embeds the panic payload
                    // unescaped; log it Debug-escaped so no payload text
                    // can forge a physical log line.
                    tracing::warn!(error = ?err, "cron job task panicked");
                }
            }
        }
    }

    /// Reap every task that already finished (non-blocking).
    fn reap_finished(&mut self, index: &ManifestIndex, memo: &mut VersionMemo) {
        while let Some(result) = self.jobs.try_join_next_with_id() {
            self.reap_result(result, index, memo);
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
    fn reap_by_done(&mut self, index: &ManifestIndex, memo: &mut VersionMemo) {
        let mut done: Vec<(CronJobKey, u64)> = Vec::new();
        for (key, gens) in self.generations.iter() {
            for gen in gens {
                if *gen.done_rx.borrow() {
                    done.push((key.clone(), gen.id));
                }
            }
        }
        for (key, id) in done {
            self.remove_generation(&key, id, index, memo);
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
    fn stop_removed(
        &mut self,
        desired: &BTreeMap<CronJobKey, DesiredJob>,
        index: &ManifestIndex,
        memo: &mut VersionMemo,
    ) {
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
                // Eager (see `remove_generation`): the resolution is
                // independent of the log level (EDG-34 round 4, F1).
                let version = resolved_version(memo, index, &key.worker);
                tracing::info!(
                    worker = ?key.worker,
                    schedule = ?key.schedule,
                    path = ?key.path,
                    method = ?key.method,
                    version = %version,
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
        memo: &mut VersionMemo,
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
            // The task's committed sleep deadline (EDG-34): the task writes
            // it before every sleep; the supervisor reads it at snapshot
            // build time, so the signal channel below is a pure wake
            // trigger and a dropped signal never corrupts the value.
            let (deadline_tx, deadline_rx) = watch::channel(None);
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
                self.job_events_tx.clone(),
                deadline_tx,
                self.wall.clone(),
            ));
            self.task_ids.insert(handle.id(), (key.clone(), id));
            self.generations
                .entry(key.clone())
                .or_default()
                .push(Generation {
                    id,
                    stop_tx,
                    done_rx,
                    deadline_rx,
                    stopped: false,
                });
            self.active.fetch_add(1, Ordering::Relaxed);
            // Eager (see `remove_generation`): the resolution is
            // independent of the log level (EDG-34 round 4, F1).
            let version = resolved_version(memo, index, &key.worker);
            tracing::info!(
                worker = ?key.worker,
                schedule = ?key.schedule,
                path = ?key.path,
                method = ?key.method,
                version = %version,
                "cron job started"
            );
        }
    }

    /// Publish the current job list to the read-only snapshot channel
    /// (EDG-34): the state of every live generation plus, for the active
    /// ones, the sleep deadline the task is committed to — read LIVE from
    /// the generation's `deadline_rx` watch cell (the task's own value,
    /// never a supervisor-side recompute). Runs in the supervisor (single
    /// owner of the job tasks); the send never blocks (the receiver lives
    /// in the process-wide `CronMetrics`).
    fn publish_snapshot(&self, metrics: &CronMetrics) {
        let mut jobs = Vec::with_capacity(self.live_gen_count());
        for (key, gens) in &self.generations {
            for gen in gens {
                let state = if gen.stopped {
                    CronJobState::Draining
                } else {
                    CronJobState::Active
                };
                // The deadline the task is waiting for (or, while its
                // dispatch runs, the current instant — the committed
                // deadline already fired and the conversion saturates to
                // `now`; the next deadline is committed only after the
                // dispatch ends). A draining generation stops scheduling:
                // `null`. A generation that has not committed any deadline
                // yet — a successor waiting for its predecessor before its
                // first sleep — is also `null`: the task has not said when
                // it will dispatch, so the snapshot must not invent one.
                // The cell holds the MONOTONIC instant the task will wait
                // for (emenda 2); the conversion to UTC uses the CURRENT
                // wall↔monotonic relationship (`wall_now + (deadline -
                // mono_now)`) with checked operations at every step:
                // saturating to the current instant when the deadline has
                // already fired, and `null` when a deadline that does not
                // fit a `DateTime<Utc>` (a `@every` interval near the
                // largest `Duration`) cannot be represented — the shared
                // supervisor never panics on a manifest value — or when
                // the instant falls after the year 9999: it still fits a
                // `DateTime<Utc>`, but it would serialize with an RFC 3339
                // extended year (`+10000-...`) that some clients cannot
                // parse, so it publishes `null` exactly as the
                // unrepresentable case does (EDG-34 round 4).
                let next_run_at = if gen.stopped {
                    None
                } else {
                    match *gen.deadline_rx.borrow() {
                        // No committed deadline yet: `null` until the
                        // task's first sleep.
                        None => None,
                        Some(deadline) => {
                            let wall_now = (self.wall)();
                            let mono_now = tokio::time::Instant::now();
                            match deadline.checked_duration_since(mono_now) {
                                // The deadline still lies ahead: convert
                                // it with the current wall↔monotonic
                                // relationship; `null` when the result
                                // does not fit a representable UTC
                                // instant (or lies after the year 9999,
                                // above).
                                Some(delta) => chrono::Duration::from_std(delta)
                                    .ok()
                                    .and_then(|delta| wall_now.checked_add_signed(delta))
                                    .filter(|at| at.year() < 10000),
                                // The committed deadline already fired (a
                                // dispatch is running or the sleep is
                                // about to be replaced): the current
                                // instant — the snapshot never shows an
                                // expired deadline as the next run.
                                None => Some(wall_now),
                            }
                        }
                    }
                };
                jobs.push(CronJobInfo {
                    worker: key.worker.clone(),
                    schedule: key.schedule.clone(),
                    path: key.path.clone(),
                    method: key.method.clone(),
                    state,
                    next_run_at,
                });
            }
        }
        // Order: worker, schedule, path, method; a key with two
        // generations (a draining predecessor and a live successor) lists
        // the draining one first.
        jobs.sort_by(|a, b| {
            (
                a.worker.as_str(),
                a.schedule.as_str(),
                a.path.as_str(),
                a.method.as_str(),
            )
                .cmp(&(
                    b.worker.as_str(),
                    b.schedule.as_str(),
                    b.path.as_str(),
                    b.method.as_str(),
                ))
                .then_with(|| {
                    i32::from(a.state == CronJobState::Active)
                        .cmp(&i32::from(b.state == CronJobState::Active))
                })
        });
        let _ = metrics.inner.snapshot_tx.send(CronJobsSnapshot { jobs });
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
    ///
    /// (EDG-34) The tracked-job counters and the job snapshot live inside
    /// the shared `metrics` object: the supervisor is the single writer
    /// and every reader — `CronScheduler`, the admin endpoint and
    /// `/metrics` — sees the same values. The `enabled` read-out is `true`
    /// only when the scheduler will actually dispatch: started AND with
    /// the Bearer root configured — `CronSchedulerConfig::new(None)` stays
    /// `false` (emenda 2); a drop guard in the supervisor flips it back to
    /// `false` when the supervisor ends, by shutdown, abort or panic.
    pub async fn start(
        config: CronSchedulerConfig,
        index: ManifestIndex,
        app: Router,
        metrics: CronMetrics,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (ready_tx, ready_rx) = oneshot::channel();
        if config.root_api_key.is_some() {
            metrics.set_scheduler_enabled(true);
        }
        let supervisor = tokio::spawn(supervise(
            config.clone(),
            index,
            app,
            metrics.clone(),
            shutdown_rx,
            ready_tx,
        ));
        // The supervisor does the first reconciliation before signaling
        // ready, so `start` keeps returning only with the boot jobs running.
        let _ = ready_rx.await;
        Self {
            shutdown_tx,
            supervisor: Some(supervisor),
            config,
            active: metrics.inner.active_jobs.clone(),
            draining: metrics.inner.draining_jobs.clone(),
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

/// Flips the `enabled` read-out back to `false` whenever the supervisor
/// ends (emenda 2) — by shutdown, abort or panic: the guard is dropped
/// with the supervisor's future on every exit path, so the endpoint never
/// reports a scheduler that is no longer running. It never touches the
/// job counters (those come from the actual reaping).
struct SupervisorEnabledGuard(CronMetrics);

impl Drop for SupervisorEnabledGuard {
    fn drop(&mut self) {
        self.0.set_scheduler_enabled(false);
    }
}

/// The supervisor: single owner of all job-task state. The first
/// reconciliation runs before the ready signal (boot workers), then the
/// loop reaps the finished tasks (non-blocking), re-reconciles on the
/// (absolute) deadline and waits for the next finished task, the next
/// deadline or the shutdown signal. Reconciliation, reaping and shutdown
/// all run here, in sequence, with no shared lock — and the whole pass is
/// synchronous, so a shutdown signal is only ever observed at the select
/// boundary, never in the middle of a transfer. (EDG-34) The loop also
/// collects the tasks' next-run updates (a task-to-supervisor channel the
/// tasks own) and republishes the read-only snapshot whenever the set or
/// the next runs can have changed.
async fn supervise(
    config: CronSchedulerConfig,
    index: ManifestIndex,
    app: Router,
    metrics: CronMetrics,
    mut shutdown_rx: watch::Receiver<bool>,
    ready_tx: oneshot::Sender<()>,
) {
    // Task → supervisor next-run updates (EDG-34): the task owns its send
    // half and the supervisor is the only reader. When every task has
    // ended the channel closes: the branch below then parks (`pending`)
    // instead of busy-looping on a permanently-ready `None`.
    let (job_events_tx, job_events_rx) = mpsc::channel(JOB_EVENTS_CAPACITY);
    let mut job_events: Option<mpsc::Receiver<JobEvent>> = Some(job_events_rx);
    let mut state = JobState::new(&metrics, job_events_tx, config.wall.clone());
    // The `enabled` read-out is owned by the supervisor's lifetime
    // (emenda 2): dropped when this task ends, by shutdown, abort or
    // panic.
    let _enabled_guard = SupervisorEnabledGuard(metrics.clone());

    // The version resolver of the per-pass memos (EDG-34 round 4, F1);
    // the memos themselves are locals of each pass, so every pass starts
    // from an empty memo and discards it with the pass.
    let resolver = config.version_resolver.clone();
    {
        let mut memo = VersionMemo::new(resolver.clone());
        reconcile_pass(&mut state, &config, &index, &app, &metrics, &mut memo);
    }
    state.publish_snapshot(&metrics);
    let _ = ready_tx.send(());

    let mut next_reconcile = tokio::time::Instant::now() + config.reconcile_interval;
    loop {
        // One memo per pass (EDG-34 round 4, F1): the reaps of this
        // iteration and the reconcile (when due) share it, so a worker
        // name is resolved at most once per iteration, however many jobs
        // of it change.
        let mut memo = VersionMemo::new(resolver.clone());
        state.reap_finished(&index, &mut memo);
        state.reap_by_done(&index, &mut memo);
        // The reconcile deadline is absolute: a wake for a finished task
        // never starves the periodic pass.
        if tokio::time::Instant::now() >= next_reconcile {
            reconcile_pass(&mut state, &config, &index, &app, &metrics, &mut memo);
            next_reconcile = tokio::time::Instant::now() + config.reconcile_interval;
        }
        // Drain the next-run signals queued since the last wake (one wake
        // processes the whole burst; this catches the stragglers). The
        // signals carry no value (the deadline lives in the generation's
        // `watch` cell, read by the publish below) — they only mark that a
        // republish is due.
        if let Some(rx) = job_events.as_mut() {
            while rx.try_recv().is_ok() {}
        }
        // Reaping, reconciliation or a next-run update may have changed
        // the set or the next runs: republish the read-only snapshot.
        state.publish_snapshot(&metrics);
        if state.live_gen_count() == 0 {
            // No live generations: wait for the next reconcile (or the
            // shutdown signal) instead of spinning on the `JoinSet`.
            tokio::select! {
                _ = shutdown_rx.changed() => break,
                _ = tokio::time::sleep_until(next_reconcile) => {}
            }
            continue;
        }
        // Wait for a finished task, a next-run update, the next deadline or
        // the shutdown signal; a finished task is reaped HERE (the
        // `join_next` result is the only place its (key, generation id)
        // is observable). The `reap_by_done` pass keeps the counters
        // correct even for a completion the `JoinSet` does not report
        // promptly.
        let woken = tokio::select! {
            _ = shutdown_rx.changed() => SupervisorWake::Shutdown,
            result = state.jobs.join_next_with_id() => SupervisorWake::Reaped(result),
            event = async {
                match job_events.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => SupervisorWake::JobEvent(event),
            _ = tokio::time::sleep_until(next_reconcile) => SupervisorWake::Deadline,
        };
        match woken {
            SupervisorWake::Shutdown => break,
            SupervisorWake::Reaped(result) => {
                if let Some(result) = result {
                    // The reap of one finished task is its own pass: its
                    // own memo (EDG-34 round 4, F1).
                    let mut memo = VersionMemo::new(resolver.clone());
                    state.reap_result(result, &index, &mut memo);
                }
                state.publish_snapshot(&metrics);
            }
            SupervisorWake::JobEvent(Some(_)) => {
                // A task committed to a new sleep deadline: republish the
                // snapshot (the deadline VALUE is read live from the
                // generation's `watch` cell, so the signal is a pure wake
                // trigger and is not read).
                state.publish_snapshot(&metrics);
            }
            SupervisorWake::JobEvent(None) => {
                // Every task has ended: drop the branch so the closed
                // `recv` (permanently ready with `None`) cannot busy-loop
                // the select.
                job_events = None;
            }
            SupervisorWake::Deadline => {}
        }
    }

    // Shutdown: no new admissions, signal every generation, let the
    // in-flight dispatches finish until the grace deadline, then abort the
    // stragglers and wait for their end — through the `JoinSet` — until the
    // abort-wait deadline; a task that does not yield is reported (`cron
    // shutdown incomplete`) instead of being declared stopped.
    shutdown_pass(&mut state, &config, &index, &metrics).await;
    state.publish_snapshot(&metrics);
}

/// Which event woke the supervisor loop.
enum SupervisorWake {
    Shutdown,
    Reaped(Option<ReapOutcome>),
    /// A next-run update from a job task (`None` = the channel closed).
    JobEvent(Option<JobEvent>),
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
    memo: &mut VersionMemo,
) {
    let desired = scan_desired_jobs(index, metrics, &mut state.invalid);

    if !desired.is_empty() && config.root_api_key.is_none() && !state.root_key_warned {
        state.root_key_warned = true;
        tracing::warn!(
            "manifest cron jobs are enabled without ROOT_API_KEY or EDGER_ROOT_KEY_FILE; jobs will dispatch without Authorization and x-edger-internal will be discarded by the pipeline"
        );
    }

    state.reap_finished(index, memo);
    state.stop_removed(&desired, index, memo);
    state.start_missing(&desired, config, index, app, metrics, memo);
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
async fn shutdown_pass(
    state: &mut JobState,
    config: &CronSchedulerConfig,
    index: &ManifestIndex,
    metrics: &CronMetrics,
) {
    // One memo for the WHOLE shutdown pass (EDG-34 round 4, F1): the
    // reaping of the signaled generations — potentially thousands of jobs
    // of one worker — resolves each name once, through this single memo.
    let mut memo = VersionMemo::new(config.version_resolver.clone());
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
    // Publish the shutdown transition (emenda 2): the counters moved to
    // `draining` through the actual signaling above, and the snapshot
    // must say so while the in-flight dispatches are still finishing.
    state.publish_snapshot(metrics);

    // Let the tasks finish until the grace deadline, reaping as we go.
    let grace_deadline = tokio::time::Instant::now() + config.shutdown_grace;
    while !state.jobs.is_empty() && tokio::time::Instant::now() < grace_deadline {
        tokio::select! {
            result = state.jobs.join_next_with_id() => {
                if let Some(result) = result {
                    state.reap_result(result, index, &mut memo);
                }
            }
            _ = tokio::time::sleep_until(grace_deadline) => {}
        }
        state.reap_finished(index, &mut memo);
        // An intermediate reap is visible right away (emenda 2): the
        // reaped job leaves the snapshot and the gauges drop.
        state.publish_snapshot(metrics);
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
                    state.reap_result(result, index, &mut memo);
                }
            }
            _ = tokio::time::sleep_until(abort_deadline) => {}
        }
        state.publish_snapshot(metrics);
    }

    state.reap_finished(index, &mut memo);
    state.publish_snapshot(metrics);
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
        // The error message embeds manifest-derived text (e.g. `invalid
        // HTTP method: <method>`): log it Debug-escaped (`?err`), so a
        // control character can never forge a physical log line
        // (CWE-117).
        tracing::warn!(
            worker = ?worker,
            path = ?path,
            error = ?err,
            "cron job is invalid; the job is skipped until the manifest changes"
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
    events_tx: mpsc::Sender<JobEvent>,
    deadline_tx: watch::Sender<Option<tokio::time::Instant>>,
    wall: WallClock,
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
        // The injectable wall clock (EDG-34): the delay of a wall-clock
        // schedule derives from the CURRENT civil instant, never from a
        // boot-time anchor.
        let now = (wall)();
        let delay = match task.schedule.delay_after(now) {
            Ok(delay) => delay,
            Err(err) => {
                metrics.record_failure();
                tracing::warn!(
                    worker = ?task.key.worker,
                    path = ?task.key.path,
                    code = %err.code,
                    "cron schedule failed"
                );
                break;
            }
        };

        // Publish the instant this sleep will wake (EDG-34): the MONOTONIC
        // deadline the task is ABOUT to wait for — `mono_now + interval`
        // for `@every` (a dispatch slower than the interval moves it; it
        // is never recomputed from the last fire or from query time) and
        // `mono_now + delay` for the wall clock (the same delay the task
        // will sleep for, derived from the expression's next matching
        // instant). The conversion to a wall-clock instant happens when
        // the supervisor reads the cell, with the current
        // wall↔monotonic relationship. The addition is CHECKED: an `@every`
        // interval near the largest `Duration` does not fit an
        // `Instant` — then the cell is left without a commit (the
        // snapshot shows `null` for this job's `nextRunAt`) and the sleep
        // below still saturates to the far future, exactly as before
        // EDG-34.
        let at = tokio::time::Instant::now().checked_add(delay);
        if let Some(at) = at {
            // The authoritative value: the task's own committed deadline,
            // in a `watch` cell the supervisor reads when it builds the
            // snapshot (EDG-34 amendment) — a signal dropped below can
            // never make the snapshot show a deadline this task has
            // already replaced.
            // A failure means the generation's receiver is already gone
            // (reaped): nothing to inform anymore.
            let _ = deadline_tx.send(Some(at));
            // The prompt-wake signal (`try_send`, never await): the
            // supervisor drains it on every wake; if the bounded queue is
            // full (supervisor busy or shutting down) the signal is
            // dropped and the next reconciliation republishes the list —
            // the deadline above is already safe.
            let _ = events_tx.try_send(JobEvent);
        }

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
                        worker = ?task.key.worker,
                        path = ?task.key.path,
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
        worker = ?task.key.worker,
        version = %version,
        path = ?task.key.path,
        method = ?task.key.method,
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

/// The version the unversioned route resolves to RIGHT NOW, for the
/// job-set change log lines (EDG-34): the plain value, `unresolved` when
/// the worker no longer resolves — the same convention as the `cron
/// dispatched` line. The value comes through the per-pass memo, so a pass
/// with many jobs of one worker resolves the name once, not per job
/// (EDG-34 round 4, F1).
fn resolved_version(memo: &mut VersionMemo, index: &ManifestIndex, worker: &str) -> String {
    memo.version(index, worker)
        .unwrap_or_else(|| "unresolved".to_owned())
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
        let schedule = CronSchedule::parse("@every 1s").unwrap();

        assert!(matches!(
            schedule,
            CronSchedule::Every(d) if d == Duration::from_millis(1000)
        ));
    }

    /// (EDG-36) an `@every` interval below the 1 s minimum is an invalid
    /// schedule (the `0` interval keeps being invalid too), including an
    /// `@every` the duration parser does not accept (an overflow no longer
    /// wraps or panics — EDG-36).
    #[test]
    fn schedule_parser_rejects_every_below_the_one_second_minimum() {
        for schedule in [
            "@every 0s",
            "@every 50ms",
            "@every 999ms",
            "@every 18446744073709552s",
        ] {
            let err = CronSchedule::parse(schedule).unwrap_err();
            assert_eq!(err.code, "CRON_SCHEDULE_INVALID");
            assert!(
                err.message.contains(schedule),
                "the message names the schedule: {schedule}"
            );
        }
        let boundary = CronSchedule::parse("@every 1000ms").unwrap();
        assert!(matches!(
            boundary,
            CronSchedule::Every(d) if d == Duration::from_millis(1000)
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
        let metrics = CronMetrics::default();
        let (events_tx, _events_rx) = mpsc::channel(JOB_EVENTS_CAPACITY);
        let mut state = JobState::new(&metrics, events_tx, Arc::new(Utc::now));
        let index = ManifestIndex::new();
        // The per-pass memo the job-set change lines resolve through
        // (EDG-34 round 4, F1): the default resolver.
        let mut memo =
            VersionMemo::new(Arc::new(|index, name| index.resolve_default_version(name)));
        let key = CronJobKey {
            worker: "probe".into(),
            schedule: "@every 60s".into(),
            path: "/tick".into(),
            method: "GET".into(),
        };
        let desired = BTreeMap::from([(
            key.clone(),
            DesiredJob {
                schedule: CronSchedule::Every(Duration::from_secs(60)),
            },
        )]);
        state.start_missing(
            &desired,
            &CronSchedulerConfig::new(Some("test".into())),
            &index,
            &Router::new(),
            &metrics,
            &mut memo,
        );
        // Give the spawned task a chance to register its stop subscription.
        tokio::task::yield_now().await;
        state.stop_removed(&BTreeMap::new(), &index, &mut memo);
        while let Some(result) = state.jobs.join_next_with_id().await {
            state.reap_result(result, &index, &mut memo);
        }
        assert_eq!(state.live_gen_count(), 0);
        assert_eq!(state.jobs.len(), 0);
        assert_eq!(
            state.task_ids.len(),
            0,
            "finished successful task retained in supervisor task_ids"
        );
    }

    /// (EDG-34 round 4, F1) The per-pass memo resolves each worker name
    /// ONCE, no matter how many jobs of the name flow through the pass:
    /// the value is cached and the resolver is not called again for the
    /// name; a `None` (the worker no longer resolves) is cached too. The
    /// counting resolver is the F1 assertion without a real clock: a pass
    /// with 2_000 jobs of one worker must resolve the name exactly once.
    #[test]
    fn version_memo_resolves_each_worker_name_once_per_pass() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let resolver: VersionResolver = std::sync::Arc::new(move |_index, name| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if name == "absent" {
                None
            } else {
                Some("1.0.0".to_owned())
            }
        });
        let index = ManifestIndex::new();
        let mut memo = VersionMemo::new(resolver);
        // Two thousand jobs of one worker (the F1 shape):
        for _ in 0..2000 {
            assert_eq!(
                memo.version(&index, "many"),
                Some("1.0.0".to_owned()),
                "every job of the name gets the version"
            );
        }
        // A worker that does not resolve: `None`, and it is cached too.
        assert_eq!(memo.version(&index, "absent"), None);
        assert_eq!(memo.version(&index, "absent"), None);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one resolution per NAME, not per job"
        );
    }
}
