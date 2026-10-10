//! EDG-15: opt-in process warmup (the manifest `warmup` field).
//!
//! A worker whose manifest declares `warmup` has, right after every process
//! created by `prewarm_worker` or the min-processes replenishment — still
//! holding the instance's dispatch lock and BEFORE the `Ready -> Idle`
//! transition — exactly ONE synthetic `GET` (the admin health-check request
//! shape, marked `x-edger-health-check: warmup`), so the first user request
//! finds the code already executed once.
//!
//! The warmup never counts: no `request_count` increment (the `maxRequests`
//! budget stays whole for users), no `Supervisor` transition (no TTL
//! arming), no request counters or metrics. A failed warmup dispatch
//! terminates the process and removes the instance WITHOUT scheduling a
//! replenishment (no spawn -> warmup failure -> replenish loop), and never
//! fails the prewarm or the replenishment attempt.
//!
//! Mock isolates + paused clock, following the `min_processes_floor`
//! pattern.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use edger_core::{
    create_worker_ref, ExecutionKind, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, WorkerConfig, WorkerManifest, WorkerRef, WorkerWarmup,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool, WorkerState};

/// One request as seen by an isolate (order preserved per isolate).
#[derive(Clone, Debug, PartialEq, Eq)]
struct SeenRequest {
    method: String,
    uri: String,
    /// Raw `x-edger-health-check` header value, when present.
    health_check: Option<String>,
    request_id: String,
}

impl SeenRequest {
    fn is_warmup(&self) -> bool {
        self.health_check.as_deref() == Some("warmup")
    }
}

/// Per-isolate state shared with the factory: what each process saw, and
/// whether it was terminated.
struct IsolateState {
    id: usize,
    requests: Mutex<Vec<SeenRequest>>,
    /// Set by the isolate when it ENTERS a held warmup request (the pool
    /// holds the instance's dispatch lock inside it).
    warmup_started: AtomicBool,
    /// Set when `terminate` runs on this isolate.
    terminated: AtomicBool,
}

struct WarmupMock {
    state: Arc<IsolateState>,
    /// Status answered for the synthetic warmup request (default 200; 500
    /// for the non-2xx scenario).
    warmup_status: Arc<AtomicU32>,
    /// The warmup dispatch fails (crash / protocol-error fixture).
    fail_warmup: Arc<AtomicBool>,
    /// Only this Nth-created process has a failing warmup; zero disables it.
    fail_warmup_for_nth_created_process: Arc<AtomicUsize>,
    /// The warmup request is HELD until the test releases it (g/h).
    hold_warmup: Arc<AtomicBool>,
    warmup_release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Isolate for WarmupMock {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let health_check = req.headers.iter().find_map(|(name, value)| {
            name.eq_ignore_ascii_case("x-edger-health-check")
                .then(|| value.clone())
        });
        let seen = SeenRequest {
            method: req.method.clone(),
            uri: req.uri.clone(),
            health_check,
            request_id: req.request_id.clone(),
        };
        self.state.requests.lock().unwrap().push(seen.clone());

        // User requests always answer 200 with this isolate's id: the body
        // proves WHICH process answered.
        if !seen.is_warmup() {
            return Ok(SerializedResponse {
                status: 200,
                headers: vec![],
                body: Some(format!("isolate-{}", self.state.id).into()),
            });
        }

        if self.fail_warmup.load(Ordering::SeqCst)
            || self
                .fail_warmup_for_nth_created_process
                .load(Ordering::SeqCst)
                == self.state.id
        {
            return Err(IsolationError::new(
                "TEST_WARMUP_FAIL",
                "test: the warmup dispatch always fails",
            ));
        }

        if self.hold_warmup.load(Ordering::SeqCst) {
            // The warmup is in flight (the pool holds the dispatch lock):
            // signal the test and wait for the explicit release. `enable`
            // keeps the receiver notifiable before the await, so a release
            // that lands between the flag store and the await cannot be
            // lost.
            let release = self.warmup_release.notified();
            tokio::pin!(release);
            release.as_mut().enable();
            self.state.warmup_started.store(true, Ordering::SeqCst);
            release.await;
        }

        Ok(SerializedResponse {
            status: self.warmup_status.load(Ordering::SeqCst) as u16,
            headers: vec![],
            body: Some(format!("isolate-{}", self.state.id).into()),
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
            headers: vec![],
            body: Some(format!("isolate-{}", self.state.id).into()),
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        self.state.terminated.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Factory counting every isolate it creates and keeping their shared
/// states: a replacement process is always a NEW creation, so the created
/// count discriminates "reused" from "recycled". The warmup knobs are
/// runtime-switchable.
#[derive(Default)]
struct WarmupFactory {
    created: AtomicUsize,
    states: Mutex<Vec<Arc<IsolateState>>>,
    warmup_status: Arc<AtomicU32>,
    fail_warmup: Arc<AtomicBool>,
    fail_warmup_for_nth_created_process: Arc<AtomicUsize>,
    hold_warmup: Arc<AtomicBool>,
    warmup_release: Arc<tokio::sync::Notify>,
}

impl WarmupFactory {
    fn new() -> Self {
        Self {
            created: AtomicUsize::new(0),
            states: Mutex::new(Vec::new()),
            warmup_status: Arc::new(AtomicU32::new(200)),
            fail_warmup: Arc::new(AtomicBool::new(false)),
            fail_warmup_for_nth_created_process: Arc::new(AtomicUsize::new(0)),
            hold_warmup: Arc::new(AtomicBool::new(false)),
            warmup_release: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn created_count(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }

    fn isolate_states(&self) -> Vec<Arc<IsolateState>> {
        self.states.lock().unwrap().clone()
    }

    /// Every request seen by every isolate, in per-isolate order (the
    /// factory creates one isolate at a time, so the creation order is the
    /// process order).
    fn all_requests(&self) -> Vec<SeenRequest> {
        self.isolate_states()
            .iter()
            .flat_map(|state| state.requests.lock().unwrap().clone())
            .collect()
    }

    fn warmup_requests(&self) -> Vec<SeenRequest> {
        self.all_requests()
            .into_iter()
            .filter(|request| request.is_warmup())
            .collect()
    }

    fn warmup_started(&self) -> bool {
        self.isolate_states()
            .iter()
            .any(|state| state.warmup_started.load(Ordering::SeqCst))
    }

    fn release_warmup(&self) {
        self.warmup_release.notify_waiters();
    }

    fn terminated_ids(&self) -> Vec<usize> {
        self.isolate_states()
            .iter()
            .filter(|state| state.terminated.load(Ordering::SeqCst))
            .map(|state| state.id)
            .collect()
    }

    fn set_warmup_status(&self, status: u16) {
        self.warmup_status.store(status as u32, Ordering::SeqCst);
    }

    fn fail_warmup_from_now_on(&self) {
        self.fail_warmup.store(true, Ordering::SeqCst);
    }

    fn fail_warmup_for_nth_created_process(&self, nth: usize) {
        self.fail_warmup_for_nth_created_process
            .store(nth, Ordering::SeqCst);
    }

    fn hold_warmup_from_now_on(&self) {
        self.hold_warmup.store(true, Ordering::SeqCst);
    }
}

impl IsolateFactory for WarmupFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn edger_core::Isolate> {
        let id = self.created.fetch_add(1, Ordering::SeqCst) + 1;
        let state = Arc::new(IsolateState {
            id,
            requests: Mutex::new(Vec::new()),
            warmup_started: AtomicBool::new(false),
            terminated: AtomicBool::new(false),
        });
        self.states.lock().unwrap().push(Arc::clone(&state));
        Box::new(WarmupMock {
            state,
            warmup_status: Arc::clone(&self.warmup_status),
            fail_warmup: Arc::clone(&self.fail_warmup),
            fail_warmup_for_nth_created_process: Arc::clone(
                &self.fail_warmup_for_nth_created_process,
            ),
            hold_warmup: Arc::clone(&self.hold_warmup),
            warmup_release: Arc::clone(&self.warmup_release),
        })
    }
}

/// Worker ref with the opt-in `warmup` manifest field (path + `5s` timeout)
/// and the given floor/TTL/maxRequests knobs.
fn warmup_worker_ref(
    name: &str,
    path: &str,
    min_processes: usize,
    max_processes: usize,
    ttl_ms: u64,
    max_requests: u32,
) -> WorkerRef {
    let mut worker_ref = create_worker_ref(
        std::path::PathBuf::from(format!("/workers/{name}")),
        WorkerManifest {
            name: name.into(),
            warmup: Some(WorkerWarmup {
                path: path.into(),
                timeout: Some("5s".into()),
            }),
            ..Default::default()
        },
    )
    .unwrap();
    worker_ref.kind = ExecutionKind::FetchHandler;
    worker_ref.config.min_processes = min_processes;
    worker_ref.config.max_processes = max_processes;
    worker_ref.config.ttl_ms = ttl_ms;
    worker_ref.config.max_requests = max_requests;
    worker_ref
}

/// Same fixture WITHOUT the `warmup` manifest field (regression: the
/// current behavior must be byte-identical).
fn plain_worker_ref(
    name: &str,
    min_processes: usize,
    max_processes: usize,
    ttl_ms: u64,
    max_requests: u32,
) -> WorkerRef {
    let mut worker_ref = create_worker_ref(
        std::path::PathBuf::from(format!("/workers/{name}")),
        WorkerManifest {
            name: name.into(),
            ..Default::default()
        },
    )
    .unwrap();
    worker_ref.kind = ExecutionKind::FetchHandler;
    worker_ref.config.min_processes = min_processes;
    worker_ref.config.max_processes = max_processes;
    worker_ref.config.ttl_ms = ttl_ms;
    worker_ref.config.max_requests = max_requests;
    worker_ref
}

fn pool(factory: Arc<WarmupFactory>, max_size: usize) -> WorkerPool {
    WorkerPool::with_factory(
        PoolConfig {
            max_size,
            ephemeral_concurrency: 4,
            ephemeral_queue_limit: 8,
        },
        factory,
    )
}

fn req(uri: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: uri.into(),
        headers: vec![],
        body: None,
        request_id: "warm-req".into(),
        base_href: None,
    }
}

async fn fetch_body(pool: &WorkerPool, worker_ref: &WorkerRef, uri: &str) -> String {
    let res = pool
        .fetch_worker(worker_ref, req(uri), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(res.status, 200, "the warm worker must answer 200");
    String::from_utf8_lossy(res.body.as_deref().unwrap_or(&[])).to_string()
}

async fn fetch(pool: &WorkerPool, worker_ref: &WorkerRef, uri: &str) {
    let res = pool
        .fetch_worker(worker_ref, req(uri), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(res.status, 200, "the warm worker must answer 200");
}

/// Yield the test task so spawned tasks (prewarm, replenishment) can run.
/// Paused clock: no real time elapses.
async fn settle(times: usize) {
    for _ in 0..times {
        tokio::task::yield_now().await;
    }
}

/// Advance the paused clock by `ms` and let everything that fired settle.
async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    settle(32).await;
}

/// Poll (bounded) until `probe` holds. Paused clock: each 1 ms sleep
/// advances the virtual clock far less than the 5 s warmup timeout fixture,
/// so the probe never trips the warmup's own deadline on the way in.
async fn wait_until(mut probe: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if probe() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("wait_until timed out");
}

// a. Prewarm with `warmup`: the isolate receives exactly ONE synthetic
//    request — GET, the manifest path, `x-edger-health-check: warmup` —
//    before the instance goes Idle. Afterwards the instance's
//    `request_count` is still 0 (the maxRequests budget is whole) and the
//    group's request counters are untouched.
#[tokio::test]
async fn prewarm_warms_the_fresh_process_before_it_goes_idle() {
    tokio::time::pause();
    let factory = Arc::new(WarmupFactory::new());
    let pool = pool(factory.clone(), 8);
    let worker_ref = warmup_worker_ref("warm-prewarm", "/dashboard", 1, 1, 30_000, 0);

    let spawned = pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(spawned, 1, "the warmed process counts as prewarmed");

    let warmups = factory.warmup_requests();
    assert_eq!(warmups.len(), 1, "exactly one synthetic request, no more");
    assert_eq!(warmups[0].method, "GET", "the warmup is always a GET");
    assert_eq!(warmups[0].uri, "/dashboard", "the manifest warmup path");
    assert!(
        warmups[0].request_id.starts_with("warmup-"),
        "the warmup request carries its own request id"
    );
    assert_eq!(
        factory.all_requests().len(),
        1,
        "nothing else reached the process"
    );

    let stats = pool.worker_stats();
    assert_eq!(stats.len(), 1);
    assert_eq!(
        stats[0].state,
        WorkerState::Idle,
        "the warmed instance is Idle"
    );
    assert_eq!(
        stats[0].request_count, 0,
        "the warmup does not consume the maxRequests budget"
    );

    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "warm-prewarm")
        .unwrap();
    assert_eq!(group.request_total, 0, "no request counters for the warmup");
    assert_eq!(group.requests_ok_total, 0);
    assert_eq!(group.requests_error_total, 0);
    assert_eq!(group.requests_cancelled_total, 0);
}

// b. Replenishment after a `maxRequests` retirement: the REPLACEMENT process
//    is warmed too (one warmup per process, per spawn).
#[tokio::test]
async fn replenishment_warms_the_replacement_process() {
    tokio::time::pause();
    let factory = Arc::new(WarmupFactory::new());
    let pool = pool(factory.clone(), 8);
    let worker_ref = warmup_worker_ref("warm-refill", "/", 1, 1, 30_000, 1);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(
        factory.warmup_requests().len(),
        1,
        "the prewarmed process is warmed once"
    );

    // The only instance reaches maxRequests (1) and retires; the group
    // drops below the floor and the background replenishment spawns the
    // replacement, which must be warmed before it goes Idle.
    let body = fetch_body(&pool, &worker_ref, "/retire").await;
    assert_eq!(body, "isolate-1");
    settle(64).await;

    let stats = pool.worker_stats();
    assert_eq!(
        stats.len(),
        1,
        "the floor is re-established without any request"
    );
    assert_eq!(stats[0].state, WorkerState::Idle);
    assert_eq!(
        factory.created_count(),
        2,
        "exactly one replenishment attempt"
    );
    assert_eq!(
        factory.warmup_requests().len(),
        2,
        "each process is warmed exactly once"
    );

    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "warm-refill")
        .unwrap();
    assert_eq!(group.replenish_total, 1);
    assert_eq!(group.recycle_max_requests_total, 1);

    // The next request is served by the warmed REPLACEMENT process.
    let body = fetch_body(&pool, &worker_ref, "/next").await;
    assert_eq!(
        body, "isolate-2",
        "the retired process must not serve again"
    );
}

// The refill batch must keep going after one process fails its warmup. A
// first warmup failure during initial prewarm leaves one process from this
// min-2 group; removing it through the EDG-13 empty-group path makes one
// replenishment attempt admit two placeholders. The first replacement then
// fails warmup, but the second must still be spawned and warmed.
#[tokio::test]
async fn replenishment_continues_the_batch_after_a_warmup_failure() {
    tokio::time::pause();
    let factory = Arc::new(WarmupFactory::new());
    factory.fail_warmup_for_nth_created_process(1);
    let pool = pool(factory.clone(), 8);
    let worker_ref = warmup_worker_ref("warm-refill-batch", "/", 2, 2, 30_000, 0);

    // The first process fails warmup; the second is the only surviving
    // floor member, so the cache still holds the same min-2 generation.
    assert_eq!(pool.prewarm_worker(&worker_ref).await.unwrap(), 1);
    let group = pool.worker_group(&worker_ref).unwrap();
    let survivor = group
        .instances_snapshot()
        .into_iter()
        .find(|instance| instance.state() == WorkerState::Idle)
        .expect("the second prewarm process survives");
    assert_eq!(factory.created_count(), 2);

    // Fail only process 3, the first admitted placeholder in the refill
    // batch. Process 4 must still receive and pass its warmup.
    factory.fail_warmup_for_nth_created_process(3);
    pool.remove_instance(&survivor);
    assert!(group.is_empty(), "the floored group remains admitted empty");
    settle(128).await;

    let instances = group.instances_snapshot();
    let idle = instances
        .iter()
        .filter(|instance| instance.state() == WorkerState::Idle)
        .count();
    let creating = instances
        .iter()
        .filter(|instance| instance.state() == WorkerState::Creating)
        .count();
    assert_eq!(idle, 1, "the second batch process is warmed and Idle");
    assert_eq!(creating, 0, "the batch leaves no Creating placeholders");
    assert_eq!(
        factory.created_count(),
        4,
        "the refill admitted two processes"
    );
    assert_eq!(factory.terminated_ids(), vec![1, 3]);
    assert_eq!(
        factory.isolate_states()[3]
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.is_warmup())
            .count(),
        1,
        "the second batch process receives its warmup"
    );

    let metrics = pool.get_metrics();
    let group_metrics = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "warm-refill-batch")
        .unwrap();
    assert_eq!(group_metrics.replenish_total, 1);

    // Warmup failure does not schedule another attempt, even after the
    // paused clock passes the surviving process's TTL.
    advance(30_000).await;
    let states = group.instances_snapshot();
    assert_eq!(
        states
            .iter()
            .filter(|instance| instance.state() == WorkerState::Idle)
            .count(),
        1
    );
    let metrics = pool.get_metrics();
    let group_metrics = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "warm-refill-batch")
        .unwrap();
    assert_eq!(group_metrics.replenish_total, 1);
}

// c. Regression: without `warmup` NO synthetic request is sent — the
//     prewarm/replenish behavior is identical to the current one.
#[tokio::test]
async fn without_warmup_no_synthetic_request_is_sent() {
    tokio::time::pause();
    let factory = Arc::new(WarmupFactory::new());
    let pool = pool(factory.clone(), 8);
    let worker_ref = plain_worker_ref("warm-none", 1, 1, 30_000, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    fetch(&pool, &worker_ref, "/one").await;
    settle(16).await;

    let requests = factory.all_requests();
    assert_eq!(
        requests.len(),
        1,
        "only the user request reached the process"
    );
    assert!(
        requests.iter().all(|request| !request.is_warmup()),
        "no synthetic (warmup) request"
    );

    let stats = pool.worker_stats();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].state, WorkerState::Idle);
    assert_eq!(stats[0].request_count, 1);

    // And a maxRequests retirement still triggers the (unwarmed)
    // replenishment exactly as before: worker 1 prewarmed (1) + worker 2
    // prewarmed (1) + the replacement (1).
    let worker_ref = plain_worker_ref("warm-none-refill", 1, 1, 30_000, 1);
    pool.prewarm_worker(&worker_ref).await.unwrap();
    fetch(&pool, &worker_ref, "/retire").await;
    settle(64).await;
    assert_eq!(factory.created_count(), 3, "the replenishment still runs");
    assert_eq!(
        factory.warmup_requests().len(),
        0,
        "and still sends no warmup"
    );
    let stats = pool.worker_stats();
    assert!(
        stats
            .iter()
            .any(|s| s.name == "warm-none-refill" && s.state == WorkerState::Idle),
        "the replenished replacement is idle"
    );
}

// d. The warmup answers a non-2xx status (500): the socket is intact, so
//    the instance goes Idle anyway (only a warn log) and serves the next
//    user request.
#[tokio::test]
async fn warmup_500_keeps_the_process_and_it_serves_the_next_request() {
    tokio::time::pause();
    let factory = Arc::new(WarmupFactory::new());
    factory.set_warmup_status(500);
    let pool = pool(factory.clone(), 8);
    let worker_ref = warmup_worker_ref("warm-500", "/", 1, 1, 30_000, 0);

    // A non-2xx warmup status only warns: the prewarm still succeeds.
    let spawned = pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(spawned, 1);

    let stats = pool.worker_stats();
    assert_eq!(stats.len(), 1);
    assert_eq!(
        stats[0].state,
        WorkerState::Idle,
        "a 500 warmup keeps the process: the socket is intact"
    );
    assert_eq!(stats[0].request_count, 0);

    // The SAME process serves the next user request.
    let body = fetch_body(&pool, &worker_ref, "/next").await;
    assert_eq!(body, "isolate-1");
    assert_eq!(
        factory.terminated_ids().len(),
        0,
        "no process was terminated"
    );
}

// e. The warmup dispatch fails: the instance is removed, the process is
//    terminated, the prewarm does not count it, and NO replenishment is
//    scheduled — after many yields and clock advances well past a
//    replenishment window, the spawn count must not change (no
//    spawn -> warmup failure -> replenish loop).
// Mutation captured: replacing `SpawnFailed` with `Regular` records a refill.
#[tokio::test]
async fn warmup_dispatch_error_removes_the_instance_without_replenishment() {
    tokio::time::pause();
    let factory = Arc::new(WarmupFactory::new());
    factory.fail_warmup_from_now_on();
    let pool = pool(factory.clone(), 8);
    let worker_ref = warmup_worker_ref("warm-fail", "/", 1, 1, 100, 0);

    // The prewarm itself must NOT fail: a warmup failure never fails the
    // boot, the rescan or the install.
    let spawned = pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(
        spawned, 0,
        "a process lost to a warmup failure is not counted as prewarmed"
    );

    // The process was terminated and the instance is gone.
    assert_eq!(
        factory.terminated_ids(),
        vec![1],
        "the failed process is terminated"
    );
    assert_eq!(pool.len(), 0, "no instance remains");

    // Assert the scheduling decision before yielding to the replenishment
    // task. If the failure path uses `Regular`, this fails immediately by
    // contract instead of letting a warmup/replenishment loop run forever.
    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "warm-fail")
        .unwrap();
    assert_eq!(
        group.replenish_total, 0,
        "the warmup-failure removal must not count a replenishment"
    );
    assert_eq!(
        group.recycle_error_total, 1,
        "the removal is recorded with the Error cause"
    );

    // Many yields (a scheduled attempt would run on another task) and
    // several TTL windows: no new attempt may appear without traffic.
    settle(128).await;
    advance(100).await;
    settle(128).await;
    advance(100).await;

    assert_eq!(
        factory.created_count(),
        1,
        "a warmup failure must not schedule another attempt (no implicit retry)"
    );
}

// f. `maxRequests: 3` with `warmup`: the process serves THREE user
//     requests before retiring — the warmup consumed no budget.
#[tokio::test]
async fn warmup_does_not_consume_the_max_requests_budget() {
    tokio::time::pause();
    let factory = Arc::new(WarmupFactory::new());
    let pool = pool(factory.clone(), 8);
    let worker_ref = warmup_worker_ref("warm-budget", "/", 1, 1, 30_000, 3);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(factory.warmup_requests().len(), 1);

    // THREE user requests on the same process (without the warmup
    // exclusion the instance would already be at its budget on the second
    // and the third request would go to a replacement).
    for (n, uri) in ["/one", "/two", "/three"].into_iter().enumerate() {
        let body = fetch_body(&pool, &worker_ref, uri).await;
        assert_eq!(body, "isolate-1", "request {n}: same process");
    }

    // The third user request reached the budget: the first process retired
    // (the warmup consumed none of it) and the background replenishment
    // refills the floor.
    assert!(
        factory.terminated_ids().contains(&1),
        "the first process retired on the third USER request"
    );
    settle(64).await;
    let body = fetch_body(&pool, &worker_ref, "/four").await;
    assert_eq!(
        body, "isolate-2",
        "the replacement serves the fourth request"
    );
    assert_eq!(
        pool.worker_stats()[0].request_count,
        1,
        "the replacement starts with a whole budget"
    );
}

// g. A user request arriving DURING a held warmup waits on the dispatch
//    lock and is dispatched only AFTER the warmup ends — in the SAME
//    process.
#[tokio::test]
async fn user_request_during_warmup_waits_and_is_served_by_the_same_process() {
    tokio::time::pause();
    let factory = Arc::new(WarmupFactory::new());
    factory.hold_warmup_from_now_on();
    let pool = pool(factory.clone(), 8);
    let worker_ref = warmup_worker_ref("warm-concurrent", "/warm", 1, 1, 30_000, 0);

    let prewarm = tokio::spawn({
        let pool = pool.clone();
        let worker_ref = worker_ref.clone();
        async move { pool.prewarm_worker(&worker_ref).await }
    });
    wait_until(|| factory.warmup_started()).await;

    // The user request arrives while the warmup is held: it must wait on
    // the dispatch lock, never run concurrently in the same process.
    let user = tokio::spawn({
        let pool = pool.clone();
        let worker_ref = worker_ref.clone();
        async move {
            pool.fetch_worker(&worker_ref, req("/user"), Some(ExecutionKind::FetchHandler))
                .await
        }
    });
    settle(32).await;
    assert_eq!(
        factory.all_requests().len(),
        1,
        "while the warmup is held, the user request has not been dispatched"
    );

    factory.release_warmup();
    let spawned = prewarm.await.unwrap().unwrap();
    assert_eq!(spawned, 1);
    let res = user.await.unwrap().unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(
        String::from_utf8_lossy(res.body.as_deref().unwrap_or(&[])),
        "isolate-1",
        "the user request was served by the same (warmed) process"
    );

    // Order inside the single process: the warmup FIRST, then the user
    // request.
    let requests = factory.all_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].is_warmup() && requests[0].uri == "/warm");
    assert!(!requests[1].is_warmup() && requests[1].uri == "/user");

    let stats = pool.worker_stats();
    assert_eq!(
        stats[0].request_count, 1,
        "the warmup consumed no maxRequests budget"
    );
    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "warm-concurrent")
        .unwrap();
    assert_eq!(group.request_total, 1, "only the user request is counted");
}

// h. Pool shutdown while a warmup is in flight (held): the shutdown
//    completes, the instance does NOT go Idle (the close drain owns it)
//    and no process is left behind.
#[tokio::test]
async fn shutdown_during_warmup_terminates_the_process_without_idling() {
    tokio::time::pause();
    let factory = Arc::new(WarmupFactory::new());
    factory.hold_warmup_from_now_on();
    let pool = pool(factory.clone(), 8);
    let worker_ref = warmup_worker_ref("warm-shutdown", "/warm", 1, 1, 30_000, 0);

    let prewarm = tokio::spawn({
        let pool = pool.clone();
        let worker_ref = worker_ref.clone();
        async move { pool.prewarm_worker(&worker_ref).await }
    });
    wait_until(|| factory.warmup_started()).await;

    // The instance exists while the warmup holds its dispatch lock.
    let instance = pool
        .worker_group(&worker_ref)
        .unwrap()
        .instances_snapshot()
        .into_iter()
        .next()
        .expect("the prewarmed instance");

    // The pool shuts down while the warmup is in flight.
    let drain = pool.shutdown();

    // The warmup finishes (200) — but the pool is shutting down: the
    // prewarm loop must NOT put the instance Idle (the close drain owns it).
    factory.release_warmup();
    settle(64).await;
    if let Some(handle) = drain {
        handle.await.unwrap();
    }
    settle(64).await;

    assert_eq!(
        instance.state(),
        WorkerState::Terminated,
        "the shut-down instance must not end up Idle"
    );
    assert!(
        factory.terminated_ids().contains(&1),
        "the process was terminated: nothing left behind"
    );
    assert_eq!(
        prewarm.await.unwrap().unwrap(),
        0,
        "an instance lost to shutdown is not counted as prewarmed"
    );
}
