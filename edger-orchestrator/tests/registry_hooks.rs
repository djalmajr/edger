//! Extension registry hook chain tests (story 05.05).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use edger_core::{
    Extension, ExtensionContext, Middleware, RequestContext, SerializedRequest, SerializedResponse,
    WorkerManifest,
};
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, run_on_init, run_on_request, run_on_shutdown, AuthGate, AuthGateConfig,
    ExtensionRegistry, ManifestIndex, OrchestratorState, ServerState, SqliteApiKeyStore,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct TestMiddleware {
    name: &'static str,
    priority: i32,
    short_circuit_status: Option<u16>,
    order_log: Arc<Mutex<Vec<&'static str>>>,
    shutdown_hits: Arc<AtomicU32>,
}

impl TestMiddleware {
    fn new(name: &'static str, priority: i32, order_log: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            name,
            priority,
            short_circuit_status: None,
            order_log,
            shutdown_hits: Arc::new(AtomicU32::new(0)),
        }
    }

    fn with_short_circuit(mut self, status: u16) -> Self {
        self.short_circuit_status = Some(status);
        self
    }
}

impl Extension for TestMiddleware {
    fn name(&self) -> &'static str {
        self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        Ok(())
    }

    fn on_shutdown(&self) -> Result<()> {
        self.shutdown_hits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl Middleware for TestMiddleware {
    fn on_request(
        &self,
        _req: &mut SerializedRequest,
        _ctx: &RequestContext,
    ) -> Result<Option<SerializedResponse>> {
        self.order_log.lock().expect("order lock").push(self.name);
        if let Some(status) = self.short_circuit_status {
            return Ok(Some(SerializedResponse {
                status,
                headers: vec![],
                body: Some(Bytes::from_static(b"short-circuit")),
            }));
        }
        Ok(None)
    }
}

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn base_orchestrator(registry: ExtensionRegistry) -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/hello"),
            WorkerManifest {
                name: "hello".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
    OrchestratorState {
        server,
        pool,
        index,
        registry,
        auth: AuthGate::new(
            AuthGateConfig {
                root_api_key: Some("root".into()),
                ..Default::default()
            },
            store,
        ),
    }
}

#[test]
fn empty_registry_returns_none() {
    let registry = ExtensionRegistry::new();
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "r".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("r");
    assert!(run_on_request(&registry, &mut req, &ctx).unwrap().is_none());
}

#[test]
fn priority_order_is_stable() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let early = Arc::new(TestMiddleware::new("early", -10, order.clone()));
    let late = Arc::new(TestMiddleware::new("late", 10, order.clone()));

    let mut registry = ExtensionRegistry::new();
    registry.register(late).unwrap();
    registry.register(early).unwrap();

    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "r".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("r");
    run_on_request(&registry, &mut req, &ctx).unwrap();

    assert_eq!(*order.lock().expect("order lock"), vec!["early", "late"]);
}

#[tokio::test]
async fn short_circuit_skips_pool_fetch() {
    let mut registry = ExtensionRegistry::new();
    registry
        .register(Arc::new(
            TestMiddleware::new("teapot", 0, Arc::new(Mutex::new(Vec::new())))
                .with_short_circuit(418),
        ))
        .unwrap();

    let app = build_pipeline(base_orchestrator(registry));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/hello")
                .header("authorization", "Bearer root")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::from_u16(418).unwrap());
}

#[test]
fn on_shutdown_invoked_once_per_extension() {
    let mw = Arc::new(TestMiddleware::new(
        "shutdown-test",
        0,
        Arc::new(Mutex::new(Vec::new())),
    ));
    let hits = mw.shutdown_hits.clone();
    let mut registry = ExtensionRegistry::new();
    registry.register(mw).unwrap();
    run_on_shutdown(&registry).unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn on_init_runs_all_extensions() {
    let mut registry = ExtensionRegistry::new();
    registry
        .register(Arc::new(TestMiddleware::new(
            "a",
            0,
            Arc::new(Mutex::new(Vec::new())),
        )))
        .unwrap();
    run_on_init(&registry, &mut ExtensionContext::default()).unwrap();
}
