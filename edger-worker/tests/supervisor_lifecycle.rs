//! Supervisor lifecycle tests (story 04.02) — written first (TDD red).

use edger_core::{create_worker_ref, ExecutionKind, SerializedRequest, WorkerManifest, WorkerRef};
use edger_isolation::MockIsolate;
use edger_worker::{IsolateFactory, PoolConfig, Supervisor, WorkerEvent, WorkerPool, WorkerState};
use std::path::PathBuf;
use std::sync::Arc;

struct MockFactory;

impl IsolateFactory for MockFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn sample_req(uri: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: uri.into(),
        headers: vec![],
        body: None,
        request_id: "life-req".into(),
        base_href: None,
    }
}

fn make_worker_ref(dir: PathBuf, name: &str, ttl_ms: u64, max_requests: u32) -> WorkerRef {
    let mut worker_ref = create_worker_ref(
        dir,
        WorkerManifest {
            name: name.into(),
            ..Default::default()
        },
    )
    .unwrap();
    worker_ref.config.ttl_ms = ttl_ms;
    worker_ref.config.max_requests = max_requests;
    worker_ref
}

fn pool() -> WorkerPool {
    WorkerPool::with_factory(
        PoolConfig {
            max_size: 8,
            ephemeral_concurrency: 4,
            ephemeral_queue_limit: 8,
        },
        Arc::new(MockFactory),
    )
}

#[tokio::test]
async fn creating_to_ready_requires_signal() {
    let worker_ref = make_worker_ref(PathBuf::from("/workers/creating"), "creating", 30_000, 0);
    let pool = pool();
    let instance = pool.get_or_create(&worker_ref).await.unwrap();
    assert_eq!(
        instance.state(),
        WorkerState::Creating,
        "new instance starts Creating"
    );

    let err = Supervisor::on_request_start(&instance).await.unwrap_err();
    assert!(err.to_string().contains("not ready"));

    Supervisor::spawn(&instance).await.unwrap();
    assert_eq!(instance.state(), WorkerState::Ready);

    Supervisor::on_request_start(&instance).await.unwrap();
    assert_eq!(instance.state(), WorkerState::Active);
}

#[tokio::test]
async fn illegal_transition_terminated_to_active_fails() {
    use edger_worker::transition;
    let err = transition(WorkerState::Terminated, WorkerEvent::Dispatch).unwrap_err();
    assert!(err.to_string().contains("invalid transition"));
}

#[tokio::test]
async fn persistent_worker_returns_to_idle_after_request() {
    let worker_ref = make_worker_ref(PathBuf::from("/workers/persist"), "persist", 30_000, 0);
    let pool = pool();

    pool.fetch(
        &worker_ref.dir,
        &worker_ref.config,
        sample_req("/"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    let instance = pool.get_or_create(&worker_ref).await.unwrap();
    assert_eq!(instance.state(), WorkerState::Idle);
    assert_eq!(instance.request_count(), 1);
}

#[tokio::test]
async fn ephemeral_ttl_zero_enters_ephemeral_term_and_removes_from_pool() {
    let worker_ref = make_worker_ref(PathBuf::from("/workers/ephem"), "ephem", 0, 0);
    let pool = pool();

    pool.fetch(
        &worker_ref.dir,
        &worker_ref.config,
        sample_req("/"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    assert_eq!(pool.len(), 0);
}

#[tokio::test]
async fn idle_ttl_expiry_triggers_termination() {
    let worker_ref = make_worker_ref(PathBuf::from("/workers/ttl"), "ttl", 50, 0);
    let pool = pool();

    pool.fetch(
        &worker_ref.dir,
        &worker_ref.config,
        sample_req("/"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();
    assert_eq!(pool.len(), 1);

    let instance = pool.get_or_create(&worker_ref).await.unwrap();
    assert_eq!(instance.state(), WorkerState::Idle);
    Supervisor::on_ttl_expired(&instance, &pool).await.unwrap();

    assert_eq!(pool.len(), 0);
}

#[tokio::test]
async fn notify_idle_called_when_entering_idle() {
    let worker_ref = make_worker_ref(PathBuf::from("/workers/idle"), "idle", 30_000, 0);
    let pool = pool();

    pool.fetch(
        &worker_ref.dir,
        &worker_ref.config,
        sample_req("/"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    let instance = pool.get_or_create(&worker_ref).await.unwrap();
    assert!(instance.idle_notification_count() >= 1);
}

#[tokio::test]
async fn critical_error_marks_unhealthy_and_terminates() {
    let worker_ref = make_worker_ref(PathBuf::from("/workers/crit"), "crit", 30_000, 0);
    let pool = pool();
    let instance = pool.get_or_create(&worker_ref).await.unwrap();
    Supervisor::spawn(&instance).await.unwrap();
    Supervisor::on_request_start(&instance).await.unwrap();
    Supervisor::on_critical_error(&instance).await.unwrap();
    assert!(instance.is_unhealthy());
    assert_eq!(instance.state(), WorkerState::Terminated);
}
