//! EDG-12: install, promote and enable prewarm the `minProcesses` floor in
//! the background — the API response never waits for the prewarm and no
//! worker request is needed to create the instances.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerRef,
};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool, WorkerState};
use tower::ServiceExt;

#[derive(Default)]
struct CountingFactory {
    created: AtomicUsize,
    prepared: Arc<AtomicUsize>,
    fail_prepare: Arc<AtomicBool>,
}

impl CountingFactory {
    fn ok() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Factory whose `prepare` always fails: prewarm attempts die there.
    fn failing() -> Arc<Self> {
        Arc::new(Self {
            created: AtomicUsize::new(0),
            prepared: Arc::new(AtomicUsize::new(0)),
            fail_prepare: Arc::new(AtomicBool::new(true)),
        })
    }

    fn created_count(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }

    fn prepared_count(&self) -> usize {
        self.prepared.load(Ordering::SeqCst)
    }
}

impl IsolateFactory for CountingFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        self.created.fetch_add(1, Ordering::SeqCst);
        Box::new(CountingIsolate {
            prepared: Arc::clone(&self.prepared),
            fail_prepare: Arc::clone(&self.fail_prepare),
        })
    }
}

struct CountingIsolate {
    prepared: Arc<AtomicUsize>,
    fail_prepare: Arc<AtomicBool>,
}

#[async_trait]
impl Isolate for CountingIsolate {
    async fn prepare(&mut self, _config: &WorkerConfig) -> Result<(), IsolationError> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        if self.fail_prepare.load(Ordering::SeqCst) {
            return Err(IsolationError::new(
                "MOCK_PREPARE_FAILURE",
                "mock prepare failure",
            ));
        }
        Ok(())
    }

    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: Vec::new(),
            body: Some(bytes::Bytes::from_static(b"ok")),
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
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(
            SerializedRequest {
                method: "GET".into(),
                uri: "/".into(),
                headers: Vec::new(),
                body: None,
                request_id: "prewarm-on-activate".into(),
                base_href: None,
            },
            config,
        )
        .await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }
}

fn state_with_factory(
    roots: Vec<std::path::PathBuf>,
    factory: Arc<CountingFactory>,
) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), factory);
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&roots).unwrap(),
        auth: edger_orchestrator::ControlAuth::with_static_key("test-root"),
    }
}

fn write_worker(root: &std::path::Path, name: &str, manifest: &str, files: &[(&str, &str)]) {
    let worker_dir = root.join(name);
    std::fs::create_dir(&worker_dir).unwrap();
    std::fs::write(worker_dir.join("manifest.yaml"), manifest).unwrap();
    for (file, contents) in files {
        std::fs::write(worker_dir.join(file), contents).unwrap();
    }
}

fn zip_package(files: &[(&str, &str)]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

fn app_zip(version: &str, min_processes: &str) -> Vec<u8> {
    zip_package(&[
        (
            "manifest.yaml",
            &format!(
                "name: zip-app\nversion: \"{version}\"\nentrypoint: index.ts\nkind: fetch\nttl: 30s\nminProcesses: {min_processes}\n"
            ),
        ),
        ("index.ts", "Deno.serve(() => new Response('ok'));"),
    ])
}

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, serde_json::Value, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", content_type)
        .header("authorization", "Bearer test-root");
    let request = match body.is_empty() {
        true => request.body(Body::empty()).unwrap(),
        false => request.body(Body::from(body)).unwrap(),
    };
    let res = app.oneshot(request).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json, text)
}

/// Poll until the condition holds (or the deadline passes); returns whether
/// it did. `timeout` keeps the waits short: no long fixed sleeps.
async fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool + Send) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return condition();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

const PREWARM_TIMEOUT: Duration = Duration::from_secs(2);
/// Window long enough for a wrongly scheduled prewarm to show up, short
/// enough to keep the negative assertions fast.
const NEGATIVE_WINDOW: Duration = Duration::from_millis(500);

fn instance_count(pool: &WorkerPool, name: &str, version: &str) -> usize {
    pool.worker_stats()
        .into_iter()
        .filter(|worker| worker.name == name && worker.version == version)
        .count()
}

fn prewarm_events(
    state: &OrchestratorState,
    worker: &str,
) -> Vec<edger_orchestrator::observability::OperationalEvent> {
    state
        .server
        .operational_events()
        .query(edger_orchestrator::observability::OperationalEventQuery {
            worker: Some(worker.into()),
            kind: Some("worker.prewarm".into()),
            ..Default::default()
        })
        .events
}

// Test 1: installing a version with `minProcesses: 1` schedules the
// background prewarm; after the response — and without any request to the
// worker — the pool already holds one idle instance of that version.
#[tokio::test]
async fn install_with_min_processes_prewarms_before_any_request() {
    let root = tempfile::tempdir().unwrap();
    let factory = CountingFactory::ok();
    let state = state_with_factory(vec![root.path().to_path_buf()], factory.clone());
    let app = build_pipeline(state.clone());

    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/install",
        "application/zip",
        app_zip("1.0.0", "1"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");
    assert_eq!(json["activation"], "active");
    assert_eq!(json["prewarm"], "scheduled");

    assert!(
        wait_for(PREWARM_TIMEOUT, || instance_count(
            &state.pool,
            "zip-app",
            "1.0.0"
        ) == 1)
        .await,
        "pool did not reach one instance of zip-app@1.0.0: {:?}",
        state.pool.worker_stats()
    );
    // Wait for the prewarm task to fully complete (the event is recorded
    // after the spawn loop, once the instance is Idle).
    assert!(
        wait_for(PREWARM_TIMEOUT, || prewarm_events(&state, "zip-app").len()
            == 1)
        .await,
        "prewarm event was not recorded: {:?}",
        prewarm_events(&state, "zip-app")
    );
    assert_eq!(
        state
            .pool
            .worker_stats()
            .into_iter()
            .find(|worker| worker.name == "zip-app" && worker.version == "1.0.0")
            .unwrap()
            .state,
        WorkerState::Idle,
        "prewarmed instance must be Idle before any request"
    );
    // Exactly one isolate was created: the prewarm's own spawn, with no
    // dispatch involved.
    assert_eq!(factory.created_count(), 1);
    assert_eq!(factory.prepared_count(), 1);

    // The prewarm outcome lands in the operational events with the created
    // count (same envelope as the rescan prewarm).
    let events = prewarm_events(&state, "zip-app");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].level,
        edger_orchestrator::observability::OperationalEventLevel::Info
    );
    assert_eq!(events[0].outcome.as_deref(), Some("completed"));
    assert_eq!(events[0].version.as_deref(), Some("1.0.0"));
    assert!(
        events[0]
            .message
            .as_deref()
            .is_some_and(|message| message.contains("prewarmed 1 process")),
        "event must carry the created count: {:?}",
        events[0].message
    );
}

// Test 2: installing a version with `minProcesses: 0` creates nothing and
// reports `not_configured`.
#[tokio::test]
async fn install_without_min_processes_reports_not_configured() {
    let root = tempfile::tempdir().unwrap();
    let factory = CountingFactory::ok();
    let state = state_with_factory(vec![root.path().to_path_buf()], factory.clone());
    let app = build_pipeline(state.clone());

    let (status, json, text) = send(
        app,
        "POST",
        "/api/admin/workers/install",
        "application/zip",
        app_zip("1.0.0", "0"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");
    assert_eq!(json["activation"], "active");
    assert_eq!(json["prewarm"], "not_configured");

    // Give a hypothetical (wrong) prewarm a chance to run: nothing happens.
    assert!(!wait_for(NEGATIVE_WINDOW, || factory.created_count() > 0).await);
    assert_eq!(factory.created_count(), 0);
    assert_eq!(instance_count(&state.pool, "zip-app", "1.0.0"), 0);
    assert!(prewarm_events(&state, "zip-app").is_empty());
}

// Test 3: promoting a version with `minProcesses: 1` whose pool does not
// exist yet creates the instance without any request.
#[tokio::test]
async fn promote_prewarms_a_version_whose_pool_does_not_exist() {
    let current = tempfile::tempdir().unwrap();
    let promoted = tempfile::tempdir().unwrap();
    write_worker(
        current.path(),
        "warmapp",
        "name: warmapp\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nttl: 30s\n",
        &[("index.ts", "Deno.serve(() => new Response('old'));")],
    );
    write_worker(
        promoted.path(),
        "warmapp",
        "name: warmapp\nversion: \"2.0.0\"\nentrypoint: index.ts\nkind: fetch\nttl: 30s\nminProcesses: 1\n",
        &[("index.ts", "Deno.serve(() => new Response('new'));")],
    );
    let factory = CountingFactory::ok();
    let state = state_with_factory(
        vec![current.path().to_path_buf(), promoted.path().to_path_buf()],
        factory.clone(),
    );
    let app = build_pipeline(state.clone());

    assert_eq!(instance_count(&state.pool, "warmapp", "2.0.0"), 0);
    let (status, json, text) = send(
        app,
        "POST",
        "/api/admin/workers/warmapp/promote?version=2.0.0",
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["status"], "promoted");
    assert_eq!(json["prewarm"], "scheduled");

    assert!(
        wait_for(PREWARM_TIMEOUT, || instance_count(
            &state.pool,
            "warmapp",
            "2.0.0"
        ) == 1)
        .await,
        "promoted version was not prewarmed: {:?}",
        state.pool.worker_stats()
    );
    assert_eq!(factory.created_count(), 1);
    assert!(
        wait_for(PREWARM_TIMEOUT, || prewarm_events(&state, "warmapp").len()
            == 1)
        .await,
        "prewarm event was not recorded: {:?}",
        prewarm_events(&state, "warmapp")
    );
    let events = prewarm_events(&state, "warmapp");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].version.as_deref(), Some("2.0.0"));
    assert_eq!(events[0].outcome.as_deref(), Some("completed"));
}

// Test 4: enabling a disabled version with `minProcesses: 1` creates the
// instance; disabling never prewarms.
#[tokio::test]
async fn enable_prewarms_the_disabled_version_and_disable_never_prewarms() {
    let root = tempfile::tempdir().unwrap();
    write_worker(
        root.path(),
        "enabler",
        "name: enabler\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nttl: 30s\nminProcesses: 1\n",
        &[("index.ts", "Deno.serve(() => new Response('ok'));")],
    );
    let factory = CountingFactory::ok();
    let state = state_with_factory(vec![root.path().to_path_buf()], factory.clone());
    let app = build_pipeline(state.clone());

    // The version starts active; disable it first so the enable under test
    // really activates a disabled version.
    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/enabler/disable?version=1.0.0",
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["prewarm"], "not_configured");
    assert!(!wait_for(NEGATIVE_WINDOW, || factory.created_count() > 0).await);
    assert_eq!(factory.created_count(), 0);

    let (status, json, text) = send(
        app,
        "POST",
        "/api/admin/workers/enabler/enable?version=1.0.0",
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["prewarm"], "scheduled");
    assert!(
        wait_for(PREWARM_TIMEOUT, || instance_count(
            &state.pool,
            "enabler",
            "1.0.0"
        ) == 1)
        .await,
        "enabled version was not prewarmed: {:?}",
        state.pool.worker_stats()
    );
    assert_eq!(factory.created_count(), 1);
    assert!(
        wait_for(PREWARM_TIMEOUT, || prewarm_events(&state, "enabler").len()
            == 1)
        .await,
        "prewarm event was not recorded: {:?}",
        prewarm_events(&state, "enabler")
    );

    // Disabling again never prewarms and creates no instance.
    let state = state.clone();
    let app = build_pipeline(state);
    let (status, json, text) = send(
        app,
        "POST",
        "/api/admin/workers/enabler/disable?version=1.0.0",
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["prewarm"], "not_configured");
    assert!(!wait_for(NEGATIVE_WINDOW, || factory.created_count() > 1).await);
    assert_eq!(factory.created_count(), 1);
}

// Test 5: promoting a version that is already warm does not create a second
// instance (the prewarm is idempotent).
#[tokio::test]
async fn promoting_an_already_warm_version_does_not_create_a_second_instance() {
    let root = tempfile::tempdir().unwrap();
    write_worker(
        root.path(),
        "idempotent",
        "name: idempotent\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nttl: 30s\nminProcesses: 1\n",
        &[("index.ts", "Deno.serve(() => new Response('ok'));")],
    );
    let factory = CountingFactory::ok();
    let state = state_with_factory(vec![root.path().to_path_buf()], factory.clone());
    let app = build_pipeline(state.clone());

    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/idempotent/promote?version=1.0.0",
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["prewarm"], "scheduled");
    // Wait for the first prewarm task to fully complete (event recorded
    // after the spawn loop), so the second promote sees the instance Idle.
    assert!(
        wait_for(PREWARM_TIMEOUT, || prewarm_events(&state, "idempotent")
            .len()
            == 1)
        .await,
        "first prewarm event was not recorded"
    );
    assert_eq!(instance_count(&state.pool, "idempotent", "1.0.0"), 1);
    assert_eq!(factory.created_count(), 1);

    // Second promote of the same warm version: scheduled, but no second
    // instance.
    let (status, json, text) = send(
        app,
        "POST",
        "/api/admin/workers/idempotent/promote?version=1.0.0",
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["prewarm"], "scheduled");
    // Wait for the second prewarm attempt to complete (second event), then
    // prove it created nothing. `query` returns newest first.
    assert!(
        wait_for(PREWARM_TIMEOUT, || prewarm_events(&state, "idempotent")
            .len()
            == 2)
        .await,
        "second prewarm event was not recorded: {:?}",
        prewarm_events(&state, "idempotent")
    );
    let messages = prewarm_events(&state, "idempotent")
        .iter()
        .map(|event| event.message.clone().unwrap_or_default())
        .collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("prewarmed 1 process")),
        "first prewarm must have created one instance: {:?}",
        messages
    );
    assert!(
        messages
            .iter()
            .any(|message| message.contains("prewarmed 0 process")),
        "second prewarm must have created nothing: {:?}",
        messages
    );
    assert!(
        !wait_for(NEGATIVE_WINDOW, || instance_count(
            &state.pool,
            "idempotent",
            "1.0.0"
        ) > 1)
        .await
    );
    assert_eq!(factory.created_count(), 1);
    assert_eq!(instance_count(&state.pool, "idempotent", "1.0.0"), 1);
}

// Decision 4: an install with `staged` leaves the version active, so it
// prewarms the floor as well (the Cinzel proof case).
#[tokio::test]
async fn staged_install_prewarms_because_the_version_is_active() {
    let root = tempfile::tempdir().unwrap();
    let factory = CountingFactory::ok();
    let state = state_with_factory(vec![root.path().to_path_buf()], factory.clone());
    let app = build_pipeline(state.clone());

    let (status, json, text) = send(
        app,
        "POST",
        "/api/admin/workers/install?staged=true",
        "application/zip",
        app_zip("1.0.0", "1"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");
    assert_eq!(json["staged"], true);
    assert_eq!(json["activation"], "active");
    assert_eq!(json["prewarm"], "scheduled");
    assert!(
        wait_for(PREWARM_TIMEOUT, || instance_count(
            &state.pool,
            "zip-app",
            "1.0.0"
        ) == 1)
        .await,
        "staged install was not prewarmed: {:?}",
        state.pool.worker_stats()
    );
    assert!(
        wait_for(PREWARM_TIMEOUT, || prewarm_events(&state, "zip-app").len()
            == 1)
        .await,
        "prewarm event was not recorded: {:?}",
        prewarm_events(&state, "zip-app")
    );
    assert_eq!(factory.created_count(), 1);
}

// Failure semantics (EDG-10): a prewarm that fails to spawn records a `warn`
// operational event and does not retry; the API response still reports
// `scheduled` because it never waits for the prewarm.
#[tokio::test]
async fn failed_prewarm_records_warn_event_and_does_not_retry() {
    let root = tempfile::tempdir().unwrap();
    let factory = CountingFactory::failing();
    let state = state_with_factory(vec![root.path().to_path_buf()], factory.clone());
    let app = build_pipeline(state.clone());

    let (status, json, text) = send(
        app,
        "POST",
        "/api/admin/workers/install",
        "application/zip",
        app_zip("1.0.0", "1"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");
    assert_eq!(json["prewarm"], "scheduled");

    assert!(
        wait_for(PREWARM_TIMEOUT, || prewarm_events(&state, "zip-app").len()
            == 1)
        .await,
        "failed prewarm event was not recorded"
    );
    let event = &prewarm_events(&state, "zip-app")[0];
    assert_eq!(
        event.level,
        edger_orchestrator::observability::OperationalEventLevel::Warn
    );
    assert_eq!(event.outcome.as_deref(), Some("failed"));
    assert!(
        event
            .message
            .as_deref()
            .is_some_and(|message| message.contains("prewarm failed")),
        "failure event must carry the error: {:?}",
        event.message
    );

    // No retry: the single attempt left no instance behind and never created
    // a second isolate.
    assert!(!wait_for(NEGATIVE_WINDOW, || factory.created_count() > 1).await);
    assert_eq!(factory.created_count(), 1);
    assert_eq!(instance_count(&state.pool, "zip-app", "1.0.0"), 0);
}
