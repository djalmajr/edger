//! (EDG-34) The admin listing of the cron jobs the scheduler tracks:
//! authentication (`observability:read`), the key's worker scope, the
//! scheduler-enabled flag and the version that follows a promote without
//! restarting the task.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::Utc;
use edger_core::{
    CronJob, Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig,
    WorkerManifest,
};
use edger_orchestrator::cron::VersionResolver;
use edger_orchestrator::{
    api_keys::ApiKeyService, build_pipeline, ControlAuth, CronMetrics, CronScheduler,
    CronSchedulerConfig, ManifestIndex, OrchestratorState, ServerState, WallClock,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::{json, Value};
use tower::ServiceExt;

const ROOT_KEY: &str = "test-root";
const RECONCILE: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// A trivial factory: the endpoint tests need the jobs to exist, not the
// ticks to be recorded.
// ---------------------------------------------------------------------------

struct OkIsolate;

#[async_trait]
impl Isolate for OkIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
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
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: Some(bytes::Bytes::from_static(b"<html></html>")),
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

struct OkFactory;

impl IsolateFactory for OkFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(OkIsolate)
    }
}

/// A factory whose dispatch takes `delay` of paused clock and reports the
/// in-flight window (EDG-34 emenda 2): a removal test needs a dispatch in
/// flight when it removes the worker.
struct DelayFactory {
    delay: Duration,
    in_flight: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
}

impl IsolateFactory for DelayFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(DelayIsolate {
            delay: self.delay,
            in_flight: self.in_flight.clone(),
            done: self.done.clone(),
        })
    }
}

struct DelayIsolate {
    delay: Duration,
    in_flight: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
}

#[async_trait]
impl Isolate for DelayIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.in_flight.store(true, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.in_flight.store(false, Ordering::SeqCst);
        self.done.fetch_add(1, Ordering::SeqCst);
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
            body: Some(bytes::Bytes::from(format!("<html>{path}</html>"))),
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
// Fixtures and helpers
// ---------------------------------------------------------------------------

fn job(schedule: &str, path: &str) -> CronJob {
    CronJob {
        schedule: schedule.into(),
        path: path.into(),
        method: None,
    }
}

/// Create the version dir (production layout `<root>/<name>/<version>`)
/// and insert the manifest into the index.
fn insert_version(
    index: &mut ManifestIndex,
    root: &Path,
    name: &str,
    version: &str,
    cron: Option<Vec<CronJob>>,
) {
    let dir = root.join(name).join(version);
    std::fs::create_dir_all(&dir).unwrap();
    index
        .insert(
            dir,
            WorkerManifest {
                name: name.into(),
                version: Some(version.into()),
                entrypoint: Some("index.ts".into()),
                cron,
                ..Default::default()
            },
        )
        .unwrap_or_else(|err| panic!("insert {name}@{version}: {err}"));
}

/// Build the pipeline and, when `with_scheduler`, start the cron scheduler
/// on the shared metrics (the endpoint reads them through the state),
/// with the VIRTUAL wall clock (EDG-34 emenda 2): the civil instant
/// follows the (paused) monotonic clock, so the deadline → UTC conversion
/// published in the snapshot is deterministic.
async fn app_with_factory(
    index: ManifestIndex,
    factory: Arc<dyn IsolateFactory>,
    with_scheduler: bool,
) -> (Option<CronScheduler>, Router, CronMetrics) {
    let auth = ControlAuth::with_static_key(ROOT_KEY)
        .with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()));
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
    let app = build_pipeline(state);
    let scheduler = if with_scheduler {
        Some(
            CronScheduler::start(
                CronSchedulerConfig::new(Some(ROOT_KEY.into()))
                    .with_reconcile_interval(RECONCILE)
                    .with_wall(virtual_wall()),
                index,
                app.clone(),
                metrics.clone(),
            )
            .await,
        )
    } else {
        None
    };
    (scheduler, app, metrics)
}

/// The virtual wall clock of the paused-clock tests: the civil instant is
/// the UTC captured here plus the elapsed (virtual) time.
fn virtual_wall() -> WallClock {
    let base = Utc::now();
    let mono0 = tokio::time::Instant::now();
    Arc::new(move || {
        let elapsed = tokio::time::Instant::now() - mono0;
        base + chrono::Duration::from_std(elapsed).unwrap_or_else(|_| chrono::Duration::zero())
    })
}

async fn app_with(index: ManifestIndex, with_scheduler: bool) -> (Option<CronScheduler>, Router) {
    let (scheduler, app, _metrics) =
        app_with_factory(index, Arc::new(OkFactory), with_scheduler).await;
    (scheduler, app)
}

/// (EDG-34 round 4, F1) The harness with an INJECTED version resolver: the
/// resolver the supervisor's per-pass memo calls on every worker-name
/// resolution. The F1 test injects a COUNTING resolver to assert that the
/// supervisor resolves each worker name at most once per pass — never
/// once per job.
async fn app_with_version_resolver(
    index: ManifestIndex,
    factory: Arc<dyn IsolateFactory>,
    resolver: VersionResolver,
) -> (Option<CronScheduler>, Router, CronMetrics) {
    let auth = ControlAuth::with_static_key(ROOT_KEY)
        .with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()));
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
    let app = build_pipeline(state);
    let scheduler = Some(
        CronScheduler::start(
            CronSchedulerConfig::new(Some(ROOT_KEY.into()))
                .with_reconcile_interval(RECONCILE)
                .with_wall(virtual_wall())
                .with_version_resolver(resolver),
            index,
            app.clone(),
            metrics.clone(),
        )
        .await,
    );
    (scheduler, app, metrics)
}

/// Advance the paused clock in 10 ms steps until `condition` holds.
async fn advance_until(max_total: Duration, mut condition: impl FnMut() -> bool) {
    let step = Duration::from_millis(10);
    let mut total = Duration::ZERO;
    while !condition() {
        if total >= max_total {
            panic!("condition not met after advancing {max_total:?} of paused clock");
        }
        tokio::time::advance(step).await;
        total += step;
    }
}

async fn send(app: Router, api_key: Option<&str>) -> (StatusCode, Value, String) {
    let mut builder = Request::builder().uri("/api/admin/cron/jobs");
    if let Some(key) = api_key {
        builder = builder.header("authorization", format!("Bearer {key}"));
    }
    let response = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value, text)
}

/// Create an API key as root and return its raw value.
async fn create_key(app: Router, body: Value) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys")
                .header("authorization", format!("Bearer {ROOT_KEY}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    value["rawKey"].as_str().unwrap().to_owned()
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
                .header("authorization", format!("Bearer {ROOT_KEY}"))
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

/// Two workers with cron jobs: `cron-w-x` (two paths, one schedule) and
/// `cron-w-y` (one path).
fn two_worker_index(root: &Path) -> ManifestIndex {
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.to_path_buf()]);
    insert_version(
        &mut index,
        root,
        "cron-w-x",
        "1.0.0",
        Some(vec![job("@every 60s", "/a"), job("@every 60s", "/b")]),
    );
    insert_version(
        &mut index,
        root,
        "cron-w-y",
        "1.0.0",
        Some(vec![job("@every 90s", "/tick")]),
    );
    index
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// (EDG-34) Without a credential the listing answers 401, like the other
/// admin endpoints.
#[tokio::test]
async fn list_requires_credentials() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let (scheduler, app) = app_with(two_worker_index(root.path()), true).await;

    let (status, json, _) = send(app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["code"], "UNAUTHORIZED");

    if let Some(mut scheduler) = scheduler {
        scheduler.shutdown().await;
    }
}

/// (EDG-34) A key without `observability:read` (even with full worker
/// scope) answers 403.
#[tokio::test]
async fn list_requires_observability_read_permission() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let (scheduler, app) = app_with(two_worker_index(root.path()), true).await;

    let key = create_key(
        app.clone(),
        json!({
            "name": "no-observability",
            "permissions": ["workers:read"],
            "namespaces": ["*"],
            "workers": ["*"]
        }),
    )
    .await;

    let (status, json, _) = send(app, Some(&key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "FORBIDDEN");

    if let Some(mut scheduler) = scheduler {
        scheduler.shutdown().await;
    }
}

/// (EDG-34) The root sees every tracked job with the full item shape,
/// ordered by worker, schedule, path and method; with the scheduler on the
/// answer carries `enabled: true` and an active job its `nextRunAt`.
#[tokio::test]
async fn root_sees_every_tracked_job_ordered() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let (scheduler, app) = app_with(two_worker_index(root.path()), true).await;

    // Let the tasks register their first sleep (the `nextRunAt` values).
    tokio::time::advance(Duration::from_millis(10)).await;

    let (status, json, text) = send(app, Some(ROOT_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["enabled"], true, "{text}");
    let jobs = json["jobs"].as_array().expect("jobs array");
    assert_eq!(jobs.len(), 3, "{text}");

    // Ordered by worker, schedule, path, method.
    assert_eq!(jobs[0]["worker"], "cron-w-x");
    assert_eq!(jobs[0]["path"], "/a");
    assert_eq!(jobs[1]["worker"], "cron-w-x");
    assert_eq!(jobs[1]["path"], "/b");
    assert_eq!(jobs[2]["worker"], "cron-w-y");
    assert_eq!(jobs[2]["path"], "/tick");

    // The full item shape: identity, resolved version, state, next run.
    for (position, item) in jobs.iter().enumerate() {
        let schedule = if position == 2 {
            "@every 90s"
        } else {
            "@every 60s"
        };
        assert_eq!(item["schedule"], schedule, "or {text}");
        assert_eq!(item["method"], "GET");
        assert_eq!(item["version"], "1.0.0", "{text}");
        assert_eq!(item["state"], "active", "{text}");
        assert!(
            item["nextRunAt"].is_string(),
            "an active job carries its nextRunAt: {text}"
        );
    }
    // RFC 3339 UTC (the `Z` form for a whole-second-or-fraction instant).
    let next_run = jobs[0]["nextRunAt"].as_str().unwrap();
    assert!(
        next_run.ends_with('Z') || next_run.ends_with("+00:00"),
        "nextRunAt is UTC: {next_run}"
    );

    if let Some(mut scheduler) = scheduler {
        scheduler.shutdown().await;
    }
}

/// (EDG-34) A key scoped to `workers=[cron-w-x]` sees only the jobs of
/// `cron-w-x` — nothing of the other worker, not even by the count.
#[tokio::test]
async fn scoped_key_sees_only_its_worker_jobs() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let (scheduler, app) = app_with(two_worker_index(root.path()), true).await;
    tokio::time::advance(Duration::from_millis(10)).await;

    let key = create_key(
        app.clone(),
        json!({
            "name": "scoped-x",
            "permissions": ["observability:read"],
            "namespaces": ["*"],
            "workers": ["cron-w-x"]
        }),
    )
    .await;

    let (status, json, text) = send(app.clone(), Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["enabled"], true, "{text}");
    let jobs = json["jobs"].as_array().expect("jobs array");
    assert_eq!(jobs.len(), 2, "only the scoped worker's jobs: {text}");
    for item in jobs.iter() {
        assert_eq!(item["worker"], "cron-w-x", "{text}");
    }

    // A key scoped to the OTHER worker sees only its own single job.
    let key_y = create_key(
        app.clone(),
        json!({
            "name": "scoped-y",
            "permissions": ["observability:read"],
            "namespaces": ["*"],
            "workers": ["cron-w-y"]
        }),
    )
    .await;
    let (status, json, text) = send(app, Some(&key_y)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let jobs = json["jobs"].as_array().expect("jobs array");
    assert_eq!(jobs.len(), 1, "{text}");
    assert_eq!(jobs[0]["worker"], "cron-w-y");

    if let Some(mut scheduler) = scheduler {
        scheduler.shutdown().await;
    }
}

/// (EDG-34) With the scheduler off the endpoint answers 200 with
/// `enabled: false` and the empty list — even when workers with cron jobs
/// are installed.
#[tokio::test]
async fn disabled_scheduler_answers_enabled_false_with_empty_list() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let (scheduler, app) = app_with(two_worker_index(root.path()), false).await;

    assert!(scheduler.is_none());
    let (status, json, text) = send(app, Some(ROOT_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["enabled"], false, "{text}");
    assert_eq!(json["jobs"].as_array().expect("jobs array").len(), 0);
}

/// (EDG-34) The item's `version` follows the unversioned route's
/// resolution at query time: a promote moves the reported version without
/// restarting the task (the job keeps its single active entry and its
/// `nextRunAt` phase).
#[tokio::test]
async fn version_follows_promote_without_restarting_the_task() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-w-v",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick")]),
    );
    insert_version(
        &mut index,
        root.path(),
        "cron-w-v",
        "2.0.0",
        Some(vec![job("@every 100ms", "/tick")]),
    );
    let (scheduler, app) = app_with(index, true).await;

    // The promoted default pointer decides the serving version: start on
    // 1.0.0 (without a pointer the highest enabled semver — 2.0.0 — would
    // resolve).
    promote(&app, "cron-w-v", "1.0.0").await;
    tokio::time::advance(Duration::from_millis(10)).await;

    let (_, before, text) = send(app.clone(), Some(ROOT_KEY)).await;
    assert_eq!(before["jobs"][0]["version"], "1.0.0", "{text}");
    assert_eq!(before["jobs"][0]["state"], "active", "{text}");
    let next_before = before["jobs"][0]["nextRunAt"]
        .as_str()
        .expect("an active job has its nextRunAt")
        .to_owned();

    // Promote to 2.0.0 (the operator path): the same `cron` entries keep
    // the running task — no restart, no draining entry.
    promote(&app, "cron-w-v", "2.0.0").await;
    for _ in 0..10 {
        tokio::time::advance(Duration::from_millis(10)).await;
        let (_, json, _text) = send(app.clone(), Some(ROOT_KEY)).await;
        if json["jobs"][0]["version"] == "2.0.0" {
            break;
        }
    }
    let (_, after, text) = send(app, Some(ROOT_KEY)).await;
    let jobs = after["jobs"].as_array().expect("jobs array");
    assert_eq!(jobs.len(), 1, "one job across the promote: {text}");
    assert_eq!(jobs[0]["version"], "2.0.0", "{text}");
    assert_eq!(jobs[0]["state"], "active", "{text}");
    assert_eq!(
        jobs[0]["nextRunAt"].as_str(),
        Some(next_before.as_str()),
        "the task kept its phase (no restart): {text}"
    );

    if let Some(mut scheduler) = scheduler {
        scheduler.shutdown().await;
    }
}

/// (EDG-34 emenda 2) A removed worker with a draining job stays visible:
/// with a dispatch in flight the last version is removed from the index,
/// and the job is reaped only when the dispatch finishes. The endpoint
/// must show the draining job — `version: null`, `state: draining`,
/// `nextRunAt: null` — to the root and to the key whose worker scope
/// covers the name (the scope is the same single predicate as
/// `list_workers`, applied to the job's NAME — not to index presence),
/// and hide it from the key whose scope does not cover the name.
#[tokio::test]
async fn removed_worker_draining_job_visible_to_root_and_scoped_key() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-del",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick")]),
    );

    let in_flight = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let (scheduler, app, metrics) = app_with_factory(
        index.clone(),
        Arc::new(DelayFactory {
            delay: Duration::from_millis(500),
            in_flight: in_flight.clone(),
            done: done.clone(),
        }),
        true,
    )
    .await;

    // Ticks fire at +100 (until +600) and +600 (until +1100): wait until
    // the second dispatch is in flight — the removal then lands with a
    // dispatch running (the probe window).
    advance_until(Duration::from_millis(1_500), || {
        in_flight.load(Ordering::SeqCst) && done.load(Ordering::SeqCst) >= 1
    })
    .await;
    index.remove_worker("cron-del", "1.0.0").unwrap();
    advance_until(Duration::from_millis(300), || metrics.draining_jobs() == 1).await;

    // Root: the draining job of the removed worker stays visible, with
    // `version: null` (the worker no longer resolves).
    let (status, json, text) = send(app.clone(), Some(ROOT_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["enabled"], true, "{text}");
    let jobs = json["jobs"].as_array().expect("jobs");
    assert_eq!(
        jobs.len(),
        1,
        "the draining job of a removed worker stays visible: {text}"
    );
    assert_eq!(jobs[0]["worker"], "cron-del", "{text}");
    assert_eq!(jobs[0]["state"], "draining", "{text}");
    assert!(
        jobs[0]["version"].is_null(),
        "a removed worker does not resolve: {text}"
    );
    assert!(jobs[0]["nextRunAt"].is_null(), "{text}");

    // A key whose scope covers the name sees the same job — the predicate
    // runs on the NAME, so the removal from the index does not hide it.
    let key = create_key(
        app.clone(),
        json!({
            "name": "scoped-del",
            "permissions": ["observability:read"],
            "namespaces": ["*"],
            "workers": ["cron-del"]
        }),
    )
    .await;
    let (status, json, text) = send(app.clone(), Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let jobs = json["jobs"].as_array().expect("jobs");
    assert_eq!(
        jobs.len(),
        1,
        "the scoped key sees the draining job: {text}"
    );
    assert_eq!(jobs[0]["worker"], "cron-del", "{text}");
    assert_eq!(jobs[0]["state"], "draining", "{text}");
    assert!(jobs[0]["version"].is_null(), "{text}");

    // A key whose scope does not cover the name sees nothing.
    let key_other = create_key(
        app.clone(),
        json!({
            "name": "scoped-other",
            "permissions": ["observability:read"],
            "namespaces": ["*"],
            "workers": ["cron-other"]
        }),
    )
    .await;
    let (status, json, text) = send(app.clone(), Some(&key_other)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(
        json["jobs"].as_array().expect("jobs").is_empty(),
        "an out-of-scope key must not see the removed worker's job: {text}"
    );

    // The in-flight dispatch finishes and the reaping empties the job.
    advance_until(Duration::from_millis(1000), || metrics.draining_jobs() == 0).await;
    assert!(metrics.jobs().jobs.is_empty());

    if let Some(mut scheduler) = scheduler {
        scheduler.shutdown().await;
    }
}

/// (EDG-35) Endpoint level (the security probe's scenario): a key with a
/// short `expiresAt` warms the authentication cache while still valid and
/// is rejected with 401 once its absolute expiration passes — before the
/// fix the cache served the warmed entry for up to the 60 s TTL (the
/// probe saw 200). Real clock: the expiration is in epoch seconds and the
/// entry is still inside the cache TTL when it passes.
#[tokio::test]
async fn warmed_expired_key_gets_401_at_the_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-exp",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick")]),
    );
    let (_scheduler, app) = app_with(index, false).await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let key = create_key(
        app.clone(),
        json!({
            "name": "short-lived",
            "permissions": ["observability:read"],
            "namespaces": ["*"],
            "workers": ["cron-exp"],
            "expiresAt": now + 1
        }),
    )
    .await;

    // Warming: the request while the key is valid passes (and caches the
    // principal).
    let (status, _json, _text) = send(app.clone(), Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "the valid key warms the cache");

    // The absolute expiration passes (epoch-second granularity: wait past
    // the boundary, with margin); the entry is still inside the 60 s
    // cache TTL.
    std::thread::sleep(Duration::from_millis(2_500));

    let (status, _json, text) = send(app, Some(&key)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the expired key must 401: {text}"
    );
}

/// (EDG-34 rodada 3, P1) A valid manifest whose `@every` interval is
/// enormous (`9000000000000000ms` ≈ 285,000 years — a deadline that does
/// not fit a `DateTime<Utc>`, and that may not fit the platform
/// `Instant` at all) must not kill the shared supervisor: the scheduler
/// stays `enabled`, the other worker's jobs keep firing, the endpoint
/// answers 200 and the huge job shows `nextRunAt: null` (the task's sleep
/// still saturates to the far future, exactly as before EDG-34). Before
/// the fix the `wall_now + delta` in the snapshot conversion panicked
/// `DateTime + TimeDelta overflowed` in the supervisor, disabled the
/// scheduler and stopped every other worker's cron.
#[tokio::test]
async fn large_valid_interval_does_not_stop_other_cron_jobs() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    insert_version(
        &mut index,
        root.path(),
        "cron-large-healthy",
        "1.0.0",
        Some(vec![job("@every 100ms", "/tick")]),
    );
    let (scheduler, app, metrics) =
        app_with_factory(index.clone(), Arc::new(OkFactory), true).await;

    // The healthy job has fired at least once.
    advance_until(Duration::from_secs(1), || metrics.executions_total() > 0).await;
    let before = metrics.executions_total();

    // The enormous interval is a VALID manifest value (it parses to a
    // `Duration`): installing it must not panic the supervisor.
    insert_version(
        &mut index,
        root.path(),
        "cron-large",
        "1.0.0",
        Some(vec![job("@every 9000000000000000ms", "/tick")]),
    );
    // Let several reconciliation passes (and the huge job's task commit
    // window) run.
    for _ in 0..30 {
        tokio::time::advance(Duration::from_millis(10)).await;
        tokio::task::yield_now().await;
    }

    assert!(
        metrics.scheduler_enabled(),
        "a valid manifest's interval must not kill the shared supervisor"
    );
    assert!(
        metrics.executions_total() > before,
        "other cron jobs must keep executing"
    );

    let (status, json, text) = send(app, Some(ROOT_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let jobs = json["jobs"].as_array().expect("jobs array");
    let healthy = jobs
        .iter()
        .find(|job| job["worker"] == "cron-large-healthy")
        .expect("the healthy job is listed");
    assert!(
        healthy["nextRunAt"].is_string(),
        "the healthy job still carries its nextRunAt: {text}"
    );
    let large = jobs
        .iter()
        .find(|job| job["worker"] == "cron-large")
        .expect("the huge job is listed");
    assert!(
        large["nextRunAt"].is_null(),
        "a deadline that does not fit a representable UTC instant is published as null: {text}"
    );

    if let Some(mut scheduler) = scheduler {
        scheduler.shutdown().await;
    }
}

/// (EDG-34 rodada 3, security probe) A control character in the `method`
/// of a `cron[]` job is refused at the index boundary (the manifest
/// validation), so such a job never reaches the scheduler (and its log
/// lines) — and the scheduler's own lines, rendered through the real
/// scheduler path with a newline in the worker NAME (the field the
/// `cron[]` validation does not cover), stay on ONE physical line:
/// registration and the invalid-job warning both carry the manifest
/// text in Debug-escaped form.
#[tokio::test]
async fn newline_method_is_refused_and_scheduler_lines_stay_on_one_physical_line() {
    use std::io::Write;

    struct BufferWriter(Arc<std::sync::Mutex<Vec<u8>>>);
    impl Write for BufferWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
    let out = buffer.clone();
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_ansi(false)
        .with_writer(move || BufferWriter(out.clone()))
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    // A newline in the worker NAME is not covered by the `cron[]`
    // field validation — the live path for manifest-derived text into
    // the scheduler's registration line.
    insert_version(
        &mut index,
        root.path(),
        "cron-line\nmarker-reg",
        "1.0.0",
        Some(vec![job("@every 60s", "/tick")]),
    );
    // An invalid (but control-free) METHOD — a space is not a token
    // character, so `Method::from_bytes` refuses it — reaches the
    // scheduler and triggers the invalid-job warning line.
    insert_version(
        &mut index,
        root.path(),
        "cron-line\nmarker-warn",
        "1.0.0",
        Some(vec![CronJob {
            schedule: "@every 60s".into(),
            path: "/tick".into(),
            method: Some("GE T".into()),
        }]),
    );

    // A control character in the METHOD is refused at the index
    // boundary: the job never reaches the scheduler (and its log lines).
    let denied = index.insert(
        root.path().join("cron-line-denied").join("1.0.0"),
        WorkerManifest {
            name: "cron-line-denied".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            cron: Some(vec![CronJob {
                schedule: "@every 60s".into(),
                path: "/tick".into(),
                method: Some("GE\ncron-line-denied-marker".into()),
            }]),
            ..Default::default()
        },
    );
    let err = denied.expect_err("a control character in the cron method is refused");
    assert_eq!(err.code, "VALIDATION_ERROR");
    assert!(
        err.message.contains("manifest.cron[0].method"),
        "the error names the offending field: {}",
        err.message
    );

    let (scheduler, _app, metrics) = app_with_factory(index, Arc::new(OkFactory), true).await;
    // Only the invalid `GE T` job counts a failure — the refused one
    // never reached the scheduler.
    assert_eq!(
        metrics.failures_total(),
        1,
        "only the invalid `GE T` job counts a scheduler failure"
    );

    if let Some(mut scheduler) = scheduler {
        scheduler.shutdown().await;
    }

    let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    // Registration and warning lines carry the newline names escaped.
    assert!(
        text.contains(r#"worker="cron-line\nmarker-reg""#),
        "the registration line keeps the name on one physical line"
    );
    assert!(
        text.contains(r#"worker="cron-line\nmarker-warn""#),
        "the warning line keeps the name on one physical line"
    );
    // No raw (unescaped) manifest text anywhere in the log output.
    assert!(
        !text.contains("worker=cron-line\nmarker"),
        "an unescaped name must not forge a physical line"
    );
    assert!(
        !text.contains("GE\ncron-line-denied-marker"),
        "the refused method must not reach the log at all"
    );
    // The warning message (the error, built from the manifest method)
    // stays on the same physical line as the rest of the warning.
    assert!(
        text.lines().any(|line| {
            line.contains("cron job is invalid") && line.contains("invalid HTTP method: GE T")
        }),
        "the invalid-job warning and its error stay on one physical line"
    );
}

/// (EDG-34 round 4, F1) The supervisor resolves the version ONCE per
/// worker NAME per pass — not once per job: with 2_000 jobs of one worker
/// the counting resolver sees ONE call for the registration pass (the
/// 2_000 `cron job started` lines of the boot reconcile) and ONE call for
/// the shutdown pass (the 2_000 `cron job drained` lines of the reaping,
/// which runs through the shutdown's single memo) — whatever the job
/// count. Paused clock; the assertion is the exact call count, not a
/// real-clock time limit. (A pass with no job-set change logs no line and
/// resolves nothing: the memo is only consulted by the change lines.)
#[tokio::test]
async fn supervisor_resolves_the_version_once_per_worker_name_per_pass() {
    tokio::time::pause();
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    const JOBS: usize = 2000;
    let jobs: Vec<CronJob> = (0..JOBS)
        .map(|i| job("@every 1h", &format!("/p{i}")))
        .collect();
    insert_version(&mut index, root.path(), "cron-w-f1", "1.0.0", Some(jobs));
    let calls = Arc::new(AtomicUsize::new(0));
    let counting = calls.clone();
    // The job-set change lines resolve their version EAGERLY (before the
    // `tracing` event — `tracing` fields are otherwise evaluated lazily,
    // only when the event reaches an interested subscriber), so the
    // count is deterministic here without a global subscriber.
    let (scheduler, _app, metrics) = app_with_version_resolver(
        index,
        Arc::new(OkFactory),
        Arc::new(move |_index, _name| {
            counting.fetch_add(1, Ordering::SeqCst);
            Some("1.0.0".to_owned())
        }),
    )
    .await;
    let mut scheduler = scheduler.expect("the scheduler starts");
    // The registration pass logged 2_000 `cron job started` lines for the
    // single worker name: it resolved the name ONCE, not once per job, and
    // tracks all 2_000 generations.
    assert_eq!(metrics.active_jobs(), JOBS);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the registration pass resolves the worker name once, not per job"
    );
    // The shutdown pass signals and reaps the 2_000 generations through
    // its own single memo: 2_000 `cron job drained` lines, one resolution.
    scheduler.shutdown().await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the shutdown reaping pass resolves the worker name once, not per job"
    );
    assert_eq!(
        metrics.active_jobs() + metrics.draining_jobs(),
        0,
        "every generation was reaped on shutdown"
    );
}
