//! Readmission after LRU capacity eviction (fix-lru-readmission).
//!
//! Eviction by capacity must not block the evicted `app@version` permanently:
//! the next request cold-starts a fresh group while the group cap and the
//! admission/queue/circuit/cancel/TTL machinery are preserved. The evicted
//! group's idle instances are drained at eviction (their TTL timer tasks hold
//! instance Arcs, so dropping the group alone would keep orphaned copies
//! alive), and a late TTL of the old group must never remove the readmitted
//! group under the same key.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    create_worker_ref, ExecutionKind, Isolate, SerializedRequest, SerializedResponse, WorkerConfig,
    WorkerManifest, WorkerRef, WorkerResponse,
};
use edger_worker::{
    lru::{GroupInsertOutcome, ReservedSlot, WorkerGroup, WorkerLru},
    IsolateFactory, PoolConfig, Supervisor, WorkerInstance, WorkerPool, WorkerState,
};

/// Test isolate: counts `prepare` (cold starts) and `terminate` (drains) per
/// worker identity, and holds any `/hold` request until the factory's notify
/// fires (the notified future is registered BEFORE the `started` flag, so no
/// wakeup can be lost).
#[derive(Clone)]
struct TestFactory {
    counters: Arc<Mutex<HashMap<String, Counters>>>,
    notify: Arc<tokio::sync::Notify>,
    started: Arc<AtomicBool>,
}

impl TestFactory {
    fn new() -> Self {
        Self {
            counters: Arc::new(Mutex::new(HashMap::new())),
            notify: Arc::new(tokio::sync::Notify::new()),
            started: Arc::new(AtomicBool::new(false)),
        }
    }

    fn counter(&self, key: &str) -> Counters {
        self.counters
            .lock()
            .expect("test counters lock")
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    fn prepares(&self, key: &str) -> usize {
        self.counter(key).prepares.load(Ordering::SeqCst)
    }

    fn terminated(&self, key: &str) -> usize {
        self.counter(key).terminated.load(Ordering::SeqCst)
    }

    fn created(&self, key: &str) -> usize {
        self.counter(key).created.load(Ordering::SeqCst)
    }

    fn started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    fn release(&self) {
        self.notify.notify_waiters();
    }
}

#[derive(Clone, Default)]
struct Counters {
    prepares: Arc<AtomicUsize>,
    terminated: Arc<AtomicUsize>,
    created: Arc<AtomicUsize>,
}

struct TestIsolate {
    counters: Counters,
    notify: Arc<tokio::sync::Notify>,
    started: Arc<AtomicBool>,
}

impl TestIsolate {
    fn new(counters: Counters, notify: Arc<tokio::sync::Notify>, started: Arc<AtomicBool>) -> Self {
        Self {
            counters,
            notify,
            started,
        }
    }
}

#[async_trait]
impl Isolate for TestIsolate {
    async fn prepare(&mut self, _config: &WorkerConfig) -> Result<(), edger_core::IsolationError> {
        self.counters.prepares.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), edger_core::IsolationError> {
        self.counters.terminated.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        if req.uri == "/hold" {
            let notified = self.notify.notified();
            self.started.store(true, Ordering::SeqCst);
            notified.await;
        } else if req.uri == "/hold-slow" {
            // Longer than the pool's 5s shutdown drain budget, to prove the
            // eviction drain does not skip a legitimate long call.
            self.started.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5_500)).await;
        }
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(req.uri)),
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(format!("spa:{path}"))),
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(req, config).await
    }
}

impl IsolateFactory for TestFactory {
    fn create_isolate(&self, worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        let key = format!("{}@{}", worker_ref.name, worker_ref.version);
        let counters = self
            .counters
            .lock()
            .expect("test counters lock")
            .entry(key.clone())
            .or_default()
            .clone();
        // Only instances created through the pool count here: an isolate
        // built via `direct_isolate` is not a group instance.
        counters.created.fetch_add(1, Ordering::SeqCst);
        Box::new(TestIsolate::new(
            counters,
            Arc::clone(&self.notify),
            Arc::clone(&self.started),
        ))
    }
}

impl TestFactory {
    /// Build an isolate that shares the factory's counters without creating a
    /// group through the pool (used to simulate an old-generation instance).
    fn direct_isolate(&self, key: &str) -> TestIsolate {
        let counters = self
            .counters
            .lock()
            .expect("test counters lock")
            .entry(key.to_string())
            .or_default()
            .clone();
        TestIsolate::new(
            counters,
            Arc::clone(&self.notify),
            Arc::clone(&self.started),
        )
    }
}

fn sample_req(uri: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: uri.into(),
        headers: vec![],
        body: None,
        request_id: "readmission-req".into(),
        base_href: None,
    }
}

fn worker_ref(dir: &str, name: &str, ttl_ms: u64) -> WorkerRef {
    let mut reference = create_worker_ref(
        PathBuf::from(dir),
        WorkerManifest {
            name: name.into(),
            ..Default::default()
        },
    )
    .unwrap();
    reference.config.ttl_ms = ttl_ms;
    reference
}

fn versioned_worker_ref(dir: &str, name: &str, version: &str, ttl_ms: u64) -> WorkerRef {
    let mut reference = create_worker_ref(
        PathBuf::from(dir),
        WorkerManifest {
            name: name.into(),
            version: Some(version.into()),
            ..Default::default()
        },
    )
    .unwrap();
    reference.config.ttl_ms = ttl_ms;
    reference
}

fn pool_with(factory: TestFactory, max_size: usize) -> WorkerPool {
    WorkerPool::with_factory(
        PoolConfig {
            max_size,
            ephemeral_concurrency: 4,
            ephemeral_queue_limit: 8,
        },
        Arc::new(factory),
    )
}

async fn wait_until(cond: impl Fn() -> bool) {
    for _ in 0..10_000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("condition not met within 10s");
}

#[tokio::test]
async fn eviction_by_capacity_readmits_identity_with_cold_start() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 3);
    let w1 = worker_ref("/workers/r-1", "r-1", 30_000);
    let w2 = worker_ref("/workers/r-2", "r-2", 30_000);
    let w3 = worker_ref("/workers/r-3", "r-3", 30_000);
    let w4 = worker_ref("/workers/r-4", "r-4", 30_000);
    let w5 = worker_ref("/workers/r-5", "r-5", 30_000);

    for worker in [&w1, &w2, &w3, &w4, &w5] {
        pool.fetch_worker(worker, sample_req("/"), Some(ExecutionKind::FetchHandler))
            .await
            .unwrap();
    }
    assert_eq!(pool.len(), 3, "group cap must hold after churn");

    // The evicted identity must serve again without recycle/restart.
    let again = pool
        .fetch_worker(&w1, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(again.status, 200);
    assert_eq!(
        pool.len(),
        3,
        "readmission must not grow the pool beyond the cap"
    );
    assert_eq!(
        factory.prepares("r-1@latest"),
        2,
        "readmission must cold-start a fresh instance"
    );
    // The evicted idle instance is drained at eviction instead of lingering
    // on its TTL timer outside the LRU.
    wait_until(|| factory.terminated("r-1@latest") >= 1).await;
}

#[tokio::test]
async fn churn_beyond_default_capacity_readmits_first_identity() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 32);
    let workers: Vec<WorkerRef> = (0..34)
        .map(|i| {
            worker_ref(
                &format!("/workers/churn-{i:02}"),
                &format!("churn-{i:02}"),
                30_000,
            )
        })
        .collect();

    for worker in &workers {
        pool.fetch_worker(worker, sample_req("/"), Some(ExecutionKind::FetchHandler))
            .await
            .unwrap();
    }
    assert_eq!(pool.len(), 32, "the 32-group cap must hold");

    let first = pool
        .fetch_worker(
            &workers[0],
            sample_req("/"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .unwrap();
    assert_eq!(
        first.status, 200,
        "the first identity must be readable again"
    );
    assert_eq!(pool.len(), 32);
    assert_eq!(factory.prepares("churn-00@latest"), 2);
}

// EDG-6 (metrics amendment): the per-worker request counters live on the
// worker IDENTITY (name/namespace/version) for the life of the edger process
// — a capacity eviction and readmission of the same identity must ACCUMULATE
// on the same series (ok=2), not restart it (the monotonic behavior
// Prometheus expects). A → B (capacity 1 evicts A) → A (cold readmission).
#[tokio::test]
async fn evicted_identity_readmission_accumulates_the_group_request_counter() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 1);
    let wa = worker_ref("/workers/mra", "mra", 30_000);
    let wb = worker_ref("/workers/mrb", "mrb", 30_000);

    pool.fetch_worker(&wa, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    pool.fetch_worker(&wb, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(pool.len(), 1, "capacity 1: B evicts A");
    assert_eq!(factory.prepares("mrb@latest"), 1);
    pool.fetch_worker(&wa, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(factory.prepares("mra@latest"), 2, "readmission cold-starts");

    let groups = pool.get_metrics().worker_groups;
    let group = groups
        .iter()
        .find(|group| group.name == "mra")
        .unwrap_or_else(|| panic!("group mra missing from the metrics snapshot"));
    assert_eq!(
        group.requests_ok_total, 2,
        "ok accumulates on the same identity series across eviction/readmission"
    );
    assert_eq!(group.requests_error_total, 0);
    assert_eq!(group.requests_cancelled_total, 0);
}

#[tokio::test]
async fn fetch_worker_stream_readmits_evicted_identity() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 3);
    let w1 = worker_ref("/workers/s-1", "s-1", 30_000);
    let w2 = worker_ref("/workers/s-2", "s-2", 30_000);
    let w3 = worker_ref("/workers/s-3", "s-3", 30_000);
    let w4 = worker_ref("/workers/s-4", "s-4", 30_000);

    for worker in [&w1, &w2, &w3, &w4] {
        let response = pool
            .fetch_worker_stream(worker, sample_req("/"), Some(ExecutionKind::FetchHandler))
            .await
            .unwrap();
        assert!(
            matches!(response, WorkerResponse::Buffered(_)),
            "test isolate only buffers"
        );
    }

    let again = pool
        .fetch_worker_stream(&w1, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    match again {
        WorkerResponse::Buffered(buffered) => assert_eq!(buffered.status, 200),
        WorkerResponse::Streamed(_) => panic!("test isolate only buffers"),
    }
    assert_eq!(pool.len(), 3);
    assert_eq!(factory.prepares("s-1@latest"), 2);
}

#[tokio::test]
async fn same_app_different_versions_survive_eviction() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 2);
    let v1 = versioned_worker_ref("/workers/app/1.0.0", "app", "1.0.0", 30_000);
    let v2 = versioned_worker_ref("/workers/app/1.1.0", "app", "1.1.0", 30_000);
    let v3 = versioned_worker_ref("/workers/app/1.2.0", "app", "1.2.0", 30_000);

    for version in [&v1, &v2, &v3] {
        pool.fetch_worker(version, sample_req("/"), Some(ExecutionKind::FetchHandler))
            .await
            .unwrap();
    }

    // v1 was evicted (idle, drained) and must cold-start again.
    let v1_again = pool
        .fetch_worker(&v1, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(v1_again.status, 200);
    assert_eq!(factory.prepares("app@1.0.0"), 2);

    // v2 was LRU-evicted by the v1 readmission and must come back too.
    let v2_again = pool
        .fetch_worker(&v2, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(v2_again.status, 200);
    assert_eq!(pool.len(), 2);
}

#[tokio::test]
async fn inflight_on_evicted_group_completes_while_readmission_serves() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 2);
    let wa = worker_ref("/workers/h-a", "h-a", 30_000);
    let wb = worker_ref("/workers/h-b", "h-b", 30_000);
    let wc = worker_ref("/workers/h-c", "h-c", 30_000);

    let hold_pool = pool.clone();
    let hold_wa = wa.clone();
    let hold = tokio::spawn(async move {
        hold_pool
            .fetch_worker(
                &hold_wa,
                sample_req("/hold"),
                Some(ExecutionKind::FetchHandler),
            )
            .await
    });
    wait_until(|| factory.started()).await;

    // Evict h-a's group while its request is in flight.
    pool.fetch_worker(&wb, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    pool.fetch_worker(&wc, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();

    // Readmission of the evicted identity must not be blocked by the
    // in-flight request on the old group.
    let ping = pool
        .fetch_worker(&wa, sample_req("/ping"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(ping.status, 200);

    // The in-flight request must complete, not be killed.
    factory.release();
    let hold_result = hold.await.unwrap().unwrap();
    assert_eq!(hold_result.status, 200);
    assert_eq!(
        hold_result.body.as_ref().map(Bytes::as_ref),
        Some(b"/hold".as_ref())
    );
    assert_eq!(pool.len(), 2);
}

#[tokio::test]
async fn concurrent_readmission_same_identity_all_succeed() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 2);
    let wa = worker_ref("/workers/c-a", "c-a", 30_000);
    let wb = worker_ref("/workers/c-b", "c-b", 30_000);
    let wc = worker_ref("/workers/c-c", "c-c", 30_000);

    for worker in [&wa, &wb, &wc] {
        pool.fetch_worker(worker, sample_req("/"), Some(ExecutionKind::FetchHandler))
            .await
            .unwrap();
    }
    // c-a's evicted idle instance is drained at eviction.
    wait_until(|| factory.terminated("c-a@latest") >= 1).await;

    let (first, second, third, fourth) = tokio::join!(
        pool.fetch_worker(&wa, sample_req("/1"), Some(ExecutionKind::FetchHandler)),
        pool.fetch_worker(&wa, sample_req("/2"), Some(ExecutionKind::FetchHandler)),
        pool.fetch_worker(&wa, sample_req("/3"), Some(ExecutionKind::FetchHandler)),
        pool.fetch_worker(&wa, sample_req("/4"), Some(ExecutionKind::FetchHandler)),
    );
    for result in [first, second, third, fourth] {
        assert_eq!(result.unwrap().status, 200);
    }
    assert_eq!(pool.len(), 2);
    assert!(
        factory.prepares("c-a@latest") >= 2,
        "at least one cold start after the eviction"
    );
}

#[tokio::test]
async fn late_cleanup_of_old_generation_does_not_remove_new_group() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 2);
    let wa = worker_ref("/workers/t-a", "t-a", 30_000);
    let wb = worker_ref("/workers/t-b", "t-b", 30_000);
    let wc = worker_ref("/workers/t-c", "t-c", 30_000);

    // Evict t-a and readmit it: the cache holds the new generation's group.
    for worker in [&wa, &wb, &wc] {
        pool.fetch_worker(worker, sample_req("/"), Some(ExecutionKind::FetchHandler))
            .await
            .unwrap();
    }
    let readmit = pool
        .fetch_worker(&wa, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(readmit.status, 200);
    let fresh = pool.get_or_create(&wa).await.unwrap();

    // Simulate the old generation's late cleanup: an instance that survived
    // the eviction-time drain (e.g. one created on the old group by a queued
    // waiter after the drain's snapshot) runs its TTL path now, after the
    // readmission. The real timer path is
    // `on_ttl_expired` -> `reserve_ttl_termination` -> `cleanup` ->
    // `pool::remove_instance`, which looks up the SAME key in the cache.
    let old = Arc::new(WorkerInstance::new(
        wa.clone(),
        Box::new(factory.direct_isolate("t-a@latest")),
    ));
    old.set_state(WorkerState::Idle);
    Supervisor::on_ttl_expired(&old, &pool).await.unwrap();

    // The new generation's group and instance must be untouched: the late
    // cleanup found the readmitted group under the same key and removed no
    // member of it (membership is by instance id).
    let after = pool.get_or_create(&wa).await.unwrap();
    assert_eq!(
        after.id(),
        fresh.id(),
        "late cleanup of the old generation must not remove the new group"
    );
    let final_fetch = pool
        .fetch_worker(&wa, sample_req("/final"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(final_fetch.status, 200);
    assert_eq!(old.state(), WorkerState::Terminated);
}

#[tokio::test]
async fn recycle_and_shutdown_still_work_around_evicted_identities() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 2);
    let wa = worker_ref("/workers/x-a", "x-a", 30_000);
    let wb = worker_ref("/workers/x-b", "x-b", 30_000);
    let wc = worker_ref("/workers/x-c", "x-c", 30_000);

    for worker in [&wa, &wb, &wc] {
        pool.fetch_worker(worker, sample_req("/"), Some(ExecutionKind::FetchHandler))
            .await
            .unwrap();
    }

    // x-a is evicted: recycle finds no cached group, and the next request
    // still cold-starts the identity.
    assert_eq!(pool.recycle_worker("x-a", None).await, 0);
    pool.fetch_worker(&wa, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();

    // Recycle of a cached worker still forces a cold start afterwards. Use a
    // fresh identity inserted last so it is the newest LRU entry and cannot
    // have been evicted by the churn above.
    let wd = worker_ref("/workers/x-d", "x-d", 30_000);
    pool.fetch_worker(&wd, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    pool.fetch_worker(&wd, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(pool.recycle_worker("x-d", None).await, 1);
    pool.fetch_worker(&wd, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(factory.prepares("x-d@latest"), 2);

    pool.shutdown();
    let err = pool
        .fetch_worker(&wa, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("shut down"));
}

/// A legitimate in-flight call longer than the 5s shutdown drain budget must
/// not be skipped by the eviction drain (which would leave the instance with
/// its TTL timer as an orphan): the drain waits for the call to complete and
/// then cleans the instance up.
#[tokio::test]
async fn long_inflight_beyond_drain_budget_is_cleaned_up_not_orphaned() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 2);
    let wa = worker_ref("/workers/l-a", "l-a", 30_000);
    let wb = worker_ref("/workers/l-b", "l-b", 30_000);
    let wc = worker_ref("/workers/l-c", "l-c", 30_000);

    let hold_pool = pool.clone();
    let hold_wa = wa.clone();
    let hold = tokio::spawn(async move {
        hold_pool
            .fetch_worker(
                &hold_wa,
                sample_req("/hold-slow"),
                Some(ExecutionKind::FetchHandler),
            )
            .await
    });
    wait_until(|| factory.started()).await;

    // Evict l-a's group while the (slow) call is in flight.
    pool.fetch_worker(&wb, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    pool.fetch_worker(&wc, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();

    // The call outlasts the drain budget; it must complete, not be aborted.
    let hold_result = hold.await.unwrap().unwrap();
    assert_eq!(hold_result.status, 200);

    // The drain then cleaned the now-idle instance instead of leaving it on
    // its TTL timer outside the LRU.
    wait_until(|| factory.terminated("l-a@latest") >= 1).await;
    // And no extra cold start happened on the old generation.
    assert_eq!(factory.prepares("l-a@latest"), 1);
}

/// A request already queued on the evicted group must be served (never
/// `Shutdown`/500): the evicted group admits no new slots or instances, so
/// the waiter wakes, retries, and resolves the identity against the new
/// generation — without any new orphan instance on the old group.
#[tokio::test]
async fn waiter_on_evicted_group_resolves_new_generation_no_orphan() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 2);
    let wa = worker_ref("/workers/q-a", "q-a", 30_000);
    let wb = worker_ref("/workers/q-b", "q-b", 30_000);
    let wc = worker_ref("/workers/q-c", "q-c", 30_000);

    // Request A holds the only process of q-a's group.
    let hold_pool = pool.clone();
    let hold_wa = wa.clone();
    let hold = tokio::spawn(async move {
        hold_pool
            .fetch_worker(
                &hold_wa,
                sample_req("/hold"),
                Some(ExecutionKind::FetchHandler),
            )
            .await
    });
    wait_until(|| factory.started()).await;

    // Request B targets the same identity and enqueues behind A.
    let second_pool = pool.clone();
    let second_wa = wa.clone();
    let second = tokio::spawn(async move {
        second_pool
            .fetch_worker(
                &second_wa,
                sample_req("/second"),
                Some(ExecutionKind::FetchHandler),
            )
            .await
    });
    // Let B park in the queue before the eviction.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Evict q-a's group: B must not fail, and no instance may be created on
    // the old group (it admits no new slots once evicted).
    pool.fetch_worker(&wb, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    pool.fetch_worker(&wc, sample_req("/"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();

    // Readmission of the identity (B's retry or this call creates the new
    // generation; first insert wins).
    let ping = pool
        .fetch_worker(&wa, sample_req("/ping"), Some(ExecutionKind::FetchHandler))
        .await
        .unwrap();
    assert_eq!(ping.status, 200);

    // Release A: both requests must complete successfully.
    factory.release();
    let hold_result = hold.await.unwrap();
    let second_result = second.await.unwrap();
    assert_eq!(
        hold_result.as_ref().unwrap().status,
        200,
        "in-flight request must not be killed"
    );
    assert_eq!(
        second_result.as_ref().unwrap().status,
        200,
        "queued request must be served, not turned into Shutdown/500"
    );

    // Exactly two instances were ever created for the identity: one per
    // generation. A third would be the orphan created on the old group.
    assert_eq!(
        factory.created("q-a@latest"),
        2,
        "no new instance on the evicted group"
    );
    assert_eq!(factory.prepares("q-a@latest"), 2);

    // Teardown: the drained old instance is accounted for, cap holds.
    wait_until(|| factory.terminated("q-a@latest") >= 1).await;
    assert_eq!(pool.len(), 2);
}

/// Concurrent readmission of the same evicted identity must share a single
/// winning group (first insert wins; losers are discarded, never replacing
/// the winner and orphaning its queue/max-process capacity). Runs on a
/// multi-thread runtime with a barrier so the misses actually contend on
/// get/create/insert. `get_or_create` is used instead of a full dispatch so
/// the assertion is deterministic: the winner group has exactly one
/// unspawned instance, so every concurrent caller must receive that same
/// instance. (Dispatch-level contention is covered by
/// `concurrent_readmission_same_identity_all_succeed`, where the winner may
/// scale its instance set up to `max_processes`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_readmission_shares_single_group() {
    let factory = TestFactory::new();
    let pool = pool_with(factory.clone(), 2);
    let wa = worker_ref("/workers/m-a", "m-a", 30_000);
    let wb = worker_ref("/workers/m-b", "m-b", 30_000);
    let wc = worker_ref("/workers/m-c", "m-c", 30_000);

    for worker in [&wa, &wb, &wc] {
        pool.fetch_worker(worker, sample_req("/"), Some(ExecutionKind::FetchHandler))
            .await
            .unwrap();
    }
    wait_until(|| factory.terminated("m-a@latest") >= 1).await;

    const N: usize = 6;
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let mut handles = Vec::new();
    for _ in 0..N {
        let pool = pool.clone();
        let wa = wa.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            pool.get_or_create(&wa).await
        }));
    }
    let mut ids: Vec<uuid::Uuid> = Vec::new();
    for handle in handles {
        ids.push(handle.await.unwrap().unwrap().id());
    }
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        1,
        "all concurrent readmissions must resolve to the single winning group's instance"
    );
    // Candidates and prepared processes are validated separately: every
    // concurrent miss builds an unspawned candidate group (1 instance each)
    // before losing the insert race, so `created` counts candidates in
    // [old + winner, old + all N candidates] — while the number of
    // ACTUALLY PREPARED (cold-started) processes stays exact: only the
    // initial fetch ever spawned, and no candidate group is ever spawned.
    let created = factory.created("m-a@latest");
    assert!(
        (2..=(1 + N)).contains(&created),
        "expected 1..={N} unspawned candidate groups (created={created})"
    );
    assert_eq!(
        factory.prepares("m-a@latest"),
        1,
        "no duplicated or candidate cold start"
    );
    assert_eq!(pool.len(), 2, "group cap preserved");
}

/// After `mark_evicted`, the group refuses new slots and new instances even
/// with free capacity: the flag is stored under the instances lock and
/// re-checked under that same lock by `reserve_slot_with_min` / `ensure_min`
/// _processes, so the linearized state is observed once the lock is held.
/// (The check-before-lock window itself is a race; this test pins the
/// post-mark invariant, and the behavioral tests cover it end to end.)
#[test]
fn evicted_group_refuses_new_slots_and_instances() {
    let group = WorkerGroup::new(Vec::new());
    let wa = worker_ref("/workers/s-a", "s-a", 30_000);

    // Sanity: a live group admits a slot and grows on demand.
    match group.reserve_slot_with_min(2, 0, || {
        Arc::new(WorkerInstance::new(
            wa.clone(),
            Box::new(TestIsolate::new(
                Counters::default(),
                Arc::new(tokio::sync::Notify::new()),
                Arc::new(AtomicBool::new(false)),
            )),
        ))
    }) {
        ReservedSlot::Acquired { .. } => {}
        _ => panic!("a live group with free capacity must admit a slot"),
    }
    assert_eq!(group.instances_snapshot().len(), 1);

    group.mark_evicted();

    // No new slot: `create` must not even run (it would orphan an instance
    // outside the cache).
    let called = Arc::new(AtomicBool::new(false));
    let called_clone = Arc::clone(&called);
    match group.reserve_slot_with_min(4, 2, || {
        called_clone.store(true, Ordering::SeqCst);
        Arc::new(WorkerInstance::new(
            wa.clone(),
            Box::new(TestIsolate::new(
                Counters::default(),
                Arc::new(tokio::sync::Notify::new()),
                Arc::new(AtomicBool::new(false)),
            )),
        ))
    }) {
        ReservedSlot::Unavailable => {}
        _ => panic!("an evicted group must not admit new slots"),
    }
    assert!(
        !called.load(Ordering::SeqCst),
        "create closure must not run"
    );
    assert_eq!(
        group.instances_snapshot().len(),
        1,
        "no new instance created"
    );

    // Prewarm growth is refused the same way.
    let grown = group.ensure_min_processes(3, || {
        Arc::new(WorkerInstance::new(
            wa.clone(),
            Box::new(TestIsolate::new(
                Counters::default(),
                Arc::new(tokio::sync::Notify::new()),
                Arc::new(AtomicBool::new(false)),
            )),
        ))
    });
    assert_eq!(grown.len(), 1, "an evicted group must not grow");
}

/// Deterministic point-of-insertion test: a second insert for the same key
/// must not replace the winner (the old behavior orphaned the winner's queue
/// and max-process capacity for the same identity).
#[test]
fn lru_insert_first_insert_wins_without_replacing_winner() {
    use edger_worker::WorkerCacheKey;

    let cache = WorkerLru::new(1);
    let key = WorkerCacheKey {
        dir: PathBuf::from("/workers/app"),
        name: "app".into(),
        version: "1.0.0".into(),
    };
    let g1 = Arc::new(WorkerGroup::new(Vec::new()));
    let g2 = Arc::new(WorkerGroup::new(Vec::new()));

    match cache.insert_group(key.clone(), Arc::clone(&g1)) {
        GroupInsertOutcome::Inserted { evicted: None } => {}
        _ => panic!("first insert must win with no eviction"),
    }
    match cache.insert_group(key.clone(), Arc::clone(&g2)) {
        GroupInsertOutcome::Existing(winner) => {
            assert!(
                Arc::ptr_eq(&winner, &g1),
                "the winner must be the first-inserted group"
            );
        }
        _ => panic!("second insert for the same key must return Existing"),
    }
    assert!(
        Arc::ptr_eq(&cache.get_group(&key).unwrap(), &g1),
        "the cached group must remain the first one (no replace)"
    );
    assert_eq!(cache.group_count(), 1);
}
