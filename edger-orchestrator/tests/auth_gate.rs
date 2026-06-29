//! Auth gate integration tests (story 05.04).

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use edger_core::{PublicRoutesConfig, WorkerManifest};
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, ApiKeyStore, AuthGate, AuthGateConfig, ExtensionRegistry, ManifestIndex,
    OrchestratorState, ServerState, SqliteApiKeyStore,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn orchestrator(
    store: Arc<SqliteApiKeyStore>,
    auth_config: AuthGateConfig,
    workers: Vec<(PathBuf, WorkerManifest)>,
) -> OrchestratorState {
    let mut index = ManifestIndex::new();
    for (dir, manifest) in workers {
        index.insert(dir, manifest).unwrap();
    }
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index,
        registry: ExtensionRegistry::new(),
        auth: AuthGate::new(auth_config, store),
    }
}

#[tokio::test]
async fn protected_route_without_key_returns_401() {
    let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
    let state = orchestrator(
        store,
        AuthGateConfig::default(),
        vec![(
            PathBuf::from("/workers/hello"),
            WorkerManifest {
                name: "hello".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )],
    );
    let app = build_pipeline(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/hello")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn root_key_accesses_any_namespace() {
    let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
    let state = orchestrator(
        store,
        AuthGateConfig {
            root_api_key: Some("root-secret".into()),
            ..Default::default()
        },
        vec![(
            PathBuf::from("/workers/acme"),
            WorkerManifest {
                name: "@acme/app".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )],
    );
    let app = build_pipeline(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/@acme/app")
                .header("authorization", "Bearer root-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn namespaced_key_allowed_and_denied() {
    let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
    store
        .insert_key(
            "btk_acme_only",
            "acme-key",
            "editor",
            &[],
            &["@acme".into()],
            None,
        )
        .unwrap();

    let workers = vec![
        (
            PathBuf::from("/workers/acme"),
            WorkerManifest {
                name: "@acme/app".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        ),
        (
            PathBuf::from("/workers/other"),
            WorkerManifest {
                name: "@other/app".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        ),
    ];

    let state = orchestrator(store.clone(), AuthGateConfig::default(), workers);
    let app = build_pipeline(state);

    let ok = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/@acme/app")
                .header("authorization", "Bearer btk_acme_only")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    let denied = app
        .oneshot(
            Request::builder()
                .uri("/@other/app")
                .header("authorization", "Bearer btk_acme_only")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn public_route_bypasses_auth() {
    let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
    let state = orchestrator(
        store,
        AuthGateConfig {
            global_public_routes: PublicRoutesConfig {
                routes: vec!["/login".into()],
                exact: false,
            },
            ..Default::default()
        },
        vec![(
            PathBuf::from("/workers/login"),
            WorkerManifest {
                name: "login".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )],
    );
    let app = build_pipeline(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn invalid_key_returns_401() {
    let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
    let state = orchestrator(
        store,
        AuthGateConfig::default(),
        vec![(
            PathBuf::from("/workers/hello"),
            WorkerManifest {
                name: "hello".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )],
    );
    let app = build_pipeline(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/hello")
                .header("authorization", "Bearer not-a-real-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn sqlite_store_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("auth.db");
    let key = "btk_persist_me";

    {
        let store = SqliteApiKeyStore::open(&db_path).unwrap();
        store
            .insert_key(key, "persist", "editor", &[], &["@acme".into()], None)
            .unwrap();
    }

    let store = SqliteApiKeyStore::open(&db_path).unwrap();
    let principal = store.lookup_by_key(key).unwrap().expect("persisted");
    assert_eq!(principal.namespaces, vec!["@acme"]);
}