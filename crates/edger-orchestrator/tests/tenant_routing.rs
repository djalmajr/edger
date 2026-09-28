//! Tenant availability must be decided before any worker invocation.

use std::fs;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use bytes::Bytes;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerRef,
};
use edger_orchestrator::tenant_identity::TenantIdentityClient;
use edger_orchestrator::{
    build_pipeline, load_manifests_from_roots, parse_routing_policy, persist_routing_policy,
    ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::json;
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<SerializedRequest>>>);

struct RecorderIsolate(Recorder);

impl IsolateFactory for Recorder {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(RecorderIsolate(self.clone()))
    }
}

#[async_trait]
impl Isolate for RecorderIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.0 .0.lock().unwrap().push(req);
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from_static(b"worker-called")),
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
        unreachable!("fetch fixture")
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }
}

#[derive(Clone)]
struct IdentifyState(Arc<Mutex<(StatusCode, String, usize)>>);

async fn identify(
    State(state): State<IdentifyState>,
    headers: HeaderMap,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer service-token")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !query.contains_key("hostname") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut current = state.0.lock().unwrap();
    current.2 += 1;
    if current.0 != StatusCode::OK {
        return current.0.into_response();
    }
    Json(json!({ "tenantSlug": current.1.clone() })).into_response()
}

async fn start_identify() -> (
    TenantIdentityClient,
    IdentifyState,
    tokio::task::JoinHandle<()>,
) {
    let state = IdentifyState(Arc::new(Mutex::new((StatusCode::OK, "acme".into(), 0))));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = reqwest::Url::parse(&format!(
        "http://{}/v1/identify",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let app = Router::new()
        .route("/v1/identify", get(identify))
        .with_state(state.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        TenantIdentityClient::new(url, "service-token").unwrap(),
        state,
        task,
    )
}

fn fixture(
    root: &std::path::Path,
    enabled: bool,
    client: Option<TenantIdentityClient>,
) -> (Router, Recorder) {
    let dir = root.join("app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: app\nversion: '1.0.0'\nentrypoint: index.ts\nkind: fetch\nhosts: [acme.example.com]\n",
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        "export default () => new Response('ok')",
    )
    .unwrap();
    let index = load_manifests_from_roots(&[], None, &[root.to_path_buf()]).unwrap();
    let policy = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();
    let server = ServerState::new_unready();
    if enabled {
        server.enable_tenant_routing();
    }
    if let Some(client) = client {
        server.set_tenant_identity_client(client);
    }
    let recorder = Recorder::default();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(recorder.clone()));
    server.mark_ready(pool.clone());
    (
        build_pipeline(OrchestratorState {
            server,
            pool,
            index,
            auth: ControlAuth::with_static_key("root-key"),
        }),
        recorder,
    )
}

async fn request(
    app: Router,
    path: &str,
    host: Option<&str>,
    tenant_header: Option<&str>,
) -> StatusCode {
    let mut req = Request::builder().uri(path);
    if let Some(host) = host {
        req = req.header("host", host);
    }
    if let Some(tenant_header) = tenant_header {
        req = req.header("x-tenant-id", tenant_header);
    }
    let response = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    status
}

fn header(req: &SerializedRequest, name: &str) -> Option<String> {
    req.headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

#[tokio::test]
async fn flag_off_needs_no_tenancit_and_never_trusts_visitor_tenant_header() {
    let root = tempfile::tempdir().unwrap();
    let (app, recorder) = fixture(root.path(), false, None);
    let status = request(app, "/app", Some("gateway.example.com"), Some("forged")).await;
    assert_eq!(status, StatusCode::OK);
    let requests = recorder.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(header(&requests[0], "x-tenant-id"), None);
}

#[tokio::test]
async fn flag_on_allows_only_identified_tenant_and_rechecks_each_request() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder) = fixture(root.path(), true, Some(client));

    assert_eq!(
        request(
            app.clone(),
            "/app",
            Some("gateway.example.com"),
            Some("forged")
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        request(app.clone(), "/anything", Some("ACME.Example.Com:443"), None).await,
        StatusCode::OK
    );
    {
        let requests = recorder.0.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(header(&requests[0], "x-tenant-id"), Some("acme".into()));
        assert_eq!(header(&requests[1], "x-tenant-id"), Some("acme".into()));
    }

    identity.0.lock().unwrap().1 = "other".into();
    assert_eq!(
        request(app.clone(), "/app@1.0.0", Some("gateway.example.com"), None).await,
        StatusCode::NOT_FOUND
    );
    identity.0.lock().unwrap().0 = StatusCode::SERVICE_UNAVAILABLE;
    assert_eq!(
        request(app.clone(), "/app", Some("gateway.example.com"), None).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        request(app.clone(), "/app", None, Some("acme")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(recorder.0.lock().unwrap().len(), 2);
    assert_eq!(identity.0.lock().unwrap().2, 4);
    task.abort();
}

#[tokio::test]
async fn missing_identity_client_fails_closed_only_when_flag_is_on() {
    let root = tempfile::tempdir().unwrap();
    let (app, recorder) = fixture(root.path(), true, None);
    assert_eq!(
        request(app, "/app", Some("gateway.example.com"), None).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(recorder.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn authenticated_internal_cron_can_run_without_a_visitor_domain() {
    let root = tempfile::tempdir().unwrap();
    let (app, recorder) = fixture(root.path(), true, None);
    let internal = Request::builder()
        .uri("/app@1.0.0")
        .header(edger_core::INTERNAL_REQUEST_HEADER, "true")
        .header("authorization", "Bearer root-key")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(internal).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(recorder.0.lock().unwrap().len(), 1);

    let forged = Request::builder()
        .uri("/app@1.0.0")
        .header(edger_core::INTERNAL_REQUEST_HEADER, "true")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(forged).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn public_dispatch_preserves_visitor_authorization() {
    let root = tempfile::tempdir().unwrap();
    let (client, _identity, task) = start_identify().await;
    let (app, recorder) = fixture(root.path(), true, Some(client));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/app")
                .header("host", "gateway.example.com")
                .header("authorization", "Bearer app-token")
                .header("x-tenant-id", "forged")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _ = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let requests = recorder.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        header(&requests[0], "authorization"),
        Some("Bearer app-token".into())
    );
    assert_eq!(header(&requests[0], "x-tenant-id"), Some("acme".into()));
    task.abort();
}
