//! Cron scheduler tests (EDG-26): the scheduler reconciles a living set of
//! jobs from the `ManifestIndex` (only the version the unversioned route
//! resolves to contributes its `cron` entries) and dispatches the
//! unversioned route, so every tick follows the current default version.
//!
//! The reconciliation tests run on a paused tokio clock
//! (`tokio::time::pause`/`advance`) so the reconcile interval and the
//! schedule intervals are exact.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use bytes::Bytes;
use chrono::{DateTime, TimeZone, Timelike, Utc};
use edger_core::{
    CronJob, Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig,
    WorkerManifest, INTERNAL_REQUEST_HEADER,
};
use edger_orchestrator::{
    build_pipeline, ControlAuth, ControlAuthConfig, CronJobState, CronMetrics, CronScheduler,
    CronSchedulerConfig, ManifestIndex, OrchestratorState, ServerState, WallClock,
    JOB_EVENTS_CAPACITY,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

const RECONCILE: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// Recording isolate: stamps every recorded request with the version of the
// worker ref it was created for, so a test can see which version served a
// tick.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct RecordedRequests {
    inner: Arc<Mutex<Vec<(String, SerializedRequest)>>>,
    stamps: Arc<Mutex<Vec<tokio::time::Instant>>>,
}

impl RecordedRequests {
    fn push(&self, version: &str, request: SerializedRequest) {
        self.stamps
            .lock()
            .expect("recorded request stamps")
            .push(tokio::time::Instant::now());
        self.inner
            .lock()
            .expect("recorded requests")
            .push((version.to_owned(), request));
    }

    fn count(&self) -> usize {
        self.inner.lock().expect("recorded requests").len()
    }

    fn count_for(&self, version: &str) -> usize {
        self.inner
            .lock()
            .expect("recorded requests")
            .iter()
            .filter(|(worker_version, _)| worker_version == version)
            .count()
    }

    fn count_for_path(&self, path: &str) -> usize {
        self.inner
            .lock()
            .expect("recorded requests")
            .iter()
            .filter(|(_, request)| request.uri == path)
            .count()
    }

    fn last_for(&self, version: &str) -> Option<SerializedRequest> {
        self.inner
            .lock()
            .expect("recorded requests")
            .iter()
            .rev()
            .find(|(worker_version, _)| worker_version == version)
            .map(|(_, request)| request.clone())
    }

    fn last_for_path(&self, path: &str) -> Option<SerializedRequest> {
        self.inner
            .lock()
            .expect("recorded requests")
            .iter()
            .rev()
            .find(|(_, request)| request.uri == path)
            .map(|(_, request)| request.clone())
    }

    fn first(&self) -> Option<SerializedRequest> {
        self.inner
            .lock()
            .expect("recorded requests")
            .first()
            .map(|(_, request)| request.clone())
    }

    /// Span between the first and the last recorded tick (paused clock).
    fn span(&self) -> Option<Duration> {
        let stamps = self.stamps.lock().expect("recorded request stamps");
        match (stamps.first(), stamps.last()) {
            (Some(first), Some(last)) if first != last => Some(*last - *first),
            _ => None,
        }
    }
}

/// Fake isolate that makes each dispatch take `delay` of paused clock and
/// counts how many dispatches run at the same time: a job that could
/// overlap its own dispatches would push the peak above 1.
#[derive(Clone)]
struct SlowFactory {
    requests: RecordedRequests,
    delay: Duration,
    in_flight: Arc<AtomicBool>,
    peak: Arc<AtomicUsize>,
    current: Arc<AtomicUsize>,
}

impl IsolateFactory for SlowFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(SlowIsolate {
            requests: self.requests.clone(),
            delay: self.delay,
            in_flight: self.in_flight.clone(),
            peak: self.peak.clone(),
            current: self.current.clone(),
            version: worker_ref.version.clone(),
        })
    }
}

struct SlowIsolate {
    requests: RecordedRequests,
    delay: Duration,
    in_flight: Arc<AtomicBool>,
    peak: Arc<AtomicUsize>,
    current: Arc<AtomicUsize>,
    version: String,
}

#[async_trait]
impl Isolate for SlowIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let in_flight_now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(in_flight_now, Ordering::SeqCst);
        self.in_flight.store(true, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.in_flight.store(false, Ordering::SeqCst);
        self.current.fetch_sub(1, Ordering::SeqCst);
        self.requests.push(&self.version, req);
        Ok(SerializedResponse {
            status: 204,
            headers: vec![],
            body: None,
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: Some(Bytes::from(format!("<html>{path}</html>"))),
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

struct RecordingFactory {
    requests: RecordedRequests,
}

impl IsolateFactory for RecordingFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(RecordingIsolate {
            requests: self.requests.clone(),
            version: worker_ref.version.clone(),
        })
    }
}

struct RecordingIsolate {
    requests: RecordedRequests,
    version: String,
}

#[async_trait]
impl Isolate for RecordingIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.requests.push(&self.version, req);
        Ok(SerializedResponse {
            status: 204,
            headers: vec![],
            body: None,
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: Some(Bytes::from(format!("<html>{path}</html>"))),
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Fixtures and clock helpers
// ---------------------------------------------------------------------------

fn job(schedule: &str, path: &str, method: Option<&str>) -> CronJob {
    CronJob {
        schedule: schedule.into(),
        path: path.into(),
        method: method.map(|method| method.to_owned()),
    }
}

fn manifest(name: &str, version: &str, cron: Option<Vec<CronJob>>) -> WorkerManifest {
    WorkerManifest {
        name: name.into(),
        version: Some(version.into()),
        entrypoint: Some("index.ts".into()),
        cron,
        ..Default::default()
    }
}

/// Create the version dir (production layout `<root>/<name>/<version>`) and
/// optionally the staged revision marker.
fn version_dir(root: &Path, name: &str, version: &str, staged: bool) -> PathBuf {
    let dir = root.join(name).join(version);
    std::fs::create_dir_all(&dir).unwrap();
    if staged {
        std::fs::write(dir.join(".edger-revision"), "rev-1\nstaged=true\n").unwrap();
    }
    dir
}

fn insert_version(
    index: &mut ManifestIndex,
    root: &Path,
    name: &str,
    version: &str,
    cron: Option<Vec<CronJob>>,
    staged: bool,
) {
    let dir = version_dir(root, name, version, staged);
    index
        .insert(dir, manifest(name, version, cron))
        .unwrap_or_else(|err| panic!("insert {name}@{version}: {err}"));
}

async fn start_harness(
    index: ManifestIndex,
    requests: RecordedRequests,
    root_key: Option<&str>,
) -> (CronScheduler, Router, CronMetrics) {
    start_harness_factory(
        index,
        Arc::new(RecordingFactory {
            requests: requests.clone(),
        }),
        root_key,
    )
    .await
}

async fn start_harness_reconcile(
    index: ManifestIndex,
    requests: RecordedRequests,
    reconcile: Duration,
    root_key: Option<&str>,
) -> (CronScheduler, Router, CronMetrics) {
    start_harness_factory_at(
        index,
        Arc::new(RecordingFactory {
            requests: requests.clone(),
        }),
        reconcile,
        root_key,
    )
    .await
}

async fn start_harness_factory(
    index: ManifestIndex,
    factory: Arc<dyn IsolateFactory>,
    root_key: Option<&str>,
) -> (CronScheduler, Router, CronMetrics) {
    start_harness_factory_at(index, factory, RECONCILE, root_key).await
}

async fn start_harness_slow(
    index: ManifestIndex,
    requests: RecordedRequests,
    delay: Duration,
    in_flight: Arc<AtomicBool>,
    peak: Arc<AtomicUsize>,
    current: Arc<AtomicUsize>,
    root_key: Option<&str>,
) -> (CronScheduler, Router, CronMetrics) {
    start_harness_factory(
        index,
        Arc::new(SlowFactory {
            requests,
            delay,
            in_flight,
            peak,
            current,
        }),
        root_key,
    )
    .await
}

async fn start_harness_factory_at(
    index: ManifestIndex,
    factory: Arc<dyn IsolateFactory>,
    reconcile: Duration,
    root_key: Option<&str>,
) -> (CronScheduler, Router, CronMetrics) {
    // The paused-clock tests run against the VIRTUAL wall (EDG-34 emenda
    // 2): the civil instant follows the (paused) monotonic clock, so the
    // deadline → UTC conversion published in the snapshot is deterministic
    // instead of drifting with the real wall clock.
    start_harness_factory_at_wall(index, factory, reconcile, root_key, virtual_wall()).await
}

async fn start_harness_factory_at_wall(
    index: ManifestIndex,
    factory: Arc<dyn IsolateFactory>,
    reconcile: Duration,
    root_key: Option<&str>,
    wall: WallClock,
) -> (CronScheduler, Router, CronMetrics) {
    let auth = match root_key {
        Some(key) => ControlAuth::with_static_key(key),
        None => ControlAuth::new(ControlAuthConfig::default()),
    };
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), factory);
    server.mark_ready(pool.clone());
    let state = OrchestratorState {
        server,
        pool,
        index: index.clone(),
        auth,
    };
    let metrics = state.server.cron_metrics();
    let index_for_cron = state.index.clone();
    let app = build_pipeline(state);
    let scheduler = CronScheduler::start(
        CronSchedulerConfig::new(root_key.map(str::to_owned))
            .with_reconcile_interval(reconcile)
            .with_wall(wall),
        index_for_cron,
        app.clone(),
        metrics.clone(),
    )
    .await;
    (scheduler, app, metrics)
}

// ---------------------------------------------------------------------------
// Injectable wall clock for the paused-clock tests (EDG-34 emenda 2): the
// civil instant is the UTC captured at creation plus the ELAPSED VIRTUAL
// time (plus a jump offset a test can apply), so the wall follows the
// (paused) monotonic clock and a civil wall step is modeled without
// touching the host clock.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TestWall {
    base: DateTime<Utc>,
    mono0: tokio::time::Instant,
    offset: Arc<std::sync::Mutex<chrono::Duration>>,
}

impl TestWall {
    fn new() -> Self {
        Self {
            base: Utc::now(),
            mono0: tokio::time::Instant::now(),
            offset: Arc::new(std::sync::Mutex::new(chrono::Duration::zero())),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        let elapsed = tokio::time::Instant::now() - self.mono0;
        let delta = chrono::Duration::from_std(elapsed).unwrap_or_else(|_| {
            chrono::Duration::seconds(i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        });
        self.base + delta + *self.offset.lock().expect("test wall offset")
    }

    /// Apply a civil wall jump (e.g. +120 s) on top of the virtual time.
    fn jump(&self, by: chrono::Duration) {
        *self.offset.lock().expect("test wall offset") = by;
    }
}

impl std::fmt::Debug for TestWall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestWall").finish_non_exhaustive()
    }
}

/// The `WallClock` the harnesses inject by default: the virtual wall
/// described above.
fn virtual_wall() -> WallClock {
    let wall = Arc::new(TestWall::new());
    Arc::new(move || wall.now())
}

/// A `WallClock` over a `TestWall` the test keeps a handle to (to read the
/// current civil instant or to apply a jump).
fn test_wall(wall: &Arc<TestWall>) -> WallClock {
    let wall = Arc::clone(wall);
    Arc::new(move || wall.now())
}

/// Promote through the admin route (the operator path).
async fn promote(app: &Router, name: &str, version: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/admin/workers/{name}/promote?version={version}"
                ))
                .header("authorization", "Bearer root-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "promote {name}@{version}"
    );
}

/// Advance the paused clock in 10 ms steps until `condition` holds; returns
/// the total virtual time advanced. Panics at `max_total`.
async fn advance_until(max_total: Duration, mut condition: impl FnMut() -> bool) -> Duration {
    let step = Duration::from_millis(10);
    let mut total = Duration::ZERO;
    while !condition() {
        if total >= max_total {
            panic!("condition not met after advancing {max_total:?} of paused clock");
        }
        tokio::time::advance(step).await;
        total += step;
    }
    total
}

/// Advance exactly `total` of virtual time.
async fn advance(total: Duration) {
    let mut advanced = Duration::ZERO;
    while advanced < total {
        let step = total
            .saturating_sub(advanced)
            .min(Duration::from_millis(10));
        tokio::time::advance(step).await;
        advanced += step;
    }
}

// ---------------------------------------------------------------------------
// Process-wide event capture (one global subscriber per test binary)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct CapturedEvent {
    fields: String,
}

#[derive(Clone)]
struct CapturingLayer {
    sink: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl CapturingLayer {
    fn new() -> (Self, Arc<Mutex<Vec<CapturedEvent>>>) {
        let sink = Arc::new(Mutex::new(Vec::new()));
        (Self { sink: sink.clone() }, sink)
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturingLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut capture = FieldCapture { out: String::new() };
        event.record(&mut capture);
        self.sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(CapturedEvent {
                fields: capture.out,
            });
    }
}

/// Records field values the same way the default `fmt` layer does
/// (`name=value`, space separated), into an owned string.
struct FieldCapture {
    out: String,
}

impl tracing::field::Visit for FieldCapture {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if !self.out.is_empty() {
            self.out.push(' ');
        }
        use std::fmt::Write as _;
        let _ = write!(self.out, "{}={:?}", field.name(), value);
    }
}

/// Install the capture subscriber once per process and hand out the shared
/// buffer. Every test filters on its own worker name / request id, so the
/// shared buffer is safe.
fn capture_sink() -> Arc<Mutex<Vec<CapturedEvent>>> {
    static SINK: std::sync::OnceLock<Arc<Mutex<Vec<CapturedEvent>>>> = std::sync::OnceLock::new();
    let sink: Arc<Mutex<Vec<CapturedEvent>>> = SINK
        .get_or_init(|| {
            let (layer, sink) = CapturingLayer::new();
            let _ = tracing_subscriber::registry()
                .with(layer)
                .with(EnvFilter::new("info"))
                .try_init();
            sink
        })
        .clone();
    sink
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// A promote swaps the version that receives the tick. The job key has no
/// version (same `cron` entries), so the task keeps its phase: exactly one
/// tick per interval, no duplicate and no restart gap.
#[tokio::test]
async fn promote_switches_serving_version_without_duplicate_dispatch() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let cron = || Some(vec![job("@every 100ms", "/tick", Some("POST"))]);

    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        cron(),
        false,
    );
    // v2 arrives staged with the same cron: staged versions never contribute.
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "2.0.0",
        cron(),
        true,
    );

    let (mut scheduler, app, metrics) =
        start_harness(index, requests.clone(), Some("root-secret")).await;

    advance_until(Duration::from_millis(500), || requests.count() == 2).await;
    assert_eq!(requests.count_for("1.0.0"), 2);
    assert_eq!(requests.count_for("2.0.0"), 0);
    assert_eq!(metrics.failures_total(), 0);

    promote(&app, "cron-worker", "2.0.0").await;

    // Ticks 3..6 (at +300/+400/+500/+600ms) all hit 2.0.0. If the promote
    // duplicated the job, the per-version counts below would not hold.
    advance_until(Duration::from_millis(640), || requests.count() == 6).await;
    assert_eq!(requests.count(), 6);
    assert_eq!(requests.count_for("1.0.0"), 2);
    assert_eq!(requests.count_for("2.0.0"), 4);
    assert_eq!(metrics.failures_total(), 0);

    // A version-keyed job would restart on the promote: the phase reset
    // adds an extra gap between the last pre-promote tick and the first
    // post-promote one, stretching the span between the first and the
    // sixth tick past 5 intervals (measured 640ms vs 600ms).
    let span = requests.span().expect("six ticks recorded");
    assert!(
        span <= Duration::from_millis(620),
        "the job must keep its phase across the promote: tick span {span:?}"
    );

    let last = requests.last_for("2.0.0").expect("a 2.0.0 tick");
    assert_eq!(last.method, "POST");
    assert_eq!(last.uri, "/tick");
    assert!(last
        .headers
        .iter()
        .any(|(name, value)| name == INTERNAL_REQUEST_HEADER && value == "true"));

    scheduler.shutdown().await;
}

/// An enabled (rollback) version, a staged version and the highest enabled
/// version all carry `cron` and still do not receive a tick: only the version
/// the unversioned route resolves to (the promoted 3.0.0 pointer, which beats
/// the higher 4.0.0) contributes.
#[tokio::test]
async fn staged_and_enabled_rollback_versions_do_not_receive_ticks() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let cron = || Some(vec![job("@every 100ms", "/tick", Some("POST"))]);

    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        cron(),
        true,
    );
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "2.0.0",
        cron(),
        false,
    );
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "3.0.0",
        cron(),
        false,
    );
    // Highest enabled semver: if the promoted pointer did not win, this
    // version (not 3.0.0) would be the one dispatching.
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "4.0.0",
        cron(),
        false,
    );

    let (mut scheduler, app, metrics) =
        start_harness(index, requests.clone(), Some("root-secret")).await;
    promote(&app, "cron-worker", "3.0.0").await;

    advance_until(Duration::from_millis(600), || requests.count() == 3).await;
    assert_eq!(requests.count_for("3.0.0"), 3);
    assert_eq!(
        requests.count_for("2.0.0"),
        0,
        "an enabled rollback version must not receive ticks"
    );
    assert_eq!(
        requests.count_for("1.0.0"),
        0,
        "a staged version must not receive ticks"
    );
    assert_eq!(
        requests.count_for("4.0.0"),
        0,
        "the promoted pointer must beat the highest enabled semver"
    );
    assert_eq!(metrics.failures_total(), 0);

    scheduler.shutdown().await;
}

/// A worker installed after the scheduler started gets its cron job within
/// one reconcile interval (plus the job's own first schedule interval).
#[tokio::test]
async fn worker_installed_after_start_gets_cron_within_one_reconcile_interval() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);

    let (mut scheduler, _app, _metrics) =
        start_harness(index.clone(), requests.clone(), Some("root-secret")).await;

    assert_eq!(scheduler.active_job_count(), 0);
    advance(Duration::from_millis(100)).await;
    assert_eq!(requests.count(), 0);

    // Install, as a rescan would: a fresh version dir added to the index.
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", Some("POST"))]),
        false,
    );

    let installed_at = tokio::time::Instant::now();
    // The job itself must be SCHEDULED within one reconcile interval (the
    // first tick is, on top of that, one schedule interval away).
    advance_until(Duration::from_millis(150), || {
        scheduler.active_job_count() == 1
    })
    .await;
    assert!(
        installed_at.elapsed() <= Duration::from_millis(80),
        "the job was not scheduled within one reconcile interval: {:?}",
        installed_at.elapsed()
    );
    advance_until(Duration::from_millis(200), || requests.count() >= 1).await;
    assert_eq!(requests.count_for("1.0.0"), 1);

    scheduler.shutdown().await;
}

/// Deleting the worker stops its cron job within one reconcile interval and
/// no further tick arrives.
#[tokio::test]
async fn deleted_worker_stops_cron_within_one_reconcile_interval() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", Some("POST"))]),
        false,
    );

    let (mut scheduler, _app, _metrics) =
        start_harness(index.clone(), requests.clone(), Some("root-secret")).await;
    advance_until(Duration::from_millis(500), || requests.count() == 2).await;
    assert_eq!(scheduler.active_job_count(), 1);

    index.remove_worker("cron-worker", "1.0.0").unwrap();
    let deleted_at = tokio::time::Instant::now();
    advance_until(Duration::from_millis(300), || {
        scheduler.active_job_count() == 0
    })
    .await;
    assert!(
        deleted_at.elapsed() <= Duration::from_millis(160),
        "the job was not stopped within one reconcile interval"
    );

    let count = requests.count();
    advance(Duration::from_millis(300)).await;
    assert_eq!(
        requests.count(),
        count,
        "no ticks may arrive after the delete"
    );

    scheduler.shutdown().await;
}

/// Disabling the default version stops its cron job within one reconcile
/// interval and no further tick arrives.
#[tokio::test]
async fn disabled_worker_stops_cron_within_one_reconcile_interval() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", Some("POST"))]),
        false,
    );

    let (mut scheduler, _app, _metrics) =
        start_harness(index.clone(), requests.clone(), Some("root-secret")).await;
    advance_until(Duration::from_millis(500), || requests.count() == 2).await;
    assert_eq!(scheduler.active_job_count(), 1);

    index
        .set_worker_enabled("cron-worker", Some("1.0.0"), false)
        .unwrap();
    let disabled_at = tokio::time::Instant::now();
    advance_until(Duration::from_millis(300), || {
        scheduler.active_job_count() == 0
    })
    .await;
    assert!(
        disabled_at.elapsed() <= Duration::from_millis(160),
        "the job was not stopped within one reconcile interval"
    );

    let count = requests.count();
    advance(Duration::from_millis(300)).await;
    assert_eq!(
        requests.count(),
        count,
        "no ticks may arrive after the disable"
    );

    scheduler.shutdown().await;
}

/// A dispatch slower than the `@every` interval: the same job never
/// overlaps its own dispatches (sequential per job, as in rc.18), and a job
/// stopped by the reconciliation lets the in-flight dispatch finish normally
/// instead of aborting it; only the next one is not scheduled.
#[tokio::test]
async fn slow_dispatches_are_sequential_and_in_flight_dispatch_finishes_on_stop() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let in_flight = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    // The worker takes 150ms while the schedule interval is 100ms.
    let delay = Duration::from_millis(150);

    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "slow-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", Some("POST"))]),
        false,
    );

    let (mut scheduler, _app, _metrics) = start_harness_slow(
        index.clone(),
        requests.clone(),
        delay,
        in_flight.clone(),
        peak.clone(),
        current.clone(),
        Some("root-secret"),
    )
    .await;

    // Two dispatches of the slow worker: if a job could overlap its own
    // dispatch, the second (due at +100ms) would start while the first is
    // still in flight and the peak would be 2.
    advance_until(Duration::from_millis(800), || requests.count() >= 2).await;
    assert!(requests.count() >= 2);
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "the same job must never run more than one dispatch in flight"
    );

    // Stop the job while a dispatch is in flight: it must finish normally
    // (recorded), and nothing is scheduled after the stop.
    advance_until(Duration::from_millis(1500), || {
        in_flight.load(Ordering::SeqCst)
    })
    .await;
    let before_stop = requests.count();
    index
        .set_worker_enabled("slow-worker", Some("1.0.0"), false)
        .unwrap();
    advance_until(Duration::from_millis(300), || {
        scheduler.active_job_count() == 0
    })
    .await;
    advance_until(Duration::from_millis(500), || {
        !in_flight.load(Ordering::SeqCst) && requests.count() == before_stop + 1
    })
    .await;
    assert_eq!(
        requests.count(),
        before_stop + 1,
        "the in-flight dispatch must complete, not be aborted"
    );

    let settled = requests.count();
    advance(Duration::from_millis(400)).await;
    assert_eq!(
        requests.count(),
        settled,
        "no dispatch may be scheduled after the stop"
    );

    scheduler.shutdown().await;
}

/// A promote to a version WITHOUT `cron` stops the job: only the resolved
/// version contributes, even if the old version stays enabled.
#[tokio::test]
async fn promote_to_version_without_cron_stops_the_job() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", Some("POST"))]),
        false,
    );
    insert_version(&mut index, root.path(), "cron-worker", "2.0.0", None, false);

    let (mut scheduler, app, _metrics) =
        start_harness(index.clone(), requests.clone(), Some("root-secret")).await;
    // 1.0.0 serves before the swap; without the pointer the highest semver
    // (2.0.0, no cron) would already be resolved and the job would not start.
    promote(&app, "cron-worker", "1.0.0").await;
    advance_until(Duration::from_millis(500), || requests.count() == 2).await;
    assert_eq!(scheduler.active_job_count(), 1);

    promote(&app, "cron-worker", "2.0.0").await;
    advance_until(Duration::from_millis(300), || {
        scheduler.active_job_count() == 0
    })
    .await;

    let count = requests.count();
    advance(Duration::from_millis(300)).await;
    assert_eq!(
        requests.count(),
        count,
        "the job must stop, not keep ticking 1.0.0"
    );

    scheduler.shutdown().await;
}

/// A promote to a version with a DIFFERENT `cron` entry swaps the job set:
/// the old key stops and the new key starts (default method included).
#[tokio::test]
async fn promote_to_version_with_different_cron_swaps_the_job_set() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", Some("POST"))]),
        false,
    );
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "2.0.0",
        Some(vec![job("@every 150ms", "/other", None)]),
        false,
    );

    let (mut scheduler, app, metrics) =
        start_harness(index.clone(), requests.clone(), Some("root-secret")).await;
    // 1.0.0 is the version that serves before the swap (without a pointer
    // the highest semver would already resolve 2.0.0).
    promote(&app, "cron-worker", "1.0.0").await;
    advance_until(Duration::from_millis(500), || {
        requests.count_for_path("/tick") == 2
    })
    .await;
    assert_eq!(requests.count_for_path("/other"), 0);

    promote(&app, "cron-worker", "2.0.0").await;

    // The /tick job stops; the /other job starts at its own 150ms phase
    // (reconcile at ~+250ms, first tick at ~+400ms).
    advance_until(Duration::from_millis(600), || {
        requests.count_for_path("/other") >= 1
    })
    .await;
    let other = requests.last_for_path("/other").expect("an /other tick");
    assert_eq!(other.method, "GET", "the default method must be used");
    assert_eq!(
        requests.count_for("2.0.0"),
        requests.count_for_path("/other"),
        "every /other tick must be served by the promoted 2.0.0"
    );
    assert_eq!(scheduler.active_job_count(), 1);
    assert_eq!(
        requests.count_for_path("/tick"),
        2,
        "the /tick job must have stopped, not run in parallel"
    );

    // A second /other interval passes; the /tick job does not come back.
    advance_until(Duration::from_millis(600), || {
        requests.count_for_path("/other") >= 2
    })
    .await;
    assert_eq!(requests.count_for_path("/tick"), 2);
    assert_eq!(metrics.failures_total(), 0);

    scheduler.shutdown().await;
}

/// Dispatch goes through the unversioned route, keeps the internal root
/// headers, and the /metrics counters move.
#[tokio::test]
async fn cron_dispatch_uses_unversioned_route_and_counts_execution() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", Some("POST"))]),
        false,
    );

    let (mut scheduler, app, metrics) =
        start_harness(index, requests.clone(), Some("root-secret")).await;
    advance_until(Duration::from_millis(500), || requests.count() == 1).await;

    let metrics_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("authorization", "Bearer root-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let metrics_body = axum::body::to_bytes(metrics_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let metrics_text = String::from_utf8(metrics_body.to_vec()).unwrap();
    scheduler.shutdown().await;

    assert_eq!(metrics.executions_total(), 1);
    assert_eq!(metrics.failures_total(), 0);
    assert!(metrics_text.contains("# TYPE edger_cron_executions_total counter"));
    assert!(metrics_text.contains("# TYPE edger_cron_failures_total counter"));

    let request = requests.first().expect("cron worker request");
    assert_eq!(request.method, "POST");
    assert_eq!(request.uri, "/tick");
    assert!(request.request_id.starts_with("cron-"));
    assert!(request
        .headers
        .iter()
        .any(|(name, value)| name == INTERNAL_REQUEST_HEADER && value == "true"));
    assert!(request
        .headers
        .iter()
        .all(|(name, _)| name != "authorization"));
}

/// Shutdown stops every job; no tick arrives after it.
#[tokio::test]
async fn cron_shutdown_stops_all_jobs() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    for name in ["cron-worker", "other-worker"] {
        insert_version(
            &mut index,
            root.path(),
            name,
            "1.0.0",
            Some(vec![job("@every 100ms", "/tick", Some("POST"))]),
            false,
        );
    }

    let (mut scheduler, _app, _metrics) =
        start_harness(index, requests.clone(), Some("root-secret")).await;
    assert_eq!(scheduler.active_job_count(), 2);
    advance_until(Duration::from_millis(600), || requests.count() >= 2).await;
    let before = requests.count();
    assert!(before >= 2);

    scheduler.shutdown().await;
    advance(Duration::from_millis(500)).await;
    assert_eq!(requests.count(), before, "no ticks after shutdown");
}

/// A schedule that does not parse is skipped (boot no longer fails), counted
/// as one failure per job key, and re-warned only if the key reappears.
#[tokio::test]
async fn invalid_schedule_is_skipped_and_counted_once() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        Some(vec![job("not a cron", "/tick", Some("POST"))]),
        false,
    );

    let (mut scheduler, _app, metrics) =
        start_harness(index, requests.clone(), Some("root-secret")).await;
    assert_eq!(scheduler.active_job_count(), 0);

    // Several reconcile passes: still exactly one failure, no ticks.
    advance(Duration::from_millis(300)).await;
    assert_eq!(
        metrics.failures_total(),
        1,
        "one failure per invalid job, not per reconcile"
    );
    assert_eq!(requests.count(), 0);

    scheduler.shutdown().await;
}

/// A worker that disappears between the tick and the next reconciliation:
/// the dispatch still runs, the pipeline answers 404, and the log line
/// reports the plain marker `version=unresolved` (no `Option` formatting).
/// The cadences (120 ms schedule, 200 ms reconcile) put the first tick
/// (~+130 ms, after the +100 ms delete) strictly before the first periodic
/// reconciliation (~+200 ms), which then stops the job.
#[tokio::test]
async fn dispatch_log_reports_unresolved_version_when_the_worker_is_gone() {
    tokio::time::pause();
    let sink = capture_sink();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();

    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "gone-worker",
        "1.0.0",
        Some(vec![job("@every 120ms", "/tick", Some("POST"))]),
        false,
    );

    let captured_before = sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    let (mut scheduler, _app, _metrics) = start_harness_reconcile(
        index.clone(),
        requests.clone(),
        Duration::from_millis(200),
        Some("root-secret"),
    )
    .await;

    // Step to +100ms in 10 ms increments so the job task registers its
    // first sleep early (the first tick is still pending; the first
    // periodic reconciliation lands only at ~+200ms, after the delete).
    for _ in 0..10 {
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    index.remove_worker("gone-worker", "1.0.0").unwrap();

    // The pending tick dispatches against a worker that no longer resolves:
    // the line reports the plain `unresolved` marker and the pipeline 404.
    advance_until(Duration::from_millis(1000), || {
        let events = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        events[captured_before..].iter().any(|event| {
            event.fields.contains("message=cron dispatched")
                && event.fields.contains("version=unresolved")
        })
    })
    .await;

    let events: Vec<CapturedEvent> = sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let line = events[captured_before..]
        .iter()
        .find(|event| {
            event.fields.contains("message=cron dispatched")
                && event.fields.contains("version=unresolved")
        })
        .expect("a 'cron dispatched' event with version=unresolved");
    assert!(
        line.fields.contains("worker=\"gone-worker\" "),
        "the dispatch line must carry the worker name: {}",
        line.fields
    );
    assert!(
        line.fields.contains("status=404"),
        "the unresolvable dispatch must 404: {}",
        line.fields
    );

    // The reconciliation stops the job right after the dispatch.
    advance_until(Duration::from_millis(1000), || {
        scheduler.active_job_count() == 0
    })
    .await;

    scheduler.shutdown().await;
}

/// Without a root key the jobs still dispatch (the pipeline discards the
/// internal header) and the missing-root-key warning fires exactly once.
#[tokio::test]
async fn no_root_key_dispatches_without_auth_headers_and_warns_once() {
    tokio::time::pause();
    let sink = capture_sink();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", Some("POST"))]),
        false,
    );

    let captured_before = sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    let (mut scheduler, _app, metrics) = start_harness(index, requests.clone(), None).await;
    advance_until(Duration::from_millis(500), || requests.count() == 1).await;
    scheduler.shutdown().await;
    advance(Duration::from_millis(150)).await;

    assert_eq!(metrics.failures_total(), 0);
    let request = requests.first().expect("cron worker request");
    assert!(request
        .headers
        .iter()
        .all(|(name, _)| name != "authorization" && name != INTERNAL_REQUEST_HEADER));

    // Snapshot and release the capture lock before asserting.
    let events: Vec<CapturedEvent> = sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let warnings = events[captured_before..]
        .iter()
        .filter(|event| {
            event
                .fields
                .contains("cron jobs are enabled without ROOT_API_KEY")
        })
        .count();
    assert_eq!(
        warnings, 1,
        "the missing-root-key warning must fire once, got {warnings}"
    );
}

/// (Amendment 1) Every dispatch logs one `cron dispatched` info line with
/// the worker, the version the unversioned route resolves to at tick time,
/// the job path, the method, the `cron-<uuid>` request id and the response
/// status. After a promote, the logged version is the NEW one.
#[tokio::test]
async fn dispatch_log_reports_the_serving_version_after_promote() {
    tokio::time::pause();
    let sink = capture_sink();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let cron = || Some(vec![job("@every 100ms", "/tick", Some("POST"))]);

    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "log-worker",
        "1.0.0",
        cron(),
        false,
    );
    insert_version(
        &mut index,
        root.path(),
        "log-worker",
        "2.0.0",
        cron(),
        false,
    );

    let (mut scheduler, app, _metrics) =
        start_harness(index, requests.clone(), Some("root-secret")).await;
    // 1.0.0 serves first; the log lines before the swap report 1.0.0.
    promote(&app, "log-worker", "1.0.0").await;
    advance_until(Duration::from_millis(300), || {
        requests.count_for("1.0.0") >= 1
    })
    .await;

    // The promote to 2.0.0: the log line of the NEXT tick must report the
    // NEW version, correlated through the same cron-<uuid> request id.
    promote(&app, "log-worker", "2.0.0").await;

    // A tick served by 2.0.0; the request id correlates the log line.
    advance_until(Duration::from_millis(600), || {
        requests.count_for("2.0.0") >= 1
    })
    .await;
    let tick = requests.last_for("2.0.0").expect("a 2.0.0 tick");
    let request_id = &tick.request_id;
    assert!(request_id.starts_with("cron-"));

    let events: Vec<CapturedEvent> = sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();

    // Control: before the promote, the same line reported the OLD version.
    let before = requests.last_for("1.0.0").expect("a 1.0.0 tick");
    let before_line = events
        .iter()
        .find(|event| {
            event.fields.contains("message=cron dispatched")
                && event
                    .fields
                    .contains(&format!("request_id={}", before.request_id))
        })
        .expect("a pre-promote 'cron dispatched' event");
    assert!(
        before_line.fields.contains("version=1.0.0 "),
        "pre-promote line must report 1.0.0: {}",
        before_line.fields
    );

    let line = events
        .iter()
        .find(|event| {
            event.fields.contains("message=cron dispatched")
                && event.fields.contains(&format!("request_id={request_id}"))
        })
        .unwrap_or_else(|| {
            panic!("no 'cron dispatched' event for {request_id}; captured: {events:?}")
        });
    let expected_prefix = format!(
        "message=cron dispatched worker=\"log-worker\" version=2.0.0 path=\"/tick\" method=\"POST\" request_id={request_id} status=204 duration_ms="
    );
    let duration_ms = line
        .fields
        .strip_prefix(&expected_prefix)
        .unwrap_or_else(|| {
            panic!(
                "the dispatch line must carry worker, the NEW version, path, method, request_id, status and duration_ms; got: {}",
                line.fields
            )
        })
        .trim_end();
    assert!(
        !duration_ms.is_empty() && duration_ms.chars().all(|c| c.is_ascii_digit()),
        "duration_ms must be the dispatch duration in milliseconds: {duration_ms:?}"
    );

    scheduler.shutdown().await;
}

/// A spoofed internal header on a public request is not forwarded to the
/// worker (pipeline behavior, kept from the pre-EDG-26 suite).
#[tokio::test]
async fn spoofed_internal_header_is_not_forwarded_to_public_worker() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(&mut index, root.path(), "cron-worker", "1.0.0", None, false);

    let (_scheduler, app, _metrics) =
        start_harness(index, requests.clone(), Some("root-secret")).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/cron-worker")
                .header(INTERNAL_REQUEST_HEADER, "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let request = requests.first().expect("public worker request");
    assert!(request
        .headers
        .iter()
        .all(|(name, _)| name != INTERNAL_REQUEST_HEADER));
}

// ---------------------------------------------------------------------------
// Removed-task ownership and draining (Emenda 4 of the review): the
// scheduler keeps every task (active and draining), a normal removal never
// aborts an in-flight dispatch, reconciliation never waits on a removed
// task, and a shutdown joins (or aborts at the deadline) every task.
// ---------------------------------------------------------------------------

/// A removed job must finish its in-flight dispatch on its own (it is
/// never aborted at the 5 s shutdown timeout) and its task leaves
/// `draining` when the next reconciliation pass reaps it.
#[tokio::test]
async fn removed_job_in_flight_dispatch_finishes_after_removal() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "long-job",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let s = started.clone();
    let c = completed.clone();
    let app = Router::new().fallback(move || {
        let s = s.clone();
        let c = c.clone();
        async move {
            s.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(10)).await;
            c.fetch_add(1, Ordering::SeqCst);
            StatusCode::NO_CONTENT
        }
    });
    let mut scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("root-secret".into()))
            .with_reconcile_interval(Duration::from_millis(50)),
        index.clone(),
        app,
        CronMetrics::default(),
    )
    .await;
    advance_until(Duration::from_secs(1), || {
        started.load(Ordering::SeqCst) > 0
    })
    .await;
    assert_eq!(started.load(Ordering::SeqCst), 1);

    // The removal pass moves the job to draining WITHOUT waiting for the
    // in-flight dispatch.
    index
        .set_worker_enabled("long-job", Some("1.0.0"), false)
        .unwrap();
    advance(Duration::from_millis(100)).await;
    assert_eq!(scheduler.active_job_count(), 0);
    assert_eq!(scheduler.draining_job_count(), 1);

    // The dispatch finishes on its own (not aborted at 5 s).
    advance_until(Duration::from_secs(12), || {
        completed.load(Ordering::SeqCst) > 0
    })
    .await;
    assert_eq!(
        completed.load(Ordering::SeqCst),
        1,
        "removed job must finish its in-flight dispatch"
    );

    // The drained task is reaped by a later reconciliation pass.
    advance_until(Duration::from_secs(1), || {
        scheduler.draining_job_count() == 0
    })
    .await;
    assert_eq!(scheduler.draining_job_count(), 0);

    scheduler.shutdown().await;
}

/// A removed job with a slow in-flight dispatch must not block the
/// reconciliation of other workers: the new job is created in the same
/// pass (within one reconcile interval), while the old one drains.
#[tokio::test]
async fn removed_job_does_not_block_other_workers_reconcile() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "old-job",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );
    let started = Arc::new(AtomicUsize::new(0));
    let s = started.clone();
    let app = Router::new().fallback(move || {
        let s = s.clone();
        async move {
            s.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(10)).await;
            StatusCode::NO_CONTENT
        }
    });
    let mut scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("root-secret".into()))
            .with_reconcile_interval(Duration::from_millis(50)),
        index.clone(),
        app,
        CronMetrics::default(),
    )
    .await;
    advance_until(Duration::from_secs(1), || {
        started.load(Ordering::SeqCst) > 0
    })
    .await;
    assert_eq!(started.load(Ordering::SeqCst), 1);

    // Disable the old worker (in-flight dispatch) and install a new one in
    // the same window.
    index
        .set_worker_enabled("old-job", Some("1.0.0"), false)
        .unwrap();
    insert_version(
        &mut index,
        root.path(),
        "new-job",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );
    advance(Duration::from_millis(200)).await;
    assert_eq!(
        scheduler.active_job_count(),
        1,
        "new job must start without waiting for the removed job"
    );
    assert_eq!(scheduler.draining_job_count(), 1);

    // Let the old dispatch finish and be reaped, then shut down cleanly.
    advance_until(Duration::from_secs(15), || {
        scheduler.draining_job_count() == 0
    })
    .await;
    assert_eq!(scheduler.draining_job_count(), 0);
    scheduler.shutdown().await;
}

/// A global shutdown must not leave a removed job's dispatch detached:
/// every task is joined (or aborted at the shared deadline) before
/// `shutdown` returns, so nothing can complete after it.
#[tokio::test]
async fn shutdown_does_not_leave_removed_jobs_dispatches_detached() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    for name in ["first-job", "second-job"] {
        insert_version(
            &mut index,
            root.path(),
            name,
            "1.0.0",
            Some(vec![job("@every 100ms", "/tick", None)]),
            false,
        );
    }
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let s = started.clone();
    let c = completed.clone();
    let app = Router::new().fallback(move || {
        let s = s.clone();
        let c = c.clone();
        async move {
            s.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(20)).await;
            c.fetch_add(1, Ordering::SeqCst);
            StatusCode::NO_CONTENT
        }
    });
    let mut scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("root-secret".into()))
            .with_reconcile_interval(Duration::from_millis(50)),
        index.clone(),
        app,
        CronMetrics::default(),
    )
    .await;
    advance_until(Duration::from_secs(1), || {
        started.load(Ordering::SeqCst) == 2
    })
    .await;
    assert_eq!(started.load(Ordering::SeqCst), 2);

    // Remove both workers while both dispatches are in flight.
    for name in ["first-job", "second-job"] {
        index
            .set_worker_enabled(name, Some("1.0.0"), false)
            .unwrap();
    }
    advance(Duration::from_millis(200)).await;
    assert_eq!(scheduler.active_job_count(), 0);
    assert_eq!(scheduler.draining_job_count(), 2);

    // The dispatches (20 s) outlive the shutdown deadline (5 s): they are
    // aborted and joined, so nothing can complete after `shutdown` returns.
    scheduler.shutdown().await;
    let at_shutdown = completed.load(Ordering::SeqCst);
    advance(Duration::from_secs(25)).await;
    assert_eq!(
        completed.load(Ordering::SeqCst),
        at_shutdown,
        "a dispatch completed after the scheduler shutdown returned"
    );
    assert_eq!(
        at_shutdown, 0,
        "both dispatches passed the shutdown deadline"
    );
}

/// A fast re-add of the same job key must not overlap: while the old task
/// is still draining its in-flight dispatch, the new task waits for it to
/// finish before its first dispatch (identity exclusion).
#[tokio::test]
async fn fast_readd_of_same_key_does_not_overlap() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "readd-job",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let overlapped = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);
    let calls_c = calls.clone();
    let in_flight_c = in_flight.clone();
    let overlapped_c = overlapped.clone();
    let release = release_rx.clone();
    let app = Router::new().fallback(move || {
        let calls = calls_c.clone();
        let in_flight = in_flight_c.clone();
        let overlapped = overlapped_c.clone();
        let mut release = release.clone();
        async move {
            in_flight.fetch_add(1, Ordering::SeqCst);
            if in_flight.load(Ordering::SeqCst) >= 2 {
                overlapped.store(true, Ordering::SeqCst);
            }
            calls.fetch_add(1, Ordering::SeqCst);
            // The first call (the old task's in-flight dispatch) is held
            // until the test releases it; later calls return at once.
            if calls.load(Ordering::SeqCst) == 1 {
                let _ = release.changed().await;
            }
            in_flight.fetch_sub(1, Ordering::SeqCst);
            StatusCode::NO_CONTENT
        }
    });
    let mut scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("root-secret".into()))
            .with_reconcile_interval(Duration::from_millis(50)),
        index.clone(),
        app,
        CronMetrics::default(),
    )
    .await;
    advance_until(Duration::from_secs(1), || calls.load(Ordering::SeqCst) > 0).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Remove and re-add the job between two reconciliation passes; the old
    // dispatch stays held, so the old task is still draining when the new
    // one starts.
    index
        .set_worker_enabled("readd-job", Some("1.0.0"), false)
        .unwrap();
    advance(Duration::from_millis(60)).await;
    assert_eq!(scheduler.draining_job_count(), 1);
    index
        .set_worker_enabled("readd-job", Some("1.0.0"), true)
        .unwrap();
    advance(Duration::from_millis(60)).await;
    assert_eq!(scheduler.active_job_count(), 1);

    // The new task must not dispatch while the old one is still draining.
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the new task dispatched while the old one was still draining"
    );

    // Give a non-waiting (defective) new task time to tick (~+300ms) while
    // the old dispatch is still held, then release it.
    advance(Duration::from_millis(80)).await;
    release_tx.send(true).unwrap();
    advance_until(Duration::from_secs(1), || calls.load(Ordering::SeqCst) > 1).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        !overlapped.load(Ordering::SeqCst),
        "the same key had two dispatches in flight at once"
    );

    // The old task is reaped; shut down cleanly.
    advance_until(Duration::from_secs(1), || {
        scheduler.draining_job_count() == 0
    })
    .await;
    scheduler.shutdown().await;
}

// ---------------------------------------------------------------------------
// Repeated removal (review probes, now permanent): every live generation of
// a key must stay tracked, so a second remove can never detach the original
// in-flight dispatch from the supervisor's ownership. These tests use a
// root key (they assert lifecycle only, not the missing-key warning) so they
// do not add emitters to the shared capture sink that
// `no_root_key_dispatches_without_auth_headers_and_warns_once` counts.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn review_second_remove_readd_preserves_exclusion() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "churn-job",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let overlap = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);
    let n = calls.clone();
    let c = current.clone();
    let o = overlap.clone();
    let app = Router::new().fallback(move || {
        let n = n.clone();
        let c = c.clone();
        let o = o.clone();
        let mut r = release_rx.clone();
        async move {
            // A second in-flight dispatch of the same identity is overlap.
            if c.fetch_add(1, Ordering::SeqCst) > 0 {
                o.store(true, Ordering::SeqCst);
            }
            let ordinal = n.fetch_add(1, Ordering::SeqCst);
            // The first call (the original in-flight dispatch) is held until
            // the test releases it.
            if ordinal == 0 {
                let _ = r.changed().await;
            }
            c.fetch_sub(1, Ordering::SeqCst);
            StatusCode::NO_CONTENT
        }
    });
    let mut scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("root-secret".into())).with_reconcile_interval(RECONCILE),
        index.clone(),
        app,
        CronMetrics::default(),
    )
    .await;
    advance_until(Duration::from_secs(1), || calls.load(Ordering::SeqCst) == 1).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Two remove/re-add cycles while the original dispatch is still held:
    // every generation of the key must stay tracked, so a new generation
    // never dispatches while an older one is still in flight.
    for _ in 0..2 {
        index
            .set_worker_enabled("churn-job", Some("1.0.0"), false)
            .unwrap();
        advance(Duration::from_millis(100)).await;
        index
            .set_worker_enabled("churn-job", Some("1.0.0"), true)
            .unwrap();
        advance(Duration::from_millis(200)).await;
    }

    // Release the original dispatch and let the waiting generations tick.
    release_tx.send(true).unwrap();
    advance(Duration::from_millis(200)).await;
    scheduler.shutdown().await;
    assert!(
        !overlap.load(Ordering::SeqCst),
        "second remove/re-add overlapped the original draining dispatch"
    );
}

#[tokio::test]
async fn review_second_remove_preserves_shutdown_ownership() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "churn-shutdown",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    let c = completed.clone();
    let app = Router::new().fallback(move || {
        let n = n.clone();
        let c = c.clone();
        async move {
            n.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(20)).await;
            c.fetch_add(1, Ordering::SeqCst);
            StatusCode::NO_CONTENT
        }
    });
    let mut scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("root-secret".into())).with_reconcile_interval(RECONCILE),
        index.clone(),
        app,
        CronMetrics::default(),
    )
    .await;
    advance_until(Duration::from_secs(1), || calls.load(Ordering::SeqCst) == 1).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Remove, re-add, remove again: the original (still draining) must stay
    // owned by the supervisor across both generations.
    index
        .set_worker_enabled("churn-shutdown", Some("1.0.0"), false)
        .unwrap();
    advance(Duration::from_millis(100)).await;
    index
        .set_worker_enabled("churn-shutdown", Some("1.0.0"), true)
        .unwrap();
    advance(Duration::from_millis(100)).await;
    index
        .set_worker_enabled("churn-shutdown", Some("1.0.0"), false)
        .unwrap();
    advance(Duration::from_millis(100)).await;

    scheduler.shutdown().await;
    let after = completed.load(Ordering::SeqCst);
    advance(Duration::from_secs(25)).await;
    assert_eq!(
        completed.load(Ordering::SeqCst),
        after,
        "original draining dispatch completed after shutdown following second remove"
    );
}

// ---------------------------------------------------------------------------
// Shutdown while a reconcile admission is live: no dispatch may complete
// after `shutdown` returns, and the published counters end at zero. Uses a
// root key so it does not add a missing-key warning emitter to the shared
// capture sink.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_during_active_reconcile_leaves_no_dispatch_and_zero_counters() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "reconcile-job",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );
    let started = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let s = started.clone();
    let c = completed.clone();
    let app = Router::new().fallback(move || {
        let s = s.clone();
        let c = c.clone();
        async move {
            s.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(20)).await;
            c.fetch_add(1, Ordering::SeqCst);
            StatusCode::NO_CONTENT
        }
    });
    let mut scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("root-secret".into())).with_reconcile_interval(RECONCILE),
        index.clone(),
        app,
        CronMetrics::default(),
    )
    .await;

    // The job is admitted and its (20 s) dispatch is in flight.
    advance_until(Duration::from_secs(1), || {
        started.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(scheduler.active_job_count(), 1);

    // Shutdown while the supervisor is actively managing the job.
    scheduler.shutdown().await;
    assert_eq!(scheduler.active_job_count(), 0);
    assert_eq!(scheduler.draining_job_count(), 0);
    let at_shutdown = completed.load(Ordering::SeqCst);

    // Nothing may complete after `shutdown` returned: the dispatch (20 s)
    // outlived the shutdown deadline (5 s) and was aborted.
    advance(Duration::from_secs(25)).await;
    assert_eq!(
        completed.load(Ordering::SeqCst),
        at_shutdown,
        "a dispatch completed after shutdown returned during a reconcile"
    );
    assert_eq!(
        at_shutdown, 0,
        "the in-flight dispatch passed the shutdown deadline"
    );
}

// ---------------------------------------------------------------------------
// Shutdown with a poll that does not yield to cancellation (Emenda 6):
// the `JoinSet` is the completion confirmation, the counters change only
// through the actual reaping, and a task that does not yield by the
// deadlines is reported instead of being declared stopped. Uses a root key
// so it does not add a missing-key warning emitter to the shared capture
// sink. Real clock and multi-thread runtime: the handler blocks a worker
// thread in a synchronous `Condvar` wait, which the cancellation cannot
// interrupt.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_reports_incomplete_when_a_child_does_not_yield() {
    let sink = capture_sink();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    insert_version(
        &mut index,
        root.path(),
        "blocked-poll",
        "1.0.0",
        Some(vec![job("@every 10ms", "/tick", None)]),
        false,
    );
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let alive = Arc::new(AtomicBool::new(false));
    let started = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(tokio::sync::Notify::new());
    let g = gate.clone();
    let a = alive.clone();
    let s = started.clone();
    let c = completed.clone();
    let app = Router::new().fallback(move || {
        let g = g.clone();
        let a = a.clone();
        let s = s.clone();
        let c = c.clone();
        async move {
            a.store(true, Ordering::SeqCst);
            s.notify_one();
            let (lock, cv) = &*g;
            let guard = lock.lock().unwrap();
            let _guard = cv
                .wait_timeout_while(guard, Duration::from_secs(15), |released| !*released)
                .unwrap();
            a.store(false, Ordering::SeqCst);
            c.notify_one();
            StatusCode::NO_CONTENT
        }
    });
    let captured_before = sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    let mut scheduler = CronScheduler::start(
        CronSchedulerConfig::new(Some("test".into()))
            .with_reconcile_interval(Duration::from_millis(50))
            .with_shutdown_grace(Duration::from_millis(100))
            .with_shutdown_abort_wait(Duration::from_millis(100)),
        index,
        app,
        CronMetrics::default(),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();

    // Grace (100 ms) + abort-wait (100 ms) run out with the handler still
    // blocked in its condvar wait: `shutdown` must return without declaring
    // the task stopped (no `store(0)`).
    scheduler.shutdown().await;
    let still_alive = alive.load(Ordering::SeqCst);
    let counts = (scheduler.active_job_count(), scheduler.draining_job_count());
    assert!(
        still_alive,
        "the blocked handler must still be running after shutdown returned"
    );
    assert!(
        counts.0 + counts.1 >= 1,
        "shutdown returned with a live task but the counters read zero: {counts:?}"
    );

    // The warning names the shortfall instead of claiming a clean stop.
    let events: Vec<CapturedEvent> = sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let incomplete = events[captured_before..].iter().any(|event| {
        event.fields.contains("message=cron shutdown incomplete")
            && event.fields.contains("remaining=1")
    });
    assert!(
        incomplete,
        "expected a `cron shutdown incomplete` warning with remaining=1; captured: {:?}",
        events[captured_before..]
            .iter()
            .map(|event| event.fields.as_str())
            .collect::<Vec<_>>()
    );

    // Release the handler and await its end, so no thread is left behind.
    let (lock, cv) = &*gate;
    *lock.lock().unwrap() = true;
    cv.notify_all();
    tokio::time::timeout(Duration::from_secs(2), completed.notified())
        .await
        .unwrap();
    assert!(
        !alive.load(Ordering::SeqCst),
        "the handler must be finished before the test ends"
    );
}

// ---------------------------------------------------------------------------
// (EDG-34) Visibility: the published gauges, the tasks' next-run instants,
// the draining visibility in the snapshot and the job-set change log lines.
// ---------------------------------------------------------------------------

/// Read `/metrics` through the pipeline (the root key passes the
/// `observability:read` gate) and return the body.
async fn fetch_metrics_text(app: &Router, root_key: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("authorization", format!("Bearer {root_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The next-run published for the first (only) tracked job.
fn first_next_run(metrics: &CronMetrics) -> Option<DateTime<Utc>> {
    metrics.jobs().jobs.first().and_then(|job| job.next_run_at)
}

/// (EDG-34) The published gauges and the snapshot follow the job through
/// registration, a removal with an in-flight dispatch (draining) and the
/// reaping: the scheduler counters, the shared `CronMetrics` and the
/// snapshot agree at every boundary, and `/metrics` renders the same
/// values.
#[tokio::test]
async fn gauges_and_snapshot_follow_register_removal_and_reaping() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let in_flight = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "gauge-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );

    // The dispatch (300 ms) outlives the schedule (100 ms), so the removal
    // can land with a dispatch in flight.
    let (mut scheduler, app, metrics) = start_harness_slow(
        index.clone(),
        requests.clone(),
        Duration::from_millis(300),
        in_flight,
        peak,
        current,
        Some("root-secret"),
    )
    .await;

    // Registered: one active job, none draining — the scheduler counters,
    // the shared metrics and the snapshot agree, and `/metrics` renders
    // them.
    advance(Duration::from_millis(50)).await;
    assert_eq!(scheduler.active_job_count(), 1);
    assert_eq!(scheduler.draining_job_count(), 0);
    assert_eq!(metrics.active_jobs(), 1);
    assert_eq!(metrics.draining_jobs(), 0);
    let snapshot = metrics.jobs();
    assert_eq!(snapshot.jobs.len(), 1);
    assert_eq!(snapshot.jobs[0].worker, "gauge-worker");
    assert_eq!(snapshot.jobs[0].state, CronJobState::Active);
    let text = fetch_metrics_text(&app, "root-secret").await;
    assert!(
        text.contains("edger_cron_jobs{state=\"active\"} 1"),
        "{text}"
    );
    assert!(
        text.contains("edger_cron_jobs{state=\"draining\"} 0"),
        "{text}"
    );

    // The first tick fires at +100 and its (300 ms) dispatch runs until
    // +400: the delete at +160 lands with the dispatch in flight, and the
    // job drains.
    advance(Duration::from_millis(160)).await;
    index.remove_worker("gauge-worker", "1.0.0").unwrap();
    advance_until(Duration::from_millis(300), || {
        scheduler.active_job_count() == 0 && scheduler.draining_job_count() == 1
    })
    .await;

    // While draining: the snapshot says the same, with `nextRunAt` null.
    let snapshot = metrics.jobs();
    assert_eq!(snapshot.jobs.len(), 1);
    assert_eq!(snapshot.jobs[0].worker, "gauge-worker");
    assert_eq!(snapshot.jobs[0].state, CronJobState::Draining);
    assert!(
        snapshot.jobs[0].next_run_at.is_none(),
        "a draining job has no next run"
    );
    assert_eq!(metrics.active_jobs(), 0);
    assert_eq!(metrics.draining_jobs(), 1);
    let text = fetch_metrics_text(&app, "root-secret").await;
    assert!(
        text.contains("edger_cron_jobs{state=\"active\"} 0"),
        "{text}"
    );
    assert!(
        text.contains("edger_cron_jobs{state=\"draining\"} 1"),
        "{text}"
    );

    // The in-flight dispatch finishes on its own (+400) and the reaping
    // empties the set: the job leaves the snapshot and the gauges zero.
    advance_until(Duration::from_millis(600), || {
        requests.count() >= 1 && scheduler.draining_job_count() == 0
    })
    .await;
    assert_eq!(scheduler.active_job_count(), 0);
    assert_eq!(metrics.active_jobs(), 0);
    assert_eq!(metrics.draining_jobs(), 0);
    assert!(
        metrics.jobs().jobs.is_empty(),
        "reaped job leaves the snapshot"
    );
    let text = fetch_metrics_text(&app, "root-secret").await;
    assert!(
        text.contains("edger_cron_jobs{state=\"active\"} 0"),
        "{text}"
    );
    assert!(
        text.contains("edger_cron_jobs{state=\"draining\"} 0"),
        "{text}"
    );

    scheduler.shutdown().await;
}

/// (EDG-34) For `@every`, `nextRunAt` is the instant the TASK will wait
/// for — the sleep deadline the task itself publishes: after a dispatch
/// slower than the interval, the next run is (dispatch end) + interval,
/// not `now + interval` recomputed at query time and not a slot grid.
#[tokio::test]
async fn next_run_is_the_task_sleep_deadline_after_a_slow_dispatch() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let in_flight = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "nextrun-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );

    // Interval 100 ms; the dispatch takes 300 ms (slower than the
    // interval). The injected wall follows the (paused) monotonic clock,
    // so the published deadline → UTC conversion is deterministic.
    let wall = Arc::new(TestWall::new());
    let (mut scheduler, _app, metrics) = start_harness_factory_at_wall(
        index,
        Arc::new(SlowFactory {
            requests: requests.clone(),
            delay: Duration::from_millis(300),
            in_flight,
            peak,
            current,
        }),
        RECONCILE,
        Some("root-secret"),
        test_wall(&wall),
    )
    .await;

    // Reference point for the virtual time (the task's first publish
    // happens at the scheduler start, virtual 0).
    let v0 = tokio::time::Instant::now();

    // Let the job task register its first sleep: the snapshot carries the
    // first deadline (start + interval — the instant the task will wait
    // for, not a wall guess).
    advance_until(Duration::from_millis(50), || {
        first_next_run(&metrics).is_some()
    })
    .await;
    let first = first_next_run(&metrics).expect("first next run");
    assert_eq!(requests.count(), 0, "no tick may have fired yet");
    // The first deadline is exactly (scheduler start) + interval, so the
    // start (the wall anchor) is recovered from it without drift.
    let interval = chrono::Duration::from_std(Duration::from_millis(100)).unwrap();
    let anchor = first - interval;

    // The value is the task's own: it does not drift while the task keeps
    // sleeping (a `now + interval` recomputed at query time would move on
    // every read — the virtual clock stays paused, so no tick fires).
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        first_next_run(&metrics).expect("second read"),
        first,
        "the published next run must be stable while the task sleeps"
    );
    // And the deadline is still ahead (the tick fires at it, not before) —
    // ahead of the CURRENT civil instant from the injected wall, not a
    // boot-time anchor.
    assert!(
        first > wall.now(),
        "the first deadline must still be in the future: {first}"
    );

    // The first tick fires at the first deadline and the (300 ms)
    // dispatch runs; only when it ends does the task compute the next
    // deadline.
    advance_until(Duration::from_millis(600), || requests.count() == 1).await;
    let (v_loop, stamp_elapsed) = {
        let stamps = requests.stamps.lock().unwrap();
        (tokio::time::Instant::now() - v0, stamps[0].elapsed())
    };
    // The virtual instant of the dispatch end (the stamp is the end of
    // the isolate's run).
    let dispatch_end = v_loop - stamp_elapsed;
    advance(Duration::from_millis(50)).await;
    let second = first_next_run(&metrics).expect("second next run");

    // The next deadline must be the task's own re-sleep instant: right
    // after the dispatch end, plus the interval. A `now + interval`
    // recomputed at query time (read at ≈ dispatch end + 50 ms) or a
    // slot grid (a flat +interval over the first deadline) would not sit
    // here.
    let lower = anchor + dispatch_end + interval;
    let upper = lower + chrono::Duration::milliseconds(30);
    assert!(
        second >= lower && second <= upper,
        "next run {second} must be the dispatch end ({dispatch_end:?}) + the interval, got window [{lower}, {upper}]"
    );
    // And it moved forward by at least the dispatch duration plus the
    // interval (the `@every` job does not re-slot on a grid).
    assert!(
        second - first >= chrono::Duration::from_std(Duration::from_millis(400)).unwrap(),
        "the next run must move by the real dispatch span plus the interval: {first} -> {second}"
    );
    // Exactly one tick fired so far (the next one is the second deadline).
    assert_eq!(requests.count(), 1);

    scheduler.shutdown().await;
}

/// The next minute boundary strictly after `t` (the `* * * * *` fire:
/// the `cron` crate fires at second 0 of every matching minute).
fn next_whole_minute(t: DateTime<Utc>) -> DateTime<Utc> {
    let micros = (t.nanosecond() as i64) / 1_000;
    t - chrono::Duration::seconds(t.second() as i64) - chrono::Duration::microseconds(micros)
        + chrono::Duration::minutes(1)
}

/// (EDG-34) For a wall-clock cron, `nextRunAt` is the next matching
/// instant of the expression — the same computation the task's delay
/// derives from (the task commits the MONOTONIC deadline it will wait
/// for; the conversion to UTC happens at publish time with the current
/// wall↔monotonic relationship).
#[tokio::test]
async fn next_run_matches_the_wall_clock_expression() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "wallclock-worker",
        "1.0.0",
        Some(vec![job("* * * * *", "/tick", None)]),
        false,
    );

    // The task reads its civil instant from the injected wall.
    let wall = Arc::new(TestWall::new());
    let before = wall.now();
    let (mut scheduler, _app, metrics) = start_harness_factory_at_wall(
        index,
        Arc::new(RecordingFactory {
            requests: requests.clone(),
        }),
        RECONCILE,
        Some("root-secret"),
        test_wall(&wall),
    )
    .await;
    advance_until(Duration::from_millis(50), || {
        first_next_run(&metrics).is_some()
    })
    .await;
    let at = first_next_run(&metrics).expect("the next run of the wall clock job");
    let after = wall.now();

    // The task computed `next_after` from the injected wall inside
    // [before, after]: the published instant is the next full minute from
    // that point.
    let expected = [next_whole_minute(before), next_whole_minute(after)];
    assert!(
        expected.contains(&at),
        "next run {at} is not the next full minute from before {before} / after {after}"
    );

    scheduler.shutdown().await;
}

/// The captured `info` events with the given message for the given worker
/// since `offset`.
fn captured_change_lines(
    sink: &Arc<Mutex<Vec<CapturedEvent>>>,
    offset: usize,
    message: &str,
    worker: &str,
) -> Vec<String> {
    let events = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    events[offset..]
        .iter()
        .filter(|event| {
            // The worker field is logged with Debug escaping (`?worker`,
            // EDG-34 emenda 2): `worker="name"` in the rendered line.
            event.fields.contains(&format!("message={message}"))
                && event.fields.contains(&format!("worker=\"{worker}\""))
        })
        .map(|event| event.fields.clone())
        .collect()
}

/// (EDG-34) The job-set change lines are `info` and fire once per change:
/// `cron job started` on registration, `cron job stopped` on the removal
/// (the drain start), `cron job drained` on the reaping — each with
/// worker/schedule/path/method and the resolved version (`unresolved` once
/// the worker no longer resolves) — and an unchanged reconciliation writes
/// none of them.
#[tokio::test]
async fn job_change_lines_are_info_once_per_change() {
    tokio::time::pause();
    let sink = capture_sink();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let in_flight = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "chg-worker",
        "1.0.0",
        Some(vec![job("@every 400ms", "/tick", None)]),
        false,
    );

    let captured_before = sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    let (mut scheduler, _app, _metrics) = start_harness_slow(
        index.clone(),
        requests.clone(),
        Duration::from_millis(100),
        in_flight,
        peak,
        current,
        Some("root-secret"),
    )
    .await;

    // Registration (the boot reconcile) logs `started` exactly once, with
    // the resolved version.
    advance_until(Duration::from_millis(200), || {
        captured_change_lines(&sink, captured_before, "cron job started", "chg-worker").len() == 1
    })
    .await;
    let started =
        &captured_change_lines(&sink, captured_before, "cron job started", "chg-worker")[0];
    assert!(started.contains("schedule=\"@every 400ms\""), "{started}");
    assert!(started.contains("path=\"/tick\""), "{started}");
    assert!(started.contains("method=\"GET\""), "{started}");
    assert!(started.contains("version=1.0.0"), "{started}");

    // Two unchanged reconciles (50 ms each) write none of the change
    // lines.
    advance(Duration::from_millis(120)).await;
    assert_eq!(
        captured_change_lines(&sink, captured_before, "cron job started", "chg-worker").len(),
        1
    );
    assert!(
        captured_change_lines(&sink, captured_before, "cron job stopped", "chg-worker").is_empty()
    );
    assert!(
        captured_change_lines(&sink, captured_before, "cron job drained", "chg-worker").is_empty()
    );

    // The first tick fires at +400 and its (100 ms) dispatch runs until
    // +500; the disable at +450 lands with the dispatch in flight and
    // starts the drain (`stopped`, once).
    advance(Duration::from_millis(450)).await;
    index
        .set_worker_enabled("chg-worker", Some("1.0.0"), false)
        .unwrap();
    advance_until(Duration::from_millis(800), || {
        captured_change_lines(&sink, captured_before, "cron job stopped", "chg-worker").len() == 1
    })
    .await;
    let stopped =
        &captured_change_lines(&sink, captured_before, "cron job stopped", "chg-worker")[0];
    assert!(stopped.contains("schedule=\"@every 400ms\""), "{stopped}");
    assert!(stopped.contains("path=\"/tick\""), "{stopped}");
    assert!(stopped.contains("method=\"GET\""), "{stopped}");
    // The worker no longer resolves (its only version is disabled).
    assert!(stopped.contains("version=unresolved"), "{stopped}");

    // The in-flight dispatch finishes (+500); the reaping logs `drained`
    // once and the counters empty.
    advance_until(Duration::from_millis(1000), || {
        captured_change_lines(&sink, captured_before, "cron job drained", "chg-worker").len() == 1
            && scheduler.draining_job_count() == 0
    })
    .await;
    let drained =
        &captured_change_lines(&sink, captured_before, "cron job drained", "chg-worker")[0];
    assert!(drained.contains("schedule=\"@every 400ms\""), "{drained}");
    assert!(drained.contains("path=\"/tick\""), "{drained}");
    assert!(drained.contains("method=\"GET\""), "{drained}");
    assert!(drained.contains("version=unresolved"), "{drained}");

    // Two more unchanged reconciles: still exactly one line per change.
    advance(Duration::from_millis(120)).await;
    assert_eq!(
        captured_change_lines(&sink, captured_before, "cron job started", "chg-worker").len(),
        1
    );
    assert_eq!(
        captured_change_lines(&sink, captured_before, "cron job stopped", "chg-worker").len(),
        1
    );
    assert_eq!(
        captured_change_lines(&sink, captured_before, "cron job drained", "chg-worker").len(),
        1
    );

    scheduler.shutdown().await;
}

/// (EDG-34) More jobs than the task→supervisor signal channel holds, all
/// registered in the same reconciliation: at the first publish every task
/// signals its deadline, the channel (capacity `JOB_EVENTS_CAPACITY`)
/// fills and `try_send` drops the overflow — and the same happens again
/// after the tick. The deadline VALUE, however, lives in each task's own
/// `watch` cell (the supervisor reads it when it builds the snapshot), so
/// the published list stays complete and a job's published deadline only
/// ever moves FORWARD — a deadline the task has already replaced never
/// comes back.
#[tokio::test]
async fn full_signal_queue_keeps_the_published_deadlines_correct() {
    tokio::time::pause();
    const JOBS: usize = 100;
    // The flood condition: more simultaneous senders than the bounded
    // signal channel holds — without this, the test would not exercise a
    // full queue.
    const {
        assert!(
            JOBS > JOB_EVENTS_CAPACITY,
            "the test needs more jobs than the signal channel holds"
        );
    }
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let in_flight = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    // One cron job per worker, the same `@every` schedule: every task
    // starts in the same reconciliation and fires on the same virtual
    // step, and each dispatch runs on its own isolate (no serialization).
    for i in 0..JOBS {
        insert_version(
            &mut index,
            root.path(),
            &format!("flood-w{i:03}"),
            "1.0.0",
            Some(vec![job("@every 100ms", "/tick", None)]),
            false,
        );
    }

    let (mut scheduler, _app, metrics) = start_harness_slow(
        index,
        requests.clone(),
        Duration::from_millis(0),
        in_flight,
        peak,
        current,
        Some("root-secret"),
    )
    .await;

    // Let every spawned task run its first loop (registering its first
    // deadline at the scheduler start) before the test advances the
    // clock: without this, the first `advance` step would jump the clock
    // while some tasks were still unrun, and their first deadlines would
    // no longer be whole multiples of the interval.
    tokio::task::yield_now().await;

    // First publish: every job carries its first deadline (start +
    // interval); the channel is already full of the D1 signals (more
    // senders than `JOB_EVENTS_CAPACITY`), the overflow dropped.
    advance_until(Duration::from_millis(50), || {
        metrics
            .jobs()
            .jobs
            .iter()
            .all(|job| job.next_run_at.is_some())
    })
    .await;
    let jobs = metrics.jobs().jobs;
    assert_eq!(
        jobs.len(),
        JOBS,
        "a full queue never loses a job from the list"
    );
    let d1 = jobs[0]
        .next_run_at
        .expect("every job carries its first deadline");
    assert!(
        jobs.iter().all(|job| job.next_run_at == Some(d1)),
        "every task sleeps until the same first deadline"
    );

    // The tick and everything after: sample the published snapshot at
    // every step. A job's published deadline may ADVANCE (its dispatch
    // ends and it re-sleeps) but must never fall back to a deadline the
    // task has already replaced — exactly what a supervisor-side
    // (recomputed or signal-fed) value could show after a dropped
    // signal. (The jobs are keyed here by worker; the path is fixed.)
    let mut last: BTreeMap<String, DateTime<Utc>> =
        jobs.iter().map(|job| (job.worker.clone(), d1)).collect();
    let mut advanced = Duration::from_millis(0);
    loop {
        tokio::time::advance(Duration::from_millis(10)).await;
        advanced += Duration::from_millis(10);
        let jobs = metrics.jobs().jobs;
        for job in &jobs {
            let Some(at) = job.next_run_at else {
                continue; // draining (not expected here) or before its first sleep
            };
            // (emenda 2) while the job's dispatch is in flight the
            // published value is the CURRENT instant (the committed
            // deadline already fired and the conversion saturates) — it
            // still only moves forward.
            let prev = last
                .get(&job.worker)
                .expect("every job was tracked since the start");
            assert!(
                at >= *prev,
                "job {} published {at} after {prev}: a replaced deadline came back",
                job.worker
            );
            last.insert(job.worker.clone(), at);
        }
        if jobs.iter().all(|job| job.next_run_at > Some(d1)) {
            break;
        }
        if advanced >= Duration::from_millis(3_000) {
            panic!(
                "a job never re-slept past its first deadline after {advanced:?} of virtual time"
            );
        }
    }

    // Every job finished its first dispatch and re-slept: the published
    // value is the task's own fresh deadline, never the replaced D1. (The
    // loop above can stop while a job's dispatch is still in flight —
    // the saturated current-wall value already moved past D1 — so drain
    // the remaining dispatches first.)
    let mut drained = Duration::from_millis(0);
    while requests.count() < JOBS {
        if drained >= Duration::from_secs(2) {
            panic!(
                "{}/{} dispatches done after an extra {drained:?} of virtual time",
                requests.count(),
                JOBS
            );
        }
        tokio::time::advance(Duration::from_millis(10)).await;
        drained += Duration::from_millis(10);
    }
    let jobs = metrics.jobs().jobs;
    assert!(
        jobs.iter().all(|job| job.next_run_at > Some(d1)),
        "every deadline is the task's own (newer than the replaced one)"
    );
    assert!(
        jobs.iter().all(|job| job.state == CronJobState::Active),
        "firing jobs stay active"
    );
    assert!(requests.count() >= JOBS, "at least one tick per job");
    assert_eq!(scheduler.active_job_count(), JOBS);
    assert_eq!(scheduler.draining_job_count(), 0);

    scheduler.shutdown().await;
}

// ---------------------------------------------------------------------------
// (EDG-34 emenda 2) The reviewer probes, now permanent: the draining
// visibility of a removed worker, the snapshot during the shutdown
// transitions, the `enabled` read-out lifecycle, the civil wall step and
// the log escaping.
// ---------------------------------------------------------------------------

/// Read `GET /api/admin/cron/jobs` through the pipeline with the root key
/// and return (status, json, text).
async fn admin_cron_jobs(app: &Router, root_key: &str) -> (StatusCode, serde_json::Value, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/cron/jobs")
                .header("authorization", format!("Bearer {root_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json, text)
}

/// (EDG-34 emenda 2) Without the Bearer root the scheduler still starts
/// (the jobs dispatch, minus the internal header), but the read-out must
/// be `false`: `enabled` is `true` only when the scheduler will actually
/// dispatch — started AND with the root Bearer configured.
#[tokio::test]
async fn missing_root_reports_disabled() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "enabled-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );

    let (mut scheduler, _app, metrics) =
        start_harness(index, RecordedRequests::default(), None).await;
    assert!(
        !metrics.scheduler_enabled(),
        "the read-out must be false without a root Bearer"
    );
    // The job still runs (the pre-existing behavior is untouched).
    advance_until(Duration::from_millis(200), || {
        scheduler.active_job_count() == 1
    })
    .await;

    scheduler.shutdown().await;
}

/// (EDG-34 emenda 2) `enabled` is `true` while the scheduler runs and
/// goes back to `false` when the supervisor ends — here by shutdown (the
/// same drop guard covers abort and panic): the endpoint and the shared
/// metrics both report the scheduler as off.
#[tokio::test]
async fn shutdown_disables_enabled_flag() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "enabled-worker",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );

    let (mut scheduler, app, metrics) =
        start_harness(index, RecordedRequests::default(), Some("root-secret")).await;
    assert!(
        metrics.scheduler_enabled(),
        "a started scheduler with a root Bearer is enabled"
    );
    let (_, before, text) = admin_cron_jobs(&app, "root-secret").await;
    assert_eq!(before["enabled"], true, "{text}");

    scheduler.shutdown().await;
    assert!(
        !metrics.scheduler_enabled(),
        "the read-out must be false once the supervisor ends"
    );
    let (status, after, text) = admin_cron_jobs(&app, "root-secret").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        after["enabled"], false,
        "the endpoint must report the scheduler off: {text}"
    );
    assert!(after["jobs"].as_array().expect("jobs").is_empty(), "{text}");
}

/// A factory with a different delay per worker name (one fast, one slow
/// dispatch) and a PER-WORKER in-flight flag, so a shutdown test can hold
/// BOTH dispatches in flight (a single shared flag would be cleared by the
/// fast task while the slow one was still running).
struct SplitFactory {
    requests: RecordedRequests,
    fast: Duration,
    slow: Duration,
    fast_in_flight: Arc<AtomicBool>,
    slow_in_flight: Arc<AtomicBool>,
    peak: Arc<AtomicUsize>,
    current: Arc<AtomicUsize>,
}

impl IsolateFactory for SplitFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        let (delay, in_flight) = if worker_ref.name.contains("-slow") {
            (self.slow, self.slow_in_flight.clone())
        } else {
            (self.fast, self.fast_in_flight.clone())
        };
        Box::new(SlowIsolate {
            requests: self.requests.clone(),
            delay,
            in_flight,
            peak: self.peak.clone(),
            current: self.current.clone(),
            version: worker_ref.version.clone(),
        })
    }
}

/// (EDG-34 emenda 2) The snapshot follows the shutdown transitions: right
/// after the supervisor signals the generations, the shared read-out says
/// `state: draining` with `nextRunAt: null` (the gauges already said so;
/// the snapshot used to keep `active` until the last task ended), and an
/// intermediate reap during the grace window removes the reaped job from
/// the list while the slower one keeps draining — the counters come from
/// the actual reaping.
#[tokio::test]
async fn shutdown_publishes_draining_snapshot_and_intermediate_reaps() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let in_flight = Arc::new(AtomicBool::new(false));
    let slow_in_flight = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "sd-fast",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );
    insert_version(
        &mut index,
        root.path(),
        "sd-slow",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );

    // Both tick at +100: the fast dispatch runs until +400, the slow one
    // until +700. The shutdown lands while BOTH are in flight, so neither
    // task can exit (or be reaped) before its dispatch finishes — the
    // `draining: 2` window is stable, not a sub-step blip.
    let (mut scheduler, app, metrics) = start_harness_factory_at_wall(
        index,
        Arc::new(SplitFactory {
            requests: requests.clone(),
            fast: Duration::from_millis(300),
            slow: Duration::from_millis(600),
            fast_in_flight: in_flight.clone(),
            slow_in_flight: slow_in_flight.clone(),
            peak: peak.clone(),
            current: current.clone(),
        }),
        RECONCILE,
        Some("root-secret"),
        virtual_wall(),
    )
    .await;
    advance_until(Duration::from_millis(300), || {
        in_flight.load(Ordering::SeqCst) && slow_in_flight.load(Ordering::SeqCst)
    })
    .await;

    // Start the shutdown (it signals the generations without waiting).
    let stopping = tokio::spawn(async move { scheduler.shutdown().await });

    // The signal lands: both jobs are draining — the SNAPSHOT and the
    // endpoint agree with the gauges (this is the window the probe hit
    // while the snapshot still said `active`).
    advance_until(Duration::from_millis(200), || metrics.draining_jobs() == 2).await;
    let snapshot = metrics.jobs();
    assert_eq!(snapshot.jobs.len(), 2, "both jobs are still tracked");
    for job in &snapshot.jobs {
        assert_eq!(job.state, CronJobState::Draining, "{}", job.worker);
        assert!(job.next_run_at.is_none(), "a draining job has no next run");
    }
    let (status, json, text) = admin_cron_jobs(&app, "root-secret").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    for item in json["jobs"].as_array().expect("jobs") {
        assert_eq!(item["state"], "draining", "{}", text);
        assert!(item["nextRunAt"].is_null(), "{}", text);
    }

    // The fast task (sleeping, no dispatch) exits and is reaped DURING the
    // grace window: the intermediate reap is visible right away — the job
    // leaves the snapshot and the gauges drop to one.
    advance_until(Duration::from_millis(500), || metrics.draining_jobs() == 1).await;
    let snapshot = metrics.jobs();
    let names: Vec<_> = snapshot
        .jobs
        .iter()
        .map(|job| job.worker.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["sd-slow"],
        "the reaped job left the snapshot: {names:?}"
    );
    assert_eq!(requests.count(), 1, "one tick done so far (the fast one)");
    assert!(
        slow_in_flight.load(Ordering::SeqCst),
        "the slow dispatch is still in flight"
    );
    let (_, json, text) = admin_cron_jobs(&app, "root-secret").await;
    let jobs = json["jobs"].as_array().expect("jobs");
    assert_eq!(jobs.len(), 1, "{}", text);
    assert_eq!(jobs[0]["worker"], "sd-slow", "{}", text);
    assert_eq!(jobs[0]["state"], "draining", "{}", text);

    // The slow dispatch finishes on its own (+700) inside the grace
    // window and the last reap empties the counters (no `store(0)`).
    stopping.await.unwrap();
    assert_eq!(metrics.active_jobs(), 0);
    assert_eq!(metrics.draining_jobs(), 0);
    assert!(metrics.jobs().jobs.is_empty());
}

/// (EDG-34 emenda 2) A civil wall step of +120 s (NTP / suspend) does not
/// move the task's MONOTONIC sleep — the tick is still 60 s away in
/// virtual time — but the deadline → UTC conversion uses the CURRENT
/// wall↔monotonic relationship: after the jump, the next republish moves
/// the published instant by +120 s and it is still 60 s in the future
/// (a boot-time anchor would render it 120 s in the past).
#[tokio::test]
async fn next_run_follows_the_current_wall_mapping_after_a_wall_step() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "walljump-worker",
        "1.0.0",
        Some(vec![job("@every 60s", "/tick", None)]),
        false,
    );

    let wall = Arc::new(TestWall::new());
    let (mut scheduler, _app, metrics) = start_harness_factory_at_wall(
        index,
        Arc::new(RecordingFactory {
            requests: requests.clone(),
        }),
        RECONCILE,
        Some("root-secret"),
        test_wall(&wall),
    )
    .await;

    advance_until(Duration::from_millis(50), || {
        first_next_run(&metrics).is_some()
    })
    .await;
    let before = first_next_run(&metrics).expect("the first deadline");

    // Model the +120 s civil step: the monotonic clock (the task's sleep)
    // is untouched, only the civil mapping moves.
    wall.jump(chrono::Duration::seconds(120));

    // The next republish (a reconciliation) converts with the current
    // mapping: the published instant jumps +120 s.
    advance_until(Duration::from_millis(200), || {
        first_next_run(&metrics) > Some(before)
    })
    .await;
    let after = first_next_run(&metrics).expect("the republished deadline");
    assert_eq!(
        after - before,
        chrono::Duration::seconds(120),
        "the civil step must move the published instant, not the task's deadline"
    );
    // ...and it is still 60 s in the future from the CURRENT wall.
    let ahead = after.signed_duration_since(wall.now());
    assert!(
        ahead >= chrono::Duration::seconds(60) - chrono::Duration::milliseconds(150)
            && ahead <= chrono::Duration::seconds(60) + chrono::Duration::milliseconds(150),
        "the published next run must still be ~60 s in the future, got {ahead}"
    );
    // The tick did not move: no dispatch fired (60 s away in virtual
    // time).
    assert_eq!(requests.count(), 0, "the monotonic sleep is unchanged");

    scheduler.shutdown().await;
}

/// (EDG-34 emenda 2, CWE-117) The cron lines log the manifest-derived
/// fields with unambiguous Debug escaping (`?field`): a CR/LF embedded in
/// a value — e.g. a worker NAME, which the `cron[]` manifest validation
/// does not cover — must stay on ONE physical line in the rendered
/// output. Rendered through the production `fmt` layer with the same
/// field structure as the job-set change lines.
#[test]
fn cron_log_fields_are_debug_escaped_on_one_physical_line() {
    use std::io::Write;

    struct SharedWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let writer_buffer = buffer.clone();
    let fmt_subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_ansi(false)
        .with_writer(move || Box::new(SharedWriter(writer_buffer.clone())))
        .finish();

    // A value with CR/LF on every manifest-derived field (a name that did
    // not go through the `cron[]` control-character validation).
    tracing::dispatcher::with_default(&tracing::Dispatch::new(fmt_subscriber), || {
        tracing::info!(
            worker = ?"evil\nfake=1",
            schedule = ?"@every 100\r\nms",
            path = ?"/ti\nck",
            method = ?"GE\nT",
            "cron job started"
        );
    });

    let rendered = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert_eq!(
        rendered.lines().count(),
        1,
        "a control character must not forge a second physical line: {rendered:?}"
    );
    assert!(
        rendered.contains(r#"worker="evil\nfake=1""#),
        "the worker must be Debug-escaped: {rendered:?}"
    );
    assert!(
        rendered.contains(r#"path="/ti\nck""#),
        "the path must be Debug-escaped: {rendered:?}"
    );
    assert!(
        rendered.contains(r#"method="GE\nT""#),
        "the method must be Debug-escaped: {rendered:?}"
    );

    // The counter-example that motivated the escaping: the same values
    // rendered with Display (the old `%field` form) WOULD leak the
    // newlines into physical lines.
    let display = format!(
        "worker={} schedule={} path={} method={}",
        "evil\nfake=1", "@every 100\r\nms", "/ti\nck", "GE\nT"
    );
    assert!(
        display.lines().count() > 1,
        "Display rendering leaks the newlines: {display:?}"
    );
}

/// (EDG-34 rodada 3, P2) A successor generation waiting for its
/// predecessor (disable + re-enable with a dispatch in flight) has NOT
/// committed any sleep: its `nextRunAt` is `null` until its first sleep,
/// and only then does it carry the real (future) deadline. The
/// saturation to the current instant applies only to an EXISTING
/// (already fired) deadline — a `None` cell must stay `null`, not become
/// a "now" the task never committed.
#[tokio::test]
async fn successor_generation_has_no_next_run_until_its_first_sleep() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let requests = RecordedRequests::default();
    let in_flight = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-successor",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick", None)]),
        false,
    );

    // The dispatch takes 10 s — the in-flight window in which the
    // disable/re-enable lands.
    let wall = Arc::new(TestWall::new());
    let (mut scheduler, app, metrics) = start_harness_factory_at_wall(
        index.clone(),
        Arc::new(SlowFactory {
            requests: requests.clone(),
            delay: Duration::from_secs(10),
            in_flight: in_flight.clone(),
            peak: peak.clone(),
            current: current.clone(),
        }),
        RECONCILE,
        Some("root-secret"),
        test_wall(&wall),
    )
    .await;

    // The first dispatch (at +100 ms) is in flight.
    advance_until(Duration::from_millis(300), || {
        in_flight.load(Ordering::SeqCst)
    })
    .await;

    // Disable: the job leaves the desired set and its generation starts
    // draining (the in-flight dispatch finishes normally).
    index
        .set_worker_enabled("cron-successor", Some("1.0.0"), false)
        .unwrap();
    advance_until(Duration::from_millis(100), || metrics.draining_jobs() == 1).await;

    // Re-enable: a NEW generation of the same key starts — it waits for
    // the still-in-flight predecessor before its first dispatch and has
    // not committed any sleep.
    index
        .set_worker_enabled("cron-successor", Some("1.0.0"), true)
        .unwrap();
    advance_until(Duration::from_millis(100), || metrics.active_jobs() == 1).await;

    // The ACTIVE job is the successor (its key sorts the draining
    // generation first, so pick by state, not by position).
    let active_job = || {
        metrics
            .jobs()
            .jobs
            .iter()
            .find(|job| job.state == CronJobState::Active)
            .cloned()
    };
    assert!(
        active_job()
            .expect("the successor generation is active")
            .next_run_at
            .is_none(),
        "the waiting successor has not committed any sleep"
    );
    // ...and the endpoint shows the same: `null`, not an invented "now".
    let (status, json, text) = admin_cron_jobs(&app, "root-secret").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let active = json["jobs"]
        .as_array()
        .expect("jobs")
        .iter()
        .find(|job| job["state"] == "active")
        .expect("the active successor is listed");
    assert!(
        active["nextRunAt"].is_null(),
        "the endpoint shows null until the successor's first sleep: {text}"
    );

    // The predecessor's dispatch ends and the successor commits its first
    // sleep: the `nextRunAt` becomes the REAL (still-ahead) deadline.
    advance_until(Duration::from_secs(11), || {
        active_job()
            .map(|job| job.next_run_at.is_some())
            .unwrap_or(false)
    })
    .await;
    let next = active_job()
        .expect("the successor")
        .next_run_at
        .expect("the real deadline");
    assert!(
        next > wall.now(),
        "the committed deadline is a real, still-ahead instant: {next}"
    );

    scheduler.shutdown().await;
}

/// (EDG-34 round 4) A `nextRunAt` that FITS a `DateTime<Utc>` but falls
/// after 9999-12-31T23:59:59.999Z would serialize with an RFC 3339
/// extended year (`+10000-...`) that some clients cannot parse: the
/// snapshot publishes `null` there — exactly as when the instant does not
/// fit a `DateTime<Utc>` at all — while the LAST millisecond of the year
/// 9999 is still published. The civil wall is parked at the last second
/// of 9999 (paused clock, deterministic: the published instant is
/// `base + interval`, independent of when the snapshot is read).
#[tokio::test]
async fn next_run_is_null_after_year_9999_and_published_in_9999() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    // 999 ms ahead of the wall base: still 9999-12-31T23:59:59.999Z (the
    // last published millisecond of the year). 1_000 ms ahead: the first
    // instant of the year 10000 (published as `null`).
    insert_version(
        &mut index,
        root.path(),
        "boundary-9999",
        "1.0.0",
        Some(vec![job("@every 999ms", "/tick", None)]),
        false,
    );
    insert_version(
        &mut index,
        root.path(),
        "boundary-10000",
        "1.0.0",
        Some(vec![job("@every 1000ms", "/tick", None)]),
        false,
    );
    let base = Utc.with_ymd_and_hms(9999, 12, 31, 23, 59, 59).unwrap();
    let mono0 = tokio::time::Instant::now();
    let wall = Arc::new(move || {
        let elapsed = tokio::time::Instant::now() - mono0;
        base + chrono::Duration::from_std(elapsed).unwrap_or_else(|_| chrono::Duration::zero())
    });
    let requests = RecordedRequests::default();
    let (mut scheduler, app, metrics) = start_harness_factory_at_wall(
        index,
        Arc::new(RecordingFactory {
            requests: requests.clone(),
        }),
        RECONCILE,
        Some("test-root"),
        wall,
    )
    .await;

    // Wait for the in-year job's deadline to be committed and published
    // (proof the tasks ran their first iteration), then a few more steps
    // so the after-year job's commit is flushed into a republish.
    advance_until(Duration::from_millis(100), || {
        metrics
            .jobs()
            .jobs
            .iter()
            .any(|j| j.worker == "boundary-9999" && j.next_run_at.is_some())
    })
    .await;
    for _ in 0..20 {
        tokio::time::advance(Duration::from_millis(1)).await;
    }

    // The snapshot: the in-year instant is published, the after-year
    // instant is `null`.
    let jobs = metrics.jobs().jobs;
    let before = jobs
        .iter()
        .find(|j| j.worker == "boundary-9999")
        .expect("the in-year job is tracked");
    assert_eq!(
        before.next_run_at,
        Some(base + chrono::Duration::milliseconds(999)),
        "the last millisecond of the year 9999 is still published"
    );
    let after = jobs
        .iter()
        .find(|j| j.worker == "boundary-10000")
        .expect("the after-year job is tracked");
    assert_eq!(
        after.next_run_at, None,
        "a deadline that fits a DateTime but falls after 9999-12-31T23:59:59.999Z publishes null"
    );

    // The endpoint renders the same values: the RFC 3339 string with a
    // four-digit year, and `null` for the after-year job.
    let (status, body, text) = admin_cron_jobs(&app, "test-root").await;
    assert_eq!(status, StatusCode::OK);
    let entry = |worker: &str| -> serde_json::Value {
        body["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["worker"] == worker)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    assert_eq!(
        entry("boundary-9999")["nextRunAt"],
        "9999-12-31T23:59:59.999Z",
        "the endpoint keeps the four-digit year: {text}"
    );
    assert!(
        entry("boundary-10000")["nextRunAt"].is_null(),
        "the endpoint publishes null for the after-year deadline: {text}"
    );

    scheduler.shutdown().await;
}
