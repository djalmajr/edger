//! End-to-end WorkerPool integration tests (story 04.04) — written first (TDD red).
//!
//! Buntime mapping: manifest.yaml fixtures → `parse_worker_config` → `WorkerPool::fetch`
//! with `edger-isolation::MockIsolate` injected via `IsolateFactory`.

mod helpers;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use edger_core::{create_worker_ref, ExecutionKind, WorkerManifest, WorkerRef};
use edger_isolation::MockIsolate;
use edger_worker::{IsolateFactory, PoolConfig, WorkerError, WorkerState};
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
    let (dir, config, manifest) = temp_worker_dir(FIXTURE_SPA);
    let kind = execution_kind_from_manifest(&manifest);
    let pool = pool_with_factory(
        Arc::new(MockIsolateFactory {
            spa_html: Some("<html><head></head><body>spa</body></html>".into()),
            ..Default::default()
        }),
        default_pool_config(),
    );

    let res = pool
        .fetch(dir.path(), &config, serialized_get("/index.html"), kind)
        .await
        .unwrap();

    let body = String::from_utf8(res.body.unwrap().to_vec()).unwrap();
    assert!(
        body.contains(r#"base href="/@app/""#),
        "SPA base href injected"
    );
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
