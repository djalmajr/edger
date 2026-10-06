//! EDG-10: `minProcesses` as a MAINTAINED floor.
//!
//! Two mechanisms, both deterministic (paused clock + mock backend; the
//! paused clock advances through `tokio::time::sleep` auto-advance, which
//! moves time to the next pending timer and fires it):
//!
//! 1. TTL respects the floor — when an idle instance's TTL expires and
//!    terminating it would drop the group below `min_processes`, the
//!    instance stays `Idle` and the timer is re-armed with the same
//!    `ttl_ms`; the decision is atomic with the group's living count, so a
//!    burst of simultaneous expirations can never breach the floor.
//! 2. Replenishment — when a removal (any cause) drops the group below
//!    `min_processes`, one background attempt refills it through the
//!    existing `prewarm_worker` path. No replenishment for evicted groups,
//!    shutdown, or `ttl_ms == 0` (ephemeral semantics intact).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use edger_core::{
    create_worker_ref, ExecutionKind, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, WorkerConfig, WorkerManifest, WorkerRef,
};
use edger_worker::pool::AdmissionSectionReleaseGuard;
use edger_worker::{instance::TtlArm, IsolateFactory, PoolConfig, WorkerPool, WorkerState};

/// Isolate that answers with its own id ("isolate-N"): the response body
/// proves WHICH process answered, which is the whole point of the floor
/// tests (kept vs replaced vs replenished). `fail_prepare` makes `prepare`
/// ALWAYS fail (P2 #1: spawn-failure replenishment).
struct NumberedIsolate {
    id: usize,
    slow_fetch_ms: u64,
    fail_prepare: bool,
}

#[async_trait]
impl Isolate for NumberedIsolate {
    async fn prepare(&mut self, _config: &WorkerConfig) -> Result<(), IsolationError> {
        if self.fail_prepare {
            return Err(IsolationError::new(
                "TEST_PREPARE_FAIL",
                "test: prepare always fails",
            ));
        }
        Ok(())
    }

    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        if self.slow_fetch_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.slow_fetch_ms)).await;
        }
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(format!("isolate-{}", self.id).into()),
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
            body: Some(format!("isolate-{}", self.id).into()),
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }
}

/// Factory counting every isolate it creates: a replacement process (a
/// breach of the floor or a cold start) is always a NEW creation, so the
/// created count discriminates "kept/reused" from "recycled".
#[derive(Default)]
struct CountingFactory {
    created: AtomicUsize,
    slow_fetch_ms: u64,
    fail_prepare: bool,
}

impl CountingFactory {
    fn new(slow_fetch_ms: u64) -> Self {
        Self {
            created: AtomicUsize::new(0),
            slow_fetch_ms,
            fail_prepare: false,
        }
    }

    /// A backend whose `prepare` ALWAYS fails (P2 #1 fixture).
    fn failing_prepare() -> Self {
        Self {
            created: AtomicUsize::new(0),
            slow_fetch_ms: 0,
            fail_prepare: true,
        }
    }

    fn created_count(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }
}

impl IsolateFactory for CountingFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn edger_core::Isolate> {
        let id = self.created.fetch_add(1, Ordering::SeqCst) + 1;
        Box::new(NumberedIsolate {
            id,
            slow_fetch_ms: self.slow_fetch_ms,
            fail_prepare: self.fail_prepare,
        })
    }
}

fn floor_worker_ref(
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

/// Same as `floor_worker_ref`, with the circuit breaker EXPLICITLY disabled
/// (`circuit_breaker_failures = 0`) — the P2 #1 fixture: spawn failures must
/// not chain attempts even without a circuit to stop them.
fn floor_worker_ref_no_circuit(
    name: &str,
    min_processes: usize,
    max_processes: usize,
    ttl_ms: u64,
) -> WorkerRef {
    let mut worker_ref = floor_worker_ref(name, min_processes, max_processes, ttl_ms, 0);
    worker_ref.config.circuit_breaker_failures = 0;
    worker_ref
}

fn pool(factory: Arc<CountingFactory>, max_size: usize) -> WorkerPool {
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
        request_id: "floor-req".into(),
        base_href: None,
    }
}

/// Serve one request and return the body (which isolate answered: "isolate-N").
async fn fetch_body(pool: &WorkerPool, worker_ref: &WorkerRef, uri: &str) -> String {
    let res = pool
        .fetch_worker(worker_ref, req(uri), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(res.status, 200, "the floor worker must answer 200");
    let body = String::from_utf8_lossy(res.body.as_deref().unwrap_or(&[])).to_string();
    body
}

async fn fetch(pool: &WorkerPool, worker_ref: &WorkerRef, uri: &str) {
    let res = pool
        .fetch_worker(worker_ref, req(uri), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(res.status, 200, "the floor worker must answer 200");
}

/// `ttl_kept_total` of the named worker group (0 when the group is gone).
fn group_kept(pool: &WorkerPool, name: &str) -> u64 {
    pool.get_metrics()
        .worker_groups
        .iter()
        .find(|group| group.name == name)
        .map(|group| group.ttl_kept_total)
        .unwrap_or(0)
}

/// Yield the test task so spawned tasks (TTL timers, replenishment
/// attempts) can run. Paused clock: no real time elapses.
async fn settle(times: usize) {
    for _ in 0..times {
        tokio::task::yield_now().await;
    }
}

/// Advance the paused clock to the next pending timer (the TTL timers or the
/// in-flight isolate sleeps) and let everything that fired settle.
async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    settle(32).await;
}

/// (EDG-10 rev3) Poll (real time — no paused clock) until `probe` holds.
/// Bounded: a regression that never reaches the state fails instead of
/// hanging the suite. Used by the admission-seam tests, which run on a
/// multi-threaded runtime (real time, no `tokio::time::pause`).
async fn wait_until(mut probe: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if probe() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("wait_until timed out");
}

/// Serve two concurrent slow fetches: with `max_processes >= 2` the fan-out
/// creates a second instance while the first holds its dispatch lock, and
/// both end `Idle` with armed TTL timers.
async fn warm_two_idle(pool: &WorkerPool, factory: &CountingFactory, worker_ref: &WorkerRef) {
    let first = tokio::spawn({
        let pool = pool.clone();
        let worker_ref = worker_ref.clone();
        async move {
            pool.fetch_worker(&worker_ref, req("/one"), Some(ExecutionKind::FetchHandler))
                .await
        }
    });
    tokio::task::yield_now().await;
    let second = tokio::spawn({
        let pool = pool.clone();
        let worker_ref = worker_ref.clone();
        async move {
            pool.fetch_worker(&worker_ref, req("/two"), Some(ExecutionKind::FetchHandler))
                .await
        }
    });
    // Let both dispatches reach the isolate (holding their dispatch locks):
    // the second request could only be served by a SECOND instance (fan-out
    // up to `max_processes`).
    settle(16).await;
    assert_eq!(
        factory.created_count(),
        2,
        "fan-out must create a second instance"
    );
    // Finish both fetches (the paused clock jumps to the isolate sleeps):
    // both instances go Idle with armed TTL timers.
    advance(100).await;
    assert!(first.await.unwrap().is_ok());
    assert!(second.await.unwrap().is_ok());
}

// 1. `min_processes: 1`, short TTL: after SEVERAL expirations the instance
//    is still alive and Idle; the next request is served by the SAME
//    instance (no cold start, no new isolate), and each expiry that kept it
//    is counted in `ttl_kept_total`.
#[tokio::test]
async fn floor_keeps_the_instance_across_repeated_ttl_expirations() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-keep", 1, 1, 100, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    // First request: the floor instance serves it, goes Idle and arms its
    // TTL timer (a prewarmed instance only starts expiring once it has been
    // idle AFTER serving).
    fetch(&pool, &worker_ref, "/first").await;
    let kept_id = pool.worker_stats()[0].worker_id;

    // Five full TTL cycles with no traffic: every expiry keeps the instance.
    // The window is 105ms — wider than the re-arm rhythm (100ms TTL + 1ms
    // preemption barrier + ≤2ms paused-clock auto-advance drift) so exactly
    // one expiry is captured per window, and narrower than two rhythms so
    // none is skipped twice.
    for cycle in 0..5 {
        advance(105).await;
        let stats = pool.worker_stats();
        assert_eq!(
            stats.len(),
            1,
            "cycle {cycle}: the floor instance must still be alive"
        );
        assert_eq!(
            stats[0].worker_id, kept_id,
            "cycle {cycle}: same instance kept"
        );
        assert_eq!(
            stats[0].state,
            WorkerState::Idle,
            "cycle {cycle}: still Idle"
        );
    }

    // The next request is served by the SAME instance — no new isolate.
    fetch(&pool, &worker_ref, "/next").await;
    let stats = pool.worker_stats();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].worker_id, kept_id);
    assert_eq!(stats[0].request_count, 2);
    assert_eq!(
        factory.created_count(),
        1,
        "no replacement process may be spawned for a kept floor instance"
    );

    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-keep")
        .unwrap();
    assert_eq!(group.ttl_kept_total, 5, "each kept expiry is counted");
    assert_eq!(group.recycle_ttl_total, 0, "nothing may have been recycled");
}

// 2. `min 1`, TWO idle instances: of the two simultaneous expirations, one
//    may terminate (2 -> 1 still satisfies the floor) and the other must be
//    kept (1 -> 0 would breach it). Exactly one survives, Idle.
#[tokio::test]
async fn surplus_instance_expires_while_floor_survives() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(50));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-two", 1, 2, 100, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    warm_two_idle(&pool, &factory, &worker_ref).await;
    assert_eq!(pool.worker_stats().len(), 2, "two idle instances");

    // Both TTL timers expire together: one terminates, one is kept.
    advance(100).await;

    let stats = pool.worker_stats();
    assert_eq!(
        stats.len(),
        1,
        "exactly one instance may survive a simultaneous double expiry"
    );
    assert_eq!(stats[0].state, WorkerState::Idle);

    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-two")
        .unwrap();
    assert_eq!(group.recycle_ttl_total, 1, "the surplus instance expired");
    assert_eq!(group.ttl_kept_total, 1, "the floor instance was kept");
}

// 3. Deterministic burst: two timers expiring together with `min 1` and two
//    instances leave EXACTLY ONE, and repeated expiry cycles keep it alive
//    (the re-armed timer is re-kept, never terminated) — the group never
//    drops to zero between cycles.
#[tokio::test]
async fn simultaneous_expirations_never_breach_the_floor() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(50));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-burst", 1, 2, 100, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    warm_two_idle(&pool, &factory, &worker_ref).await;
    assert_eq!(pool.worker_stats().len(), 2);

    // Burst 1: two simultaneous expirations -> exactly one remains.
    advance(100).await;
    let survivor = pool.worker_stats();
    assert_eq!(
        survivor.len(),
        1,
        "the burst must leave exactly one instance"
    );
    let survivor_id = survivor[0].worker_id;

    // Bursts 2-4: the single survivor keeps being kept (timer re-armed each
    // time) — the count never dips below the floor.
    for burst in 2..=4 {
        advance(100).await;
        let stats = pool.worker_stats();
        assert_eq!(stats.len(), 1, "burst {burst}: still exactly one");
        assert_eq!(
            stats[0].worker_id, survivor_id,
            "burst {burst}: the SAME survivor is kept"
        );
        assert_eq!(stats[0].state, WorkerState::Idle);
    }

    // And the survivor serves the next request without a cold start.
    fetch(&pool, &worker_ref, "/after").await;
    let stats = pool.worker_stats();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].worker_id, survivor_id);
    assert_eq!(
        factory.created_count(),
        2,
        "no extra process was ever spawned"
    );

    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-burst")
        .unwrap();
    assert_eq!(group.recycle_ttl_total, 1, "only the surplus expired");
    assert_eq!(
        group.ttl_kept_total, 4,
        "1 (burst 1) + 3 (bursts 2-4) kept expirations"
    );
}

// 4. A removal by `max_requests` that drops the group below the floor
//    triggers ONE background replenishment with NO request in flight — and
//    the attempt revalidates the GROUP GENERATION at execution (review P2
//    #2): it refills the SAME generation that is still admitted, directly
//    (never through `get_or_create_group`, so it cannot re-admit a removed
//    identity or evict another group). With `max_requests: 1` the next
//    request retires another instance and a second attempt refills again —
//    the floor survives repeated turnover, one attempt per removal.
#[tokio::test]
async fn max_requests_removal_triggers_background_replenishment() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-refill", 2, 2, 30_000, 1);

    // Prewarm the floor (2 instances, both Idle): isolate-1 and isolate-2.
    pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(factory.created_count(), 2);

    // isolate-1 (the first round-robin pick) reaches maxRequests on the
    // first request and retires; the group drops below the floor (2 -> 1)
    // and the pool refills the SAME generation in the BACKGROUND (nothing
    // below sends a request before the assertions).
    assert_eq!(fetch_body(&pool, &worker_ref, "/retire").await, "isolate-1");
    settle(64).await;

    let stats = pool.worker_stats();
    assert_eq!(
        stats.len(),
        2,
        "the floor is re-established without any request"
    );
    assert!(
        stats.iter().all(|s| s.state == WorkerState::Idle),
        "both instances are idle and ready"
    );
    assert_eq!(
        factory.created_count(),
        3,
        "exactly one replenishment attempt"
    );

    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-refill")
        .unwrap();
    assert_eq!(group.replenish_total, 1, "one replenishment triggered");
    assert_eq!(group.recycle_max_requests_total, 1);

    // The next request is served by a member of the SAME generation (the
    // background attempt, not a cold start: no new process appears before
    // this request). It then retires again (maxRequests=1) and a SECOND
    // attempt refills the floor — one attempt per removal, no more.
    let body = fetch_body(&pool, &worker_ref, "/next").await;
    assert_ne!(
        body, "isolate-1",
        "the retired instance must not serve again"
    );
    settle(64).await;
    let stats = pool.worker_stats();
    assert_eq!(stats.len(), 2, "the floor survives the second turnover");
    assert!(
        stats.iter().all(|s| s.state == WorkerState::Idle),
        "the refilled instance is idle and ready"
    );
    assert_eq!(
        factory.created_count(),
        4,
        "one attempt per removal, no more"
    );

    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-refill")
        .unwrap();
    assert_eq!(group.replenish_total, 2, "one replenishment per removal");
    assert_eq!(group.recycle_max_requests_total, 2);
}

// 4b. Generation scope (review P2 #2, complement): when the removal EMPTIES
//     the group (min 1, the only instance retires), the group leaves the
//     cache; the background attempt revalidates the generation and finds it
//     is no longer the admitted generation, so it does NOTHING — it must not
//     re-admit the identity (that could evict an unrelated group). The floor
//     is re-established by the NEXT request through the demand path, which
//     cold-starts a fresh generation.
#[tokio::test]
async fn emptied_group_attempt_does_not_readmit_the_identity() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-empty", 1, 1, 30_000, 1);

    // The only instance reaches maxRequests and retires: the group is
    // emptied and leaves the cache; the scheduled attempt must not
    // re-create it in the background.
    assert_eq!(fetch_body(&pool, &worker_ref, "/retire").await, "isolate-1");
    settle(64).await;

    assert_eq!(
        factory.created_count(),
        1,
        "the attempt sees the generation left the cache and does nothing"
    );
    assert!(
        pool.worker_stats().iter().all(|s| s.name != "floor-empty"),
        "the identity is not re-admitted without traffic"
    );

    // The next request re-establishes the floor through the demand path: a
    // fresh generation cold-starts (isolate-2) and answers 200.
    assert_eq!(fetch_body(&pool, &worker_ref, "/next").await, "isolate-2");
    settle(64).await;
    assert_eq!(
        factory.created_count(),
        2,
        "only the demand path created the next process"
    );
}

// 5a. `ttl: 0` (ephemeral): the current semantics are intact — the minimum
//     only prewarms at startup, and an ephemeral removal triggers NO
//     replenishment (a new instance appears only with the next request).
#[tokio::test]
async fn ephemeral_ttl_zero_has_no_replenishment() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-ephemeral", 1, 1, 0, 0);

    fetch(&pool, &worker_ref, "/one").await;
    settle(64).await;

    assert_eq!(
        pool.len(),
        0,
        "the ephemeral instance is removed after the request"
    );
    assert_eq!(
        factory.created_count(),
        1,
        "no background replenishment for ttl: 0"
    );
    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-ephemeral")
        .unwrap();
    assert_eq!(group.replenish_total, 0);

    // The next request cold-starts a fresh instance (demand path, as today)
    // and removes it again after the response (ephemeral).
    fetch(&pool, &worker_ref, "/two").await;
    assert_eq!(
        factory.created_count(),
        2,
        "the next request, not the pool, creates the next instance"
    );
    assert_eq!(pool.len(), 0, "and the ephemeral instance is removed again");
}

// 5b. Shutdown: a removal while the pool is shutting down triggers NO
//     replenishment.
#[tokio::test]
async fn shutdown_blocks_replenishment() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-shutdown", 1, 1, 30_000, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    let instance = pool.get_or_create(&worker_ref).await.unwrap();
    assert_eq!(factory.created_count(), 1);

    let drain = pool.shutdown();
    if let Some(handle) = drain {
        handle.await.unwrap();
    }

    // A removal arriving during shutdown must not refill anything.
    pool.remove_instance(&instance);
    settle(64).await;

    assert_eq!(
        factory.created_count(),
        1,
        "no replenishment after shutdown"
    );
    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-shutdown")
        .unwrap();
    assert_eq!(group.replenish_total, 0);
}

// 5c. LRU-evicted group: a removal of an instance whose group was evicted by
//     capacity triggers NO replenishment (the eviction drain owns that
//     generation; the identity may re-admit a fresh group on demand).
//
//     The in-flight (slow) fetch holds the evicted instance's dispatch lock,
//     so the eviction drain is PARKED on it: the evicted group's Arc is
//     still alive when the removal runs, which is what deterministically
//     exercises the `is_evicted` guard.
#[tokio::test]
async fn evicted_group_gets_no_replenishment() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(50));
    // LRU capacity 1: admitting the second worker evicts the first group.
    let pool = pool(factory.clone(), 1);
    let wa = floor_worker_ref("floor-evict-a", 1, 1, 30_000, 0);
    let wb = floor_worker_ref("floor-evict-b", 1, 1, 30_000, 0);

    pool.prewarm_worker(&wa).await.unwrap();
    let a_instance = pool.get_or_create(&wa).await.unwrap();
    assert_eq!(factory.created_count(), 1);

    // Hold the evicted instance's dispatch lock with an in-flight fetch.
    let inflight = tokio::spawn({
        let pool = pool.clone();
        let wa = wa.clone();
        async move {
            pool.fetch_worker(&wa, req("/hold"), Some(ExecutionKind::FetchHandler))
                .await
        }
    });
    settle(16).await;

    // Admit B: A's group is evicted (marked) and the background drain parks
    // on the held dispatch lock (keeping the evicted group alive).
    pool.prewarm_worker(&wb).await.unwrap();
    settle(16).await;
    assert_eq!(factory.created_count(), 2);

    // A removal of the evicted instance must not resurrect A.
    pool.remove_instance(&a_instance);
    settle(64).await;

    assert_eq!(
        factory.created_count(),
        2,
        "no replenishment for an evicted group"
    );
    assert!(
        pool.worker_stats()
            .iter()
            .all(|stats| stats.name != "floor-evict-a"),
        "the evicted generation is not re-created"
    );
    assert!(
        pool.worker_stats()
            .iter()
            .any(|stats| stats.name == "floor-evict-b"),
        "the admitted worker is untouched"
    );
    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-evict-a")
        .unwrap();
    assert_eq!(group.replenish_total, 0);

    // Let the in-flight fetch finish and the eviction drain complete.
    advance(100).await;
    assert!(inflight.await.unwrap().is_ok());
}

// 8. (review P2 #1) A removal whose SPAWN failed NEVER triggers a
//    replenishment, from where it comes: with `prepare` always failing,
//    floor 1, positive TTL and the circuit breaker EXPLICITLY disabled
//    (`circuit_breaker_failures = 0`), one removal yields EXACTLY ONE
//    creation attempt — after many yields and clock advances, with no
//    requests. (Old behavior: each failed placeholder removal scheduled its
//    own successor — an unbounded retry loop without traffic or backoff.)
#[tokio::test]
async fn spawn_failure_never_triggers_replenishment() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::failing_prepare());
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref_no_circuit("floor-nospawn", 1, 2, 100);

    // The only attempt: the prewarm's own spawn, which fails in `prepare`
    // and removes the placeholder with the SpawnFailed policy.
    let err = pool.prewarm_worker(&worker_ref).await.unwrap_err();
    assert!(err.to_string().contains("TEST_PREPARE_FAIL"));
    assert_eq!(factory.created_count(), 1, "exactly one creation attempt");

    // Many yields (the old loop scheduled a successor task per removal) and
    // several TTL windows: no new attempt may appear without traffic.
    settle(128).await;
    advance(100).await;
    settle(128).await;
    advance(100).await;

    assert_eq!(
        factory.created_count(),
        1,
        "a failed spawn must not schedule another attempt (no implicit retry)"
    );
    assert_eq!(pool.len(), 0);
    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-nospawn")
        .unwrap();
    assert_eq!(
        group.replenish_total, 0,
        "the failed placeholder removal must not count a replenishment"
    );
}

// 9. (review P2 #2) The replenishment task revalidates the GROUP
//    GENERATION at execution: LRU capacity 1, A (floor 2) loses an
//    instance (task scheduled but NOT executed yet), B is admitted (A is
//    evicted), then the task runs: B stays in the cache, A is NOT
//    re-admitted, and no creation happens for A. (Old behavior: the task
//    went through `get_or_create_group`, which created a fresh generation
//    for A and could evict B without any traffic.)
#[tokio::test]
async fn replenishment_task_does_not_readmit_evicted_generation() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    // LRU capacity 1: admitting B evicts A's group.
    let pool = pool(factory.clone(), 1);
    let wa = floor_worker_ref("floor-gen-a", 2, 2, 30_000, 0);
    let wb = floor_worker_ref("floor-gen-b", 1, 1, 30_000, 0);

    pool.prewarm_worker(&wa).await.unwrap();
    assert_eq!(factory.created_count(), 2, "A prewarms its floor (2)");

    // A loses one instance: the replenishment task is SCHEDULED (synchronous
    // removal — the task cannot have run yet on this runtime).
    let a_instance = pool.get_or_create(&wa).await.unwrap();
    pool.remove_instance(&a_instance);
    let metrics = pool.get_metrics();
    assert_eq!(
        metrics
            .worker_groups
            .iter()
            .find(|group| group.name == "floor-gen-a")
            .unwrap()
            .replenish_total,
        1,
        "the removal scheduled one attempt"
    );

    // Admit B: capacity 1 evicts A's generation (marked + drained in the
    // background) BEFORE the replenishment task is allowed to run.
    pool.prewarm_worker(&wb).await.unwrap();
    assert_eq!(factory.created_count(), 3, "B prewarms one");

    // NOW let the scheduled task (and the eviction drain) run.
    settle(128).await;

    assert_eq!(
        factory.created_count(),
        3,
        "no creation for the evicted generation A"
    );
    let stats = pool.worker_stats();
    assert!(
        stats.iter().any(|s| s.name == "floor-gen-b"),
        "B must still be in the cache"
    );
    assert!(
        stats.iter().all(|s| s.name != "floor-gen-a"),
        "A must not be re-admitted by the stale task"
    );
    let metrics = pool.get_metrics();
    assert_eq!(
        metrics
            .worker_groups
            .iter()
            .find(|group| group.name == "floor-gen-a")
            .unwrap()
            .replenish_total,
        1,
        "still exactly the one dispatched attempt"
    );
}

// 10. (review P2 #3) Timer GENERATION: a deterministic barrier between the
//     Keep decision and the re-arm install (the cooperative yield inside
//     `on_ttl_expired`'s Keep path). A request completes in that window;
//     its timer owns the window. At the end there is EXACTLY ONE live
//     timer and no expiry attributable to the old (stale) timer:
//     advancing the clock to where the stale re-arm would have fired
//     produces no extra decision, and the request's timer fires exactly
//     once (Keep, instance alive).
//
//     With an old-style fire path (no generation check), the stale timer
//     would also fire at that deadline — an extra expiry decision per
//     stale task — and the assertion below would fail.
#[tokio::test]
async fn keep_rearm_yields_to_request_timer_generation() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-gen-timer", 1, 2, 100, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    // Request #1: the instance goes Idle and arms timer A (deadline t+100).
    fetch(&pool, &worker_ref, "/first").await;
    let inst = pool.get_or_create(&worker_ref).await.unwrap();
    assert_eq!(factory.created_count(), 1);

    // Let A fire at t+100: the timer task claims its generation, makes the
    // Keep decision, then PARKS on the 1ms preemption barrier (a real timer
    // sleep — with the paused clock it cannot elapse while this task keeps
    // the runtime busy). Wait until the decision is observable
    // (`ttl_kept_total >= 1`) so request #2 is guaranteed to complete
    // strictly INSIDE the decision→re-arm window, no matter the wake order
    // of the two tasks at the shared deadline.
    tokio::time::sleep(Duration::from_millis(100)).await;
    while group_kept(&pool, "floor-gen-timer") == 0 {
        settle(8).await;
    }

    // Request #2 completes INSIDE the window: it cancels the (claimed)
    // timer, runs, and installs its own timer B (deadline t+200).
    fetch(&pool, &worker_ref, "/second").await;

    // Yield so the parked timer task resumes and reaches its re-arm: the
    // generation was bumped by the request (cancel + B's arm), so the
    // stale re-arm must be dropped — B is the only live timer.
    settle(64).await;

    // The stale re-arm would have fired at t+200 (armed at ≈t+100 with the
    // same 100ms TTL) — the SAME deadline as B. If it existed, an extra
    // expiry decision would appear here.
    tokio::time::sleep(Duration::from_millis(100)).await;
    settle(64).await;

    // Generation audit (deterministic kill for "ignore the generation on
    // firing"): exactly 6 bumps are legitimate by this point — 3 per
    // request (start cancel, complete cancel, arm). The fired task parked
    // on the 1ms barrier re-armed only at t+101 — AFTER the request's
    // cancel/arm — where the generation check drops it (no bump). A firing
    // path that ignores the generation re-arms at t+101 regardless and
    // bumps the generation to 7 (and its stale timer would later add an
    // extra expiry at t+201).
    assert_eq!(
        inst.ttl_timer_generation(),
        6,
        "a stale timer must not bump the generation (gen={})",
        inst.ttl_timer_generation()
    );

    let stats = pool.worker_stats();
    assert_eq!(stats.len(), 1, "the instance survived the window");
    assert_eq!(stats[0].worker_id, inst.id());
    assert_eq!(stats[0].state, WorkerState::Idle);

    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "floor-gen-timer")
        .unwrap();
    // At most: the Keep decision made while the barrier was parked (1) plus
    // B's own expiry at t+200 (1). A stale re-arm firing at the same
    // deadline would push this to >= 3.
    assert!(
        group.ttl_kept_total <= 2,
        "no expiry may be attributable to the stale timer, kept={}",
        group.ttl_kept_total
    );
    assert_eq!(group.recycle_ttl_total, 0, "nothing may have expired out");
    assert_eq!(
        factory.created_count(),
        1,
        "no replacement process was ever needed"
    );
}

// 11. (review rev2 P2 #1) A stale fired timer's Keep re-arm must not take
//     over a window it no longer owns. The slot's `claimed` field is SHARED
//     state: a newer task's claim overwrites it, so a re-arm that compares
//     `claimed` against `slot.gen` without the task's OWN generation would
//     pass for a stale task. Interleaving (each step is one side of the
//     race, held here by the test):
//
//     * A fires, claims its generation (gen_a) and decides Keep — then
//       suspends on the barrier (its re-arm is the deferred call below);
//     * a request completes in the window: it cancels A's (claimed) timer
//       and installs its own (B);
//     * B fires and claims ITS generation (gen_b); B's re-arm is now the
//       suspended one;
//     * release A: it must neither alter the slot nor install a timer;
//     * release B: only B re-arms.
#[tokio::test]
async fn stale_keep_rearm_cannot_take_over_a_newer_generation() {
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-gen-steal", 1, 2, 60_000, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    let inst = pool.get_or_create(&worker_ref).await.unwrap();
    inst.set_state(WorkerState::Idle);

    // A: a fresh window is armed; A fires and claims its generation. A's
    // Keep decision is made and A suspends on the barrier — its re-arm is
    // the deferred call below (the dummy spawn stands in for the timer
    // task A would install).
    let dummy = |gen: u64| {
        tokio::spawn(async move {
            let _ = gen;
        })
    };
    let gen_a = inst
        .arm_and_install(TtlArm::New, dummy)
        .expect("A is armed");
    assert!(
        inst.claim_ttl_timer(gen_a),
        "A fires and claims its generation"
    );

    // The request completes in the window: it cancels A's (claimed) timer
    // and installs its own (B). B fires and claims ITS generation; B's
    // re-arm is now the suspended one.
    inst.cancel_ttl_timer();
    let gen_b = inst
        .arm_and_install(TtlArm::New, dummy)
        .expect("B is armed");
    assert!(
        inst.claim_ttl_timer(gen_b),
        "B fires and claims its generation (claimed is now gen_b)"
    );

    // Release A first: `claimed` alone would say "gen_b == slot.gen" and let
    // A through; A carries its OWN generation (gen_a), which is no longer
    // the slot's. A must not alter the slot or install a timer.
    assert!(
        inst.arm_and_install(TtlArm::KeepRearm(gen_a), dummy)
            .is_none(),
        "A's stale re-arm must be dropped (it carries gen_a, the slot holds gen_b)"
    );
    assert_eq!(
        inst.ttl_timer_generation(),
        gen_b,
        "A's rejected re-arm must not bump the generation"
    );

    // Release B: only B re-arms — `slot.gen` and `slot.claimed` both still
    // equal B's own generation.
    assert_eq!(
        inst.arm_and_install(TtlArm::KeepRearm(gen_b), dummy),
        Some(gen_b.wrapping_add(1)),
        "B re-arms its own generation"
    );
    assert_eq!(inst.ttl_timer_generation(), gen_b.wrapping_add(1));
    assert_eq!(factory.created_count(), 1, "nothing else ran");
}

// 12. (review rev2 P2 #2) The replenishment attempt is scheduled while the
//     generation is still open, then the group is CLOSED before the attempt
//     runs (this is what shutdown and recycle_worker do: `close_queue` +
//     drained-set snapshot, one critical section with the admission). The
//     attempt must admit NOTHING into the closed group — no placeholder
//     creation, no prepare, nothing outside the drained set — even though
//     its generation revalidation passed at scheduling time.
#[tokio::test]
async fn closed_group_rejects_replenishment_admission() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-close-reject", 2, 2, 30_000, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(factory.created_count(), 2);
    let remaining = pool.get_or_create(&worker_ref).await.unwrap();
    // Hold the generation's Arc across the close (the real-world counterpart
    // is the drain/another path keeping it alive): the attempt's WEAK must
    // promote, so the admission — not a Weak upgrade failure — is what
    // rejects it.
    let group = pool
        .worker_group(&worker_ref)
        .expect("the admitted generation is in the cache");

    // A removal drops the group below the floor: the attempt is scheduled
    // (the generation is still open and admitted when it is).
    pool.remove_instance(&remaining);

    // The close runs while the attempt is still queued: the group is closed
    // IN PLACE (still admitted in the cache) — the exact state the
    // close+snapshot section of shutdown/recycle leaves behind.
    group.close_queue();
    settle(64).await; // release the attempt

    assert_eq!(
        factory.created_count(),
        2,
        "no placeholder may be admitted into a closed group"
    );
    assert_eq!(
        group.instances_snapshot().len(),
        1,
        "the closed group keeps only its pre-existing instance"
    );
    let stats = pool.worker_stats();
    assert_eq!(stats.len(), 1, "the remaining instance stays admitted");
    assert_eq!(stats[0].name, "floor-close-reject");
}

// 12b. (review rev2 P2 #2) Same contract through the REAL close path —
//     `shutdown`: close + drained-set snapshot + cache clear run before the
//     (queued) attempt executes; the attempt admits nothing, and the drain
//     terminates every instance of the group.
#[tokio::test]
async fn shutdown_rejects_replenishment_admission() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-close-shutdown", 2, 2, 30_000, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(factory.created_count(), 2);
    let remaining = pool.get_or_create(&worker_ref).await.unwrap();
    // Keep the generation alive: the cache is cleared by the shutdown, so
    // without this Arc the attempt would only fail at the Weak promotion.
    let _group = pool
        .worker_group(&worker_ref)
        .expect("the admitted generation is in the cache");

    // The attempt is scheduled…
    pool.remove_instance(&remaining);
    // …and the shutdown runs BEFORE it executes (close + snapshot are one
    // critical section with the admission).
    let drain = pool
        .shutdown()
        .expect("a drain handle under a tokio runtime");
    settle(64).await; // the attempt runs and must refuse
    drain.await.unwrap(); // the drain terminates the group's instances

    assert_eq!(
        factory.created_count(),
        2,
        "no placeholder may be admitted after shutdown"
    );
    assert!(
        pool.worker_stats().is_empty(),
        "nothing survives outside the drain"
    );
}

// 12c. (review rev2 P2 #2) Same contract through `recycle_worker`: the
//     drained set is exactly the group's instances at close time — a
//     replenishment admitted after the close would show up as an extra
//     creation and an extra recycled instance.
#[tokio::test]
async fn recycle_worker_rejects_replenishment_admission() {
    tokio::time::pause();
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-close-recycle", 2, 2, 30_000, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(factory.created_count(), 2);
    let remaining = pool.get_or_create(&worker_ref).await.unwrap();
    let _group = pool
        .worker_group(&worker_ref)
        .expect("the admitted generation is in the cache");

    // The attempt is scheduled…
    pool.remove_instance(&remaining);
    // …and the recycle runs before it executes (remove + close + snapshot
    // are one critical section with the admission, before the drain await).
    let recycled = pool.recycle_worker("floor-close-recycle", None).await;
    settle(64).await; // the attempt runs and must refuse

    assert_eq!(
        recycled, 1,
        "the drained set is exactly the instance alive at close time"
    );
    assert_eq!(
        factory.created_count(),
        2,
        "no placeholder may be admitted after recycle"
    );
    assert!(
        pool.worker_stats().is_empty(),
        "nothing survives outside the drain"
    );
}

// 13. (review rev3 P2) close vs HELD admission section — `shutdown`
//     variant. The existing close tests (12b/12c) run the close while the
//     attempt is still QUEUED (the close wins before any validation) —
//     they exercise the initial guards, not the `close_admission`
//     serialization. This fixture holds the attempt INSIDE the admission
//     section (after generation revalidation, before placeholder
//     admission, still holding `close_admission`) and verifies:
//
//     * the close fired from ANOTHER task must WAIT on the held section
//       (the group is not closed while the section holds the lock);
//     * after release, the placeholder the section admits is in the
//       close-time drained set — drained and terminated, nothing left
//       outside it.
//
//     Mutation (rev3): removing the `close_admission` synchronization
//     (keeping the `closed` guards) lets the close complete while the
//     section is held — both the "close waits" assertion and the drained-
//     set coverage fail.
//
//     Multi-threaded runtime + real time: while the section is held,
//     three workers are needed — the paused section (holding
//     `close_admission`) polls with a real-time sleep on its worker, the
//     close task blocks on the mutex on its own, and the test (arm/
//     release) runs on the third.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn close_waits_for_held_admission_section_and_drains_admitted_placeholder() {
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-seam-shutdown", 2, 2, 30_000, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(factory.created_count(), 2);
    let remaining = pool.get_or_create(&worker_ref).await.unwrap();
    // Keep the generation alive across the close (the shutdown clears the
    // cache): the drained-set proof below reads the group this Arc points
    // to.
    let group = pool
        .worker_group(&worker_ref)
        .expect("the admitted generation is in the cache");

    // (rev4 P2 #1) Obtain and ARM the seam BEFORE the removal: the
    // removal schedules the attempt on ANOTHER worker of the
    // multi-threaded runtime, which may reach the seam while `armed` is
    // still false — arming first guarantees the only scheduled attempt
    // finds the hook armed.
    let seam = pool.admission_section_seam();
    seam.arm();
    // The removal drops the group below the floor and schedules the
    // attempt — the only attempt this pool will ever schedule.
    pool.remove_instance(&remaining);
    // (rev4 P2 #2) Release is guaranteed by a scope guard: a failed
    // assertion (unwinding) still drops the guard and releases the seam,
    // so the attempt is never left parked in the hold.
    let release_guard = AdmissionSectionReleaseGuard::new(seam);
    wait_until(|| seam.is_paused()).await;
    // The attempt is INSIDE the section, after validation, holding
    // `close_admission`.

    // Fire the shutdown from ANOTHER task: its close section (close +
    // drained-set snapshot) must wait on the held section.
    let close_task = tokio::spawn({
        let pool = pool.clone();
        async move { pool.shutdown() }
    });
    // Give the close task time to reach the lock, then verify it is
    // WAITING: the close section cannot have run — it needs the lock the
    // paused section holds. (Fixed settle, not a condition wait: while
    // the section is held, `is_closed` legitimately stays false.)
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !group.is_closed(),
        "the close must wait for the held admission section"
    );

    // Release the section: it admits the placeholder and drops the lock;
    // the close then runs and its snapshot must include the placeholder.
    release_guard.release();
    wait_until(|| group.is_closed()).await;
    let drain = close_task
        .await
        .unwrap()
        .expect("a drain handle under a tokio runtime");
    drain.await.unwrap();

    // The placeholder WAS admitted (the section ran to completion)…
    assert_eq!(
        factory.created_count(),
        3,
        "the held section admitted its placeholder"
    );
    // …and it entered the drained set: the group holds exactly the
    // pre-existing instance + the placeholder, and BOTH were terminated
    // by the drain — nothing prepared or alive outside the drained set.
    let drained = group.instances_snapshot();
    assert_eq!(
        drained.len(),
        2,
        "the drained set is the remaining instance plus the admitted placeholder"
    );
    assert!(
        drained
            .iter()
            .all(|instance| instance.state() == WorkerState::Terminated),
        "every instance of the drained set was terminated by the drain"
    );
    assert!(
        pool.worker_stats().is_empty(),
        "nothing survives outside the drain"
    );
    // (rev4 P2 #2) The hold was released in time — the 10s deadline was
    // never hit (a hit would mean the release path was lost).
    assert!(
        !seam.timed_out(),
        "the hold was released within the deadline"
    );
}

// 13b. (review rev3 P2) Same fixture through `recycle_worker`: the
//      recycle's close section (remove + close + drained-set snapshot)
//      must wait on the held section, and the released section's
//      placeholder must be in the recycled (drained) set — `recycled`
//      counts exactly the instances alive at close time. (Same 3-worker
//      layout as 13.)
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn recycle_waits_for_held_admission_section_and_drains_admitted_placeholder() {
    let factory = Arc::new(CountingFactory::new(0));
    let pool = pool(factory.clone(), 8);
    let worker_ref = floor_worker_ref("floor-seam-recycle", 2, 2, 30_000, 0);

    pool.prewarm_worker(&worker_ref).await.unwrap();
    assert_eq!(factory.created_count(), 2);
    let remaining = pool.get_or_create(&worker_ref).await.unwrap();
    let group = pool
        .worker_group(&worker_ref)
        .expect("the admitted generation is in the cache");

    // (rev4 P2 #1) Same ordering as 13: arm BEFORE the removal schedules
    // the attempt (it may run on another worker at any moment).
    let seam = pool.admission_section_seam();
    seam.arm();
    pool.remove_instance(&remaining);
    // (rev4 P2 #2) Release guaranteed by a scope guard (see 13).
    let release_guard = AdmissionSectionReleaseGuard::new(seam);
    wait_until(|| seam.is_paused()).await;

    // Fire the recycle from ANOTHER task: its close section must wait on
    // the held section (before its first await).
    let close_task = tokio::spawn({
        let pool = pool.clone();
        async move { pool.recycle_worker("floor-seam-recycle", None).await }
    });
    // Give the recycle task time to reach the lock, then verify it is
    // WAITING (same fixed-settle reasoning as 13).
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !group.is_closed(),
        "the recycle must wait for the held admission section"
    );

    // Release: the section admits the placeholder and drops the lock; the
    // recycle then closes and drains — its drained set must include the
    // placeholder.
    release_guard.release();
    wait_until(|| group.is_closed()).await;
    let recycled = close_task.await.unwrap();

    assert_eq!(
        factory.created_count(),
        3,
        "the held section admitted its placeholder"
    );
    assert_eq!(
        recycled, 2,
        "the recycled (drained) set is the remaining instance plus the admitted placeholder"
    );
    let drained = group.instances_snapshot();
    assert_eq!(
        drained.len(),
        2,
        "both drained instances are of this generation"
    );
    assert!(
        drained
            .iter()
            .all(|instance| instance.state() == WorkerState::Terminated),
        "every instance of the drained set was terminated by the recycle drain"
    );
    assert!(
        pool.worker_stats().is_empty(),
        "nothing survives outside the drain"
    );
    // (rev4 P2 #2) Same deadline proof as 13.
    assert!(
        !seam.timed_out(),
        "the hold was released within the deadline"
    );
}
