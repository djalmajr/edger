//! End-to-end WorkerPool integration tests (story 04.04) — written first (TDD red).
//!
//! Buntime mapping: manifest.yaml fixtures → `parse_worker_config` → `WorkerPool::fetch`
//! with `edger-isolation::MockIsolate` injected via `IsolateFactory`.

mod helpers;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    create_worker_ref, BodyStream, ExecutionKind, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, StreamedResponse, WorkerConfig, WorkerIsolation, WorkerManifest, WorkerRef,
    WorkerResponse,
};
use edger_isolation::MockIsolate;
use edger_worker::{IsolateFactory, PoolConfig, WorkerError, WorkerPool, WorkerState};
use helpers::{
    default_pool_config, execution_kind_from_manifest, pool_with_factory, serialized_get,
    temp_worker_dir, MockIsolateFactory,
};

const FIXTURE_PERSISTENT: &str = include_str!("fixtures/persistent.yaml");
const FIXTURE_SERVERLESS: &str = include_str!("fixtures/serverless.yaml");
const FIXTURE_SPA: &str = include_str!("fixtures/spa.yaml");

#[derive(Default)]
struct RecordingFactory {
    created_refs: Arc<Mutex<Vec<WorkerRef>>>,
}

impl IsolateFactory for RecordingFactory {
    fn create_isolate(&self, worker_ref: &WorkerRef) -> Box<dyn edger_core::Isolate> {
        self.created_refs.lock().unwrap().push(worker_ref.clone());
        Box::new(MockIsolate::new())
    }
}

struct NumberedSlowFactory {
    delay_ms: u64,
    next_id: AtomicUsize,
}

impl NumberedSlowFactory {
    fn new(delay_ms: u64) -> Self {
        Self {
            delay_ms,
            next_id: AtomicUsize::new(1),
        }
    }

    fn created_count(&self) -> usize {
        self.next_id.load(Ordering::SeqCst) - 1
    }
}

impl IsolateFactory for NumberedSlowFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn edger_core::Isolate> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        Box::new(NumberedSlowIsolate {
            delay_ms: self.delay_ms,
            id,
        })
    }
}

#[derive(Default)]
struct FailingPrepareFactory {
    created: AtomicUsize,
    prepare_attempts: Arc<AtomicUsize>,
}

impl FailingPrepareFactory {
    fn created_count(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }

    fn prepare_count(&self) -> usize {
        self.prepare_attempts.load(Ordering::SeqCst)
    }
}

impl IsolateFactory for FailingPrepareFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn edger_core::Isolate> {
        self.created.fetch_add(1, Ordering::SeqCst);
        Box::new(FailingPrepareIsolate {
            prepare_attempts: Arc::clone(&self.prepare_attempts),
        })
    }
}

struct FailingPrepareIsolate {
    prepare_attempts: Arc<AtomicUsize>,
}

#[async_trait]
impl Isolate for FailingPrepareIsolate {
    async fn prepare(&mut self, _config: &WorkerConfig) -> Result<(), IsolationError> {
        self.prepare_attempts.fetch_add(1, Ordering::SeqCst);
        Err(IsolationError::new(
            "UDS_WORKER_FAILED",
            "ready handshake failed",
        ))
    }

    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        unreachable!("dispatch must not run when prepare fails")
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
        self.execute_fetch(serialized_get("/"), config).await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }
}

struct NumberedSlowIsolate {
    delay_ms: u64,
    id: usize,
}

#[async_trait]
impl Isolate for NumberedSlowIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(format!("isolate-{}", self.id))),
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
        _path: &str,
        _base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(serialized_get("/"), config).await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(req, config).await
    }
}

struct NumberedStreamingFactory {
    next_id: AtomicUsize,
}

impl NumberedStreamingFactory {
    fn new() -> Self {
        Self {
            next_id: AtomicUsize::new(1),
        }
    }
}

impl IsolateFactory for NumberedStreamingFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn edger_core::Isolate> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        Box::new(NumberedStreamingIsolate { id })
    }
}

struct NumberedStreamingIsolate {
    id: usize,
}

#[async_trait]
impl Isolate for NumberedStreamingIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(format!("isolate-{}", self.id))),
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
        _path: &str,
        _base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(serialized_get("/"), config).await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn execute_fetch_stream(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, edger_core::IsolationError> {
        if req.uri == "/stream" {
            return Ok(WorkerResponse::Streamed(StreamedResponse {
                status: 200,
                headers: vec![],
                body: pending_body_stream(),
            }));
        }
        self.execute_fetch(req, config)
            .await
            .map(WorkerResponse::Buffered)
    }
}

struct FirstInstanceFailsFactory {
    next_id: AtomicUsize,
}

impl FirstInstanceFailsFactory {
    fn new() -> Self {
        Self {
            next_id: AtomicUsize::new(1),
        }
    }
}

impl IsolateFactory for FirstInstanceFailsFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn edger_core::Isolate> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        Box::new(FirstInstanceFailsIsolate { id })
    }
}

struct FirstInstanceFailsIsolate {
    id: usize,
}

#[async_trait]
impl Isolate for FirstInstanceFailsIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        tokio::time::sleep(Duration::from_millis(40)).await;
        if self.id == 1 {
            return Err(edger_core::IsolationError::new(
                "OOM",
                "simulated per-process OOM",
            ));
        }
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(format!("isolate-{}", self.id))),
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
        _path: &str,
        _base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(serialized_get("/"), config).await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, edger_core::IsolationError> {
        self.execute_fetch(req, config).await
    }
}

struct PendingBody;

impl futures_core::Stream for PendingBody {
    type Item = Result<Bytes, edger_core::IsolationError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

fn pending_body_stream() -> BodyStream {
    Box::pin(PendingBody)
}

fn numbered_worker_ref(max_processes: usize, min_processes: usize, max_requests: u32) -> WorkerRef {
    create_worker_ref(
        std::path::PathBuf::from("/workers/story18-numbered"),
        WorkerManifest {
            name: "story18-numbered".into(),
            max_processes: Some(max_processes),
            min_processes: Some(min_processes),
            max_requests: Some(max_requests),
            ttl: Some(serde_yaml::Value::String("30s".into())),
            ..Default::default()
        },
    )
    .unwrap()
}

fn circuit_breaker_worker_ref(failures: u32, cooldown: &str) -> WorkerRef {
    create_worker_ref(
        std::path::PathBuf::from("/workers/story20-crashy"),
        WorkerManifest {
            name: "story20-crashy".into(),
            circuit_breaker_failures: Some(failures),
            cooldown: Some(serde_yaml::Value::String(cooldown.into())),
            max_processes: Some(1),
            ttl: Some(serde_yaml::Value::String("30s".into())),
            ..Default::default()
        },
    )
    .unwrap()
}

fn oneshot_worker_ref() -> WorkerRef {
    create_worker_ref(
        std::path::PathBuf::from("/workers/story20-oneshot"),
        WorkerManifest {
            name: "story20-oneshot".into(),
            isolation: Some(WorkerIsolation::Oneshot),
            max_processes: Some(1),
            ttl: Some(serde_yaml::Value::String("30s".into())),
            ..Default::default()
        },
    )
    .unwrap()
}

fn queued_worker_ref(max_processes: usize, queue_limit: usize, queue_timeout: &str) -> WorkerRef {
    create_worker_ref(
        std::path::PathBuf::from("/workers/story18-queue"),
        WorkerManifest {
            name: "story18-queue".into(),
            max_processes: Some(max_processes),
            queue_limit: Some(queue_limit),
            queue_timeout: Some(serde_yaml::Value::String(queue_timeout.into())),
            ttl: Some(serde_yaml::Value::String("30s".into())),
            ..Default::default()
        },
    )
    .unwrap()
}

fn ttl_stream_worker_ref(ttl: &str) -> WorkerRef {
    create_worker_ref(
        std::path::PathBuf::from("/workers/story18-ttl-stream"),
        WorkerManifest {
            name: "story18-ttl-stream".into(),
            max_processes: Some(1),
            queue_limit: Some(1),
            queue_timeout: Some(serde_yaml::Value::String("1s".into())),
            ttl: Some(serde_yaml::Value::String(ttl.into())),
            ..Default::default()
        },
    )
    .unwrap()
}

async fn fetch_numbered(pool: WorkerPool, worker_ref: WorkerRef, path: &str) -> usize {
    let body = pool
        .fetch_worker(
            &worker_ref,
            serialized_get(path),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .unwrap()
        .body
        .unwrap();
    String::from_utf8(body.to_vec())
        .unwrap()
        .strip_prefix("isolate-")
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn integration_persistent_worker_cache_hit() {
    let (dir, config, _) = temp_worker_dir(FIXTURE_PERSISTENT);
    let pool = pool_with_factory(
        Arc::new(MockIsolateFactory::default()),
        default_pool_config(),
    );

    pool.fetch(
        dir.path(),
        &config,
        serialized_get("/a"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    pool.fetch(
        dir.path(),
        &config,
        serialized_get("/b"),
        Some(ExecutionKind::FetchHandler),
    )
    .await
    .unwrap();

    let metrics = pool.get_metrics();
    assert_eq!(metrics.cache_hits, 1, "second fetch should hit cache");
    assert!(metrics.cache_misses >= 1);

    let dir_name = dir
        .path()
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap()
        .to_string();
    let worker_ref = create_worker_ref(
        dir.path().to_path_buf(),
        WorkerManifest {
            name: dir_name,
            ..Default::default()
        },
    )
    .unwrap();
    let instance = pool.get_or_create(&worker_ref).await.unwrap();
    assert_eq!(instance.state(), WorkerState::Idle);
}

#[tokio::test]
async fn integration_ephemeral_serverless_terminates_after_response() {
    let (dir, config, _) = temp_worker_dir(FIXTURE_SERVERLESS);
    let pool = pool_with_factory(
        Arc::new(MockIsolateFactory::default()),
        default_pool_config(),
    );

    pool.fetch(dir.path(), &config, serialized_get("/"), None)
        .await
        .unwrap();
    assert_eq!(pool.len(), 0, "ephemeral worker removed after response");

    let misses_before = pool.get_metrics().cache_misses;
    pool.fetch(dir.path(), &config, serialized_get("/again"), None)
        .await
        .unwrap();
    assert!(
        pool.get_metrics().cache_misses > misses_before,
        "second ephemeral fetch is a cache miss"
    );
}

#[tokio::test]
async fn integration_factory_receives_resolved_worker_ref_before_dispatch() {
    let (dir, _config, manifest) = temp_worker_dir(
        r#"name: "@ops/wasm-api"
version: "2.0.0"
ttl: 30
entrypoint: index.wasm
kind: wasm
"#,
    );
    let worker_ref = create_worker_ref(dir.path().to_path_buf(), manifest).unwrap();
    let factory = Arc::new(RecordingFactory::default());
    let created_refs = factory.created_refs.clone();
    let pool = pool_with_factory(factory, default_pool_config());

    let res = pool
        .fetch_worker(&worker_ref, serialized_get("/runtime-boundary"), None)
        .await
        .unwrap();

    assert_eq!(res.status, 200);
    assert!(String::from_utf8(res.body.unwrap().to_vec())
        .unwrap()
        .starts_with("wasm:GET /runtime-boundary"));
    let created = created_refs.lock().unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].name, "@ops/wasm-api");
    assert_eq!(created[0].namespace.as_deref(), Some("@ops"));
    assert_eq!(created[0].version, "2.0.0");
    assert_eq!(
        created[0].kind,
        ExecutionKind::WasmModule {
            entry: Some("index.wasm".into())
        }
    );
}

#[tokio::test]
async fn integration_spa_static_injects_base_href() {
    let (dir, mut config, manifest) = temp_worker_dir(FIXTURE_SPA);
    std::fs::write(
        dir.path().join("index.html"),
        "<html><head></head><body>spa</body></html>",
    )
    .unwrap();
    config.env = HashMap::from([
        ("PUBLIC_API_URL".into(), "https://api.example.test".into()),
        ("OPENAI_API_KEY".into(), "sk-secret".into()),
    ]);
    config.public_env = vec!["PUBLIC_API_URL".into(), "OPENAI_API_KEY".into()];
    let kind = execution_kind_from_manifest(&manifest);
    let created_refs = Arc::new(Mutex::new(Vec::new()));
    let pool = pool_with_factory(
        Arc::new(RecordingFactory {
            created_refs: Arc::clone(&created_refs),
        }),
        default_pool_config(),
    );

    let res = pool
        .fetch(dir.path(), &config, serialized_get("/index.html"), kind)
        .await
        .unwrap();

    let body = String::from_utf8(res.body.unwrap().to_vec()).unwrap();
    assert!(
        body.contains(r#"<base href="/@app/" />"#),
        "SPA base href injected"
    );
    assert!(body.contains("<script>window.__env__="));
    assert!(body.contains(r#""PUBLIC_API_URL":"https://api.example.test""#));
    assert!(!body.contains("OPENAI_API_KEY"));
    assert!(created_refs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn integration_max_requests_retires_then_respawns() {
    let mut yaml = FIXTURE_PERSISTENT.to_string();
    yaml = yaml.replace("maxRequests: 0", "maxRequests: 1");
    let (dir, mut config, _) = temp_worker_dir(&yaml);
    config.max_requests = 1;

    let pool = pool_with_factory(
        Arc::new(MockIsolateFactory::default()),
        default_pool_config(),
    );

    pool.fetch(dir.path(), &config, serialized_get("/1"), None)
        .await
        .unwrap();
    assert_eq!(pool.get_metrics().terminated_total, 1);

    let misses = pool.get_metrics().cache_misses;
    pool.fetch(dir.path(), &config, serialized_get("/2"), None)
        .await
        .unwrap();
    assert!(pool.get_metrics().cache_misses > misses);
}

// Mutation captured: counting regular handler failures instead of prepare
// failures would keep creating fresh isolates on the third request.
#[tokio::test]
async fn story20_circuit_breaker_opens_after_configured_spawn_failures() {
    let factory = Arc::new(FailingPrepareFactory::default());
    let pool = WorkerPool::with_factory(default_pool_config(), factory.clone());
    let worker_ref = circuit_breaker_worker_ref(2, "5s");

    let first = pool
        .fetch_worker(
            &worker_ref,
            serialized_get("/first"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;
    assert!(matches!(first, Err(WorkerError::Isolation(_))));

    let second = pool
        .fetch_worker(
            &worker_ref,
            serialized_get("/second"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;
    assert!(matches!(second, Err(WorkerError::Isolation(_))));
    assert_eq!(factory.created_count(), 2);
    assert_eq!(factory.prepare_count(), 2);

    let third = pool
        .fetch_worker(
            &worker_ref,
            serialized_get("/fast-fail"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;
    assert!(matches!(third, Err(WorkerError::CircuitOpen { .. })));
    assert_eq!(
        factory.created_count(),
        2,
        "open breaker must fail fast without respawning"
    );
    assert_eq!(factory.prepare_count(), 2);
}

// Mutation captured: forgetting to map isolation=oneshot to maxRequests=1
// leaves the first process warm and makes the second request reuse isolate-1.
#[tokio::test]
async fn story20_oneshot_isolation_recycles_after_exactly_one_request() {
    let factory = Arc::new(NumberedSlowFactory::new(0));
    let pool = WorkerPool::with_factory(default_pool_config(), factory.clone());
    let worker_ref = oneshot_worker_ref();

    let first = fetch_numbered(pool.clone(), worker_ref.clone(), "/first").await;
    assert_eq!(first, 1);
    assert_eq!(pool.len(), 0);
    assert_eq!(pool.get_metrics().terminated_total, 1);

    let second = fetch_numbered(pool.clone(), worker_ref, "/second").await;
    assert_eq!(second, 2);
    assert_eq!(factory.created_count(), 2);
}

#[tokio::test]
async fn story18_default_manifest_keeps_one_process_per_worker() {
    let factory = Arc::new(NumberedSlowFactory::new(60));
    let pool = WorkerPool::with_factory(default_pool_config(), factory.clone());
    let worker_ref = create_worker_ref(
        std::path::PathBuf::from("/workers/story18-default"),
        WorkerManifest {
            name: "story18-default".into(),
            ttl: Some(serde_yaml::Value::String("30s".into())),
            ..Default::default()
        },
    )
    .unwrap();

    let (first, second) = tokio::join!(
        fetch_numbered(pool.clone(), worker_ref.clone(), "/one"),
        fetch_numbered(pool.clone(), worker_ref.clone(), "/two")
    );

    assert_eq!(first, second);
    assert_eq!(
        factory.created_count(),
        1,
        "default manifest must preserve the single-process worker behavior"
    );
}

#[tokio::test]
async fn story18_max_processes_fans_out_concurrent_fetches() {
    let factory = Arc::new(NumberedSlowFactory::new(80));
    let pool = WorkerPool::with_factory(default_pool_config(), factory.clone());
    let worker_ref = numbered_worker_ref(3, 0, 0);

    let (first, second, third) = tokio::join!(
        fetch_numbered(pool.clone(), worker_ref.clone(), "/one"),
        fetch_numbered(pool.clone(), worker_ref.clone(), "/two"),
        fetch_numbered(pool.clone(), worker_ref.clone(), "/three")
    );
    let ids = HashSet::from([first, second, third]);

    assert_eq!(
        ids.len(),
        3,
        "three concurrent fetches with maxProcesses=3 must use three instances"
    );
    assert_eq!(factory.created_count(), 3);
    assert_eq!(pool.len(), 3);
}

#[tokio::test]
async fn story18_max_requests_recycles_only_one_instance() {
    let factory = Arc::new(NumberedSlowFactory::new(40));
    let pool = WorkerPool::with_factory(default_pool_config(), factory);
    let warm_ref = numbered_worker_ref(2, 0, 0);

    let (first, second) = tokio::join!(
        fetch_numbered(pool.clone(), warm_ref.clone(), "/warm-one"),
        fetch_numbered(pool.clone(), warm_ref.clone(), "/warm-two")
    );
    assert_eq!(HashSet::from([first, second]).len(), 2);
    assert_eq!(pool.worker_stats().len(), 2);

    let retiring_ref = numbered_worker_ref(2, 0, 1);
    let retired_id = fetch_numbered(pool.clone(), retiring_ref, "/retire-one").await;
    let remaining_request_counts = pool
        .worker_stats()
        .into_iter()
        .map(|stats| stats.request_count)
        .collect::<Vec<_>>();

    assert_eq!(
        pool.len(),
        1,
        "maxRequests must remove only the instance that reached its limit"
    );
    assert_eq!(remaining_request_counts, vec![1]);

    let survivor_ref = numbered_worker_ref(2, 0, 0);
    let survivor_id = fetch_numbered(pool.clone(), survivor_ref, "/survivor").await;
    assert_ne!(
        retired_id, survivor_id,
        "the sibling process should continue serving after one instance is recycled"
    );
}

// Mutation captured: selecting an idle sibling before replenishing a group that
// fell below minProcesses makes the second maxRequests=1 dispatch retire the
// last process, dropping the group to zero and failing this regression.
#[tokio::test]
async fn story18_max_requests_one_recycles_instances_without_killing_group() {
    let factory = Arc::new(NumberedSlowFactory::new(0));
    let pool = WorkerPool::with_factory(default_pool_config(), factory);
    let worker_ref = numbered_worker_ref(2, 2, 1);

    let first = fetch_numbered(pool.clone(), worker_ref.clone(), "/first").await;
    assert_eq!(first, 1);
    assert_eq!(
        pool.len(),
        1,
        "only the process that reached maxRequests should be recycled"
    );

    let second = fetch_numbered(pool.clone(), worker_ref.clone(), "/second").await;
    assert_eq!(
        second, 3,
        "demand should replenish minProcesses with a fresh process before reusing the survivor"
    );
    assert_eq!(
        pool.len(),
        1,
        "the sibling process must survive repeated per-instance recycling"
    );

    let third = fetch_numbered(pool.clone(), worker_ref, "/third").await;
    assert_eq!(third, 4);
    assert_eq!(pool.len(), 1);
}

// Mutation captured: recycling the whole WorkerGroup on a single isolate/OOM
// error removes the healthy sibling, so the follow-up dispatch cannot observe
// an already-surviving process.
#[tokio::test]
async fn story18_instance_error_does_not_kill_sibling_process() {
    let pool = WorkerPool::with_factory(
        default_pool_config(),
        Arc::new(FirstInstanceFailsFactory::new()),
    );
    let worker_ref = numbered_worker_ref(2, 2, 0);

    let (first, second) = tokio::join!(
        pool.fetch_worker(
            &worker_ref,
            serialized_get("/oom"),
            Some(ExecutionKind::FetchHandler),
        ),
        pool.fetch_worker(
            &worker_ref,
            serialized_get("/sibling"),
            Some(ExecutionKind::FetchHandler),
        )
    );

    assert!(matches!(first, Err(WorkerError::Isolation(_))));
    let second = second.expect("healthy sibling should complete");
    assert_eq!(
        String::from_utf8(second.body.unwrap().to_vec()).unwrap(),
        "isolate-2"
    );
    assert_eq!(
        pool.worker_stats().len(),
        1,
        "failed process should be removed without evicting the sibling"
    );

    let follow_up = fetch_numbered(pool.clone(), worker_ref, "/follow-up").await;
    assert_eq!(
        follow_up, 3,
        "next demand replenishes capacity instead of recreating the whole group"
    );
    assert_eq!(pool.worker_stats().len(), 2);
}

// Mutation captured: treating TTL as a group-level timer or failing to keep a
// streamed response Active lets the short TTL terminate the streaming process
// before the body is dropped.
#[tokio::test]
async fn story18_active_stream_is_not_terminated_by_ttl() {
    let pool = WorkerPool::with_factory(
        default_pool_config(),
        Arc::new(NumberedStreamingFactory::new()),
    );
    let worker_ref = ttl_stream_worker_ref("25ms");

    let streamed = pool
        .fetch_worker_stream(
            &worker_ref,
            serialized_get("/stream"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .unwrap();
    assert!(matches!(streamed, WorkerResponse::Streamed(_)));

    tokio::time::sleep(Duration::from_millis(80)).await;
    let stats = pool.worker_stats();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].state, WorkerState::Active);

    drop(streamed);
}

// Mutation captured: a shutdown that only flips a pool flag leaves an already
// queued waiter sleeping until queue_timeout instead of waking it with the
// typed shutdown error.
#[tokio::test]
async fn story18_shutdown_rejects_new_requests_drains_active_and_wakes_queue() {
    let factory = Arc::new(NumberedSlowFactory::new(160));
    let pool = WorkerPool::with_factory(default_pool_config(), factory);
    let worker_ref = queued_worker_ref(1, 1, "1s");

    let active = tokio::spawn(fetch_numbered(pool.clone(), worker_ref.clone(), "/active"));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let queued_pool = pool.clone();
    let queued_ref = worker_ref.clone();
    let queued = tokio::spawn(async move {
        queued_pool
            .fetch_worker(
                &queued_ref,
                serialized_get("/queued"),
                Some(ExecutionKind::FetchHandler),
            )
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    pool.shutdown();

    let new_request = pool
        .fetch_worker(
            &worker_ref,
            serialized_get("/after-shutdown"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;
    assert!(matches!(new_request, Err(WorkerError::Shutdown)));

    let queued_result = tokio::time::timeout(Duration::from_millis(200), queued)
        .await
        .expect("queued waiter must be woken by shutdown")
        .unwrap();
    assert!(matches!(queued_result, Err(WorkerError::Shutdown)));

    let active_id = active.await.unwrap();
    assert_eq!(active_id, 1, "active request should finish during drain");
}

#[tokio::test]
async fn story18_shutdown_drains_all_instances_in_group() {
    let factory = Arc::new(NumberedSlowFactory::new(40));
    let pool = WorkerPool::with_factory(default_pool_config(), factory);
    let worker_ref = numbered_worker_ref(3, 0, 0);

    let _ = tokio::join!(
        fetch_numbered(pool.clone(), worker_ref.clone(), "/one"),
        fetch_numbered(pool.clone(), worker_ref.clone(), "/two"),
        fetch_numbered(pool.clone(), worker_ref.clone(), "/three")
    );
    assert_eq!(pool.len(), 3);

    pool.shutdown();

    assert_eq!(pool.len(), 0);
    let err = pool
        .fetch_worker(
            &worker_ref,
            serialized_get("/after-shutdown"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("shut down"));
}

#[tokio::test]
async fn story18_queue_limit_zero_rejects_when_process_cap_is_busy() {
    let factory = Arc::new(NumberedSlowFactory::new(200));
    let pool = WorkerPool::with_factory(default_pool_config(), factory);
    let worker_ref = queued_worker_ref(1, 0, "1s");

    let first = tokio::spawn(fetch_numbered(
        pool.clone(),
        worker_ref.clone(),
        "/holding-capacity",
    ));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let second = pool
        .fetch_worker(
            &worker_ref,
            serialized_get("/rejected"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;

    assert!(matches!(second, Err(WorkerError::WorkerQueueFull)));
    assert_eq!(pool.get_metrics().worker_queue_rejected, 1);
    let _ = first.await.unwrap();
}

#[tokio::test]
async fn story18_queue_limit_one_allows_one_waiter_then_rejects_excess() {
    let factory = Arc::new(NumberedSlowFactory::new(250));
    let pool = WorkerPool::with_factory(default_pool_config(), factory);
    let worker_ref = queued_worker_ref(1, 1, "1s");

    let first = tokio::spawn(fetch_numbered(pool.clone(), worker_ref.clone(), "/first"));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let queued = tokio::spawn(fetch_numbered(pool.clone(), worker_ref.clone(), "/queued"));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let excess = pool
        .fetch_worker(
            &worker_ref,
            serialized_get("/excess"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;

    assert!(matches!(excess, Err(WorkerError::WorkerQueueFull)));
    assert_eq!(pool.get_metrics().worker_queue_enqueued, 1);
    let _ = first.await.unwrap();
    let queued_id = queued.await.unwrap();
    assert_eq!(queued_id, 1);
}

#[tokio::test]
async fn story18_queue_timeout_returns_typed_error() {
    let factory = Arc::new(NumberedSlowFactory::new(250));
    let pool = WorkerPool::with_factory(default_pool_config(), factory);
    let worker_ref = queued_worker_ref(1, 1, "25ms");

    let first = tokio::spawn(fetch_numbered(pool.clone(), worker_ref.clone(), "/first"));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let timed_out = pool
        .fetch_worker(
            &worker_ref,
            serialized_get("/timeout"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;

    assert!(matches!(timed_out, Err(WorkerError::WorkerQueueTimeout)));
    assert_eq!(pool.get_metrics().worker_queue_timeout, 1);
    let _ = first.await.unwrap();
}

#[tokio::test]
async fn story18_ephemeral_ignores_persistent_queue_limit() {
    let factory = Arc::new(NumberedSlowFactory::new(120));
    let pool = WorkerPool::with_factory(
        PoolConfig {
            max_size: 8,
            ephemeral_concurrency: 2,
            ephemeral_queue_limit: 0,
        },
        factory,
    );
    let worker_ref = create_worker_ref(
        std::path::PathBuf::from("/workers/story18-ephemeral-queue"),
        WorkerManifest {
            name: "story18-ephemeral-queue".into(),
            max_processes: Some(1),
            queue_limit: Some(0),
            ttl: Some(serde_yaml::Value::Number(0.into())),
            ..Default::default()
        },
    )
    .unwrap();

    let first = tokio::spawn(fetch_numbered(pool.clone(), worker_ref.clone(), "/first"));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let second = pool
        .fetch_worker(
            &worker_ref,
            serialized_get("/second"),
            Some(ExecutionKind::FetchHandler),
        )
        .await;

    assert!(
        second.is_ok(),
        "ephemeral dispatch must skip persistent queue"
    );
    assert_eq!(pool.get_metrics().worker_queue_rejected, 0);
    assert_eq!(pool.get_metrics().worker_queue_enqueued, 0);
    let _ = first.await.unwrap();
}

#[tokio::test]
async fn story18_cancelled_queue_waiter_does_not_leak_queue_capacity() {
    let factory = Arc::new(NumberedSlowFactory::new(250));
    let pool = WorkerPool::with_factory(default_pool_config(), factory);
    let worker_ref = queued_worker_ref(1, 1, "1s");

    let first = tokio::spawn(fetch_numbered(pool.clone(), worker_ref.clone(), "/first"));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    let cancelled = tokio::time::timeout(
        Duration::from_millis(40),
        pool.fetch_worker(
            &worker_ref,
            serialized_get("/cancelled-waiter"),
            Some(ExecutionKind::FetchHandler),
        ),
    )
    .await;
    assert!(cancelled.is_err(), "queued waiter future must be cancelled");

    let next = tokio::time::timeout(
        Duration::from_secs(1),
        pool.fetch_worker(
            &worker_ref,
            serialized_get("/next-waiter"),
            Some(ExecutionKind::FetchHandler),
        ),
    )
    .await
    .expect("next waiter must enter the queue instead of seeing a leaked slot")
    .expect("next waiter must eventually dispatch");

    assert_eq!(next.status, 200);
    assert_eq!(pool.get_metrics().worker_queue_queued, 0);
    let _ = first.await.unwrap();
}

#[tokio::test]
async fn story18_long_stream_on_one_instance_does_not_block_free_sibling_process() {
    let pool = WorkerPool::with_factory(
        default_pool_config(),
        Arc::new(NumberedStreamingFactory::new()),
    );
    let worker_ref = queued_worker_ref(2, 1, "1s");

    let streamed = pool
        .fetch_worker_stream(
            &worker_ref,
            serialized_get("/stream"),
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .unwrap();
    assert!(matches!(streamed, WorkerResponse::Streamed(_)));

    let second = tokio::time::timeout(
        Duration::from_millis(100),
        pool.fetch_worker(
            &worker_ref,
            serialized_get("/fast"),
            Some(ExecutionKind::FetchHandler),
        ),
    )
    .await
    .expect("free sibling process should serve while the stream is still open")
    .unwrap();

    assert_eq!(second.status, 200);
    assert_eq!(
        String::from_utf8(second.body.unwrap().to_vec()).unwrap(),
        "isolate-2"
    );
    drop(streamed);
}

#[tokio::test]
async fn integration_concurrent_ephemeral_respects_concurrency() {
    let (dir, config, _) = temp_worker_dir(FIXTURE_SERVERLESS);
    let pool = pool_with_factory(
        Arc::new(MockIsolateFactory {
            slow_fetch_ms: 120,
            ..Default::default()
        }),
        PoolConfig {
            max_size: 8,
            ephemeral_concurrency: 1,
            ephemeral_queue_limit: 0,
        },
    );

    let pool_a = pool.clone();
    let pool_b = pool.clone();
    let path = dir.path().to_path_buf();
    let cfg = config.clone();

    let first = tokio::spawn(async move {
        pool_a
            .fetch(&path, &cfg, serialized_get("/slow"), None)
            .await
    });

    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(40)).await;

    let second = pool_b
        .fetch(dir.path(), &config, serialized_get("/blocked"), None)
        .await;

    assert!(matches!(second, Err(WorkerError::EphemeralQueueFull)));
    let _ = first.await.unwrap().unwrap();
}

#[tokio::test]
async fn integration_collision_on_namespace_mismatch() {
    let (dir, _config, manifest) = temp_worker_dir(FIXTURE_PERSISTENT);
    let pool = pool_with_factory(
        Arc::new(MockIsolateFactory::default()),
        default_pool_config(),
    );

    let base = create_worker_ref(dir.path().to_path_buf(), manifest).unwrap();
    pool.get_or_create(&base).await.unwrap();

    let mut mismatched = base.clone();
    mismatched.namespace = Some("@evil".into());
    let result = pool.get_or_create(&mismatched).await;
    assert!(matches!(result, Err(WorkerError::Collision { .. })));
}

#[tokio::test]
async fn integration_shutdown_rejects_fetch() {
    let (dir, config, _) = temp_worker_dir(FIXTURE_PERSISTENT);
    let pool = pool_with_factory(
        Arc::new(MockIsolateFactory::default()),
        default_pool_config(),
    );

    pool.shutdown();
    let err = pool
        .fetch(dir.path(), &config, serialized_get("/"), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("shut down"));
}
