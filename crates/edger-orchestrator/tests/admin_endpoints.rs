//! Admin endpoint contract coverage for surviving Epic 17 control-plane routes.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::{CreateApiKeyRequest, ExecutionKind, SerializedRequest, WorkerManifest};
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    api_keys::ApiKeyService, build_pipeline, ControlAuth, ControlAuthConfig, ManifestIndex,
    OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::Value;
use tower::ServiceExt;

const ROOT_KEY: &str = "test-root";

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn state_with_auth(auth: ControlAuth) -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/hello"),
            WorkerManifest {
                name: "hello".into(),
                version: Some("1.0.0".into()),
                health_check: Some(edger_core::WorkerHealthCheck {
                    path: "/health".into(),
                    method: Some("GET".into()),
                    mode: edger_core::WorkerHealthCheckMode::Manual,
                    timeout: Some("1s".into()),
                }),
                ..Default::default()
            },
        )
        .unwrap();

    state_with_index(auth, index)
}

fn state_with_index(auth: ControlAuth, index: ManifestIndex) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index,
        auth,
    }
}

#[tokio::test]
async fn manual_health_check_is_explicit_root_only_and_observable() {
    let state = root_state();
    let app = build_pipeline(state.clone());
    let uri = "/api/admin/workers/hello/health-check?version=1.0.0";

    let (status, json, _) = send(app.clone(), "POST", uri, None, Body::empty()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["code"], "UNAUTHORIZED");

    let (status, json, text) = send(app, "POST", uri, Some(ROOT_KEY), Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["healthy"], true);
    assert_eq!(json["trigger"], "manual");
    assert_eq!(json["path"], "/health");

    let events = state.server.operational_events().query(
        edger_orchestrator::observability::OperationalEventQuery {
            worker: Some("hello".into()),
            version: Some("1.0.0".into()),
            kind: Some("health_check".into()),
            ..Default::default()
        },
    );
    assert_eq!(events.events.len(), 1);
    assert_eq!(events.events[0].outcome.as_deref(), Some("healthy"));

    let group = state
        .pool
        .get_metrics()
        .worker_groups
        .into_iter()
        .find(|group| group.name == "hello" && group.version == "1.0.0")
        .expect("health check should create a worker process");
    assert_eq!(group.request_total, 0);
    assert_eq!(group.health.sample_count, 0);
}

fn root_state() -> OrchestratorState {
    state_with_auth(ControlAuth::with_static_key(ROOT_KEY))
}

fn open_state() -> OrchestratorState {
    state_with_auth(ControlAuth::new(ControlAuthConfig::default()))
}

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    api_key: Option<&str>,
    body: Body,
) -> (StatusCode, Value, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(key) = api_key {
        let header = if uri.contains("/invoke") {
            "x-edger-control-authorization"
        } else {
            "authorization"
        };
        request = request.header(header, format!("Bearer {key}"));
    }

    let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

async fn send_with_origin(
    app: Router,
    method: &str,
    uri: &str,
    origin: &str,
) -> (StatusCode, Value, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {ROOT_KEY}"))
        .header("host", "edger.local")
        .header("origin", origin)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

// Mutation captured: treating missing/wrong credentials as root makes the
// 401 cases pass through, while making open mode require a key breaks the open
// 200 cases.
#[tokio::test]
async fn admin_auth_matrix_covers_read_and_mutation_routes() {
    for (method, uri) in [
        ("GET", "/api/admin/workers"),
        ("POST", "/api/admin/workers/hello/disable"),
    ] {
        let app = build_pipeline(root_state());

        let (status, json, _text) = send(app.clone(), method, uri, None, Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["code"], "UNAUTHORIZED");

        let (status, json, _text) =
            send(app.clone(), method, uri, Some("wrong"), Body::empty()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["code"], "UNAUTHORIZED");

        let (status, _json, text) = send(app, method, uri, Some(ROOT_KEY), Body::empty()).await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {text}");

        let open_app = build_pipeline(open_state());
        let (status, _json, text) = send(open_app, method, uri, None, Body::empty()).await;
        assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    }
}

#[tokio::test]
async fn delete_promote_and_invoke_authenticate_before_worker_lookup() {
    for (method, uri) in [
        ("DELETE", "/api/admin/workers/missing"),
        ("POST", "/api/admin/workers/missing/promote?version=1.0.0"),
        ("GET", "/api/admin/workers/missing/invoke?version=1.0.0"),
    ] {
        for api_key in [None, Some("wrong")] {
            let (status, json, _) = send(
                build_pipeline(root_state()),
                method,
                uri,
                api_key,
                Body::empty(),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
            assert_eq!(json["code"], "UNAUTHORIZED");
        }
    }
}

// O Epic 17.A removeu a keys API e este teste era o tripwire da remoção.
// A reintrodução é DELIBERADA (keys persistentes com permissions); o contrato
// agora é o inverso: as rotas existem, e sem store configurado respondem 503
// explícito — nunca o 404 de rota inexistente. O ciclo completo com store
// vive em tests/api_keys_admin.rs.
#[tokio::test]
async fn admin_keys_routes_are_registered_again() {
    let app = build_pipeline(root_state());

    for method in ["GET", "POST"] {
        let (status, json, text) = send(
            app.clone(),
            method,
            "/api/admin/keys",
            Some(ROOT_KEY),
            Body::empty(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "unexpected body: {text}"
        );
        assert_eq!(json["code"], "KEYS_STORE_UNAVAILABLE");
    }
}

// Mutation captured: changing open/static auth to return a non-root principal
// breaks the root role and wildcard namespace contract.
#[tokio::test]
async fn admin_session_returns_root_principal() {
    let app = build_pipeline(root_state());
    let (status, json, text) = send(
        app,
        "GET",
        "/api/admin/session",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["principal"]["name"], "root");
    assert_eq!(json["principal"]["role"], "admin");
    assert_eq!(json["principal"]["isRoot"], true);
    assert_eq!(json["principal"]["namespaces"], serde_json::json!(["*"]));
}

// Mutation captured: dropping worker catalog construction leaves the expected
// worker entry missing.
#[tokio::test]
async fn admin_catalog_returns_worker_entries() {
    let app = build_pipeline(root_state());

    let (status, catalog, text) = send(
        app.clone(),
        "GET",
        "/api/admin/catalog",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    let items = catalog["items"].as_array().expect("catalog items array");
    let worker = items
        .iter()
        .find(|item| item["id"] == "worker:hello")
        .expect("hello worker catalog entry");
    assert_eq!(worker["kind"], "worker");
    assert_eq!(worker["owner"], "hello");
    assert_eq!(worker["route"], "/hello");
    assert_eq!(worker["status"], "loaded");

    let (status, _json, _text) = send(
        app,
        "GET",
        "/api/admin/extensions",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// Mutation captured: toggling only the admin listing state without affecting
// route resolution would keep `/hello` serving after disable and this test
// goes red.
#[tokio::test]
async fn worker_disable_and_enable_controls_data_plane_route() {
    let app = build_pipeline(root_state());

    let (status, _json, text) = send(app.clone(), "GET", "/hello", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert!(text.contains("fetch:GET /"));

    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/hello/disable",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["status"], "disabled");

    let (status, _json, _text) = send(app.clone(), "GET", "/hello", None, Body::empty()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/hello/enable",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["status"], "loaded");

    let (status, _json, text) = send(app, "GET", "/hello", None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert!(text.contains("fetch:GET /"));
}

#[tokio::test]
async fn worker_recycle_targets_one_version_and_requires_an_existing_version() {
    let mut index = ManifestIndex::new();
    for version in ["1.0.0", "2.0.0"] {
        index
            .insert(
                PathBuf::from(format!("/workers/hello-{version}")),
                WorkerManifest {
                    name: "hello".into(),
                    version: Some(version.into()),
                    min_processes: Some(0),
                    max_processes: Some(1),
                    ttl: Some(serde_yaml::Value::String("30s".into())),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let state = state_with_index(ControlAuth::with_static_key(ROOT_KEY), index);
    let workers = state.index.worker_refs();
    for worker in &workers {
        state
            .pool
            .fetch_worker(
                worker,
                SerializedRequest {
                    method: "GET".into(),
                    uri: "/warm".into(),
                    headers: vec![],
                    body: None,
                    request_id: format!("recycle-{}", worker.version),
                    base_href: None,
                },
                Some(ExecutionKind::FetchHandler),
            )
            .await
            .unwrap();
    }
    let before = state.pool.get_metrics();
    assert_eq!(before.worker_groups.len(), 2);
    let version_b_id = before
        .worker_groups
        .iter()
        .find(|group| group.version == "2.0.0")
        .unwrap()
        .processes[0]
        .id;
    let app = build_pipeline(state.clone());

    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/hello/recycle?version=1.0.0",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["name"], "hello");
    assert_eq!(json["version"], "1.0.0");
    assert!(json["recycled"].as_u64().unwrap() >= 1, "{json}");
    assert_eq!(json["prewarm"], "not_configured");
    let after = state.pool.get_metrics();
    let version_a = after
        .worker_groups
        .iter()
        .find(|group| group.version == "1.0.0")
        .expect("version A metrics remain available after recycling");
    assert_eq!(version_a.total_processes, 0);
    assert!(version_a.processes.is_empty());
    let version_b = after
        .worker_groups
        .iter()
        .find(|group| group.version == "2.0.0")
        .expect("recycling version A must preserve version B");
    assert_eq!(version_b.total_processes, 1);
    assert_eq!(version_b.processes[0].id, version_b_id);

    let (status, json, _) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/hello/recycle",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "VALIDATION_ERROR");

    let (status, json, _) = send(
        app,
        "POST",
        "/api/admin/workers/hello/recycle?version=9.9.9",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["code"], "NOT_FOUND");
}

#[tokio::test]
async fn worker_recycle_requires_workers_toggle_permission() {
    let keys = Arc::new(ApiKeyService::in_memory().unwrap());
    let state =
        state_with_auth(ControlAuth::with_static_key(ROOT_KEY).with_key_service(Arc::clone(&keys)));
    let read_only = keys
        .create(
            &edger_core::root_principal(),
            CreateApiKeyRequest {
                name: "recycle-read-only".into(),
                permissions: vec!["workers:read".into()],
                namespaces: vec!["*".into()],
                workers: vec!["*".into()],
                expires_at: None,
                role: None,
            },
        )
        .unwrap();

    let (status, json, _) = send(
        build_pipeline(state),
        "POST",
        "/api/admin/workers/hello/recycle?version=1.0.0",
        Some(&read_only.raw_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "FORBIDDEN");
}

#[tokio::test]
async fn worker_recycle_rewarms_an_enabled_min_process_floor() {
    let mut state = root_state();
    state
        .index
        .insert(
            PathBuf::from("/workers/recycle-prewarm"),
            WorkerManifest {
                name: "recycle-prewarm".into(),
                version: Some("1.0.0".into()),
                min_processes: Some(1),
                max_processes: Some(1),
                ttl: Some(serde_yaml::Value::String("30s".into())),
                ..Default::default()
            },
        )
        .unwrap();
    let worker = state
        .index
        .worker_refs()
        .into_iter()
        .find(|worker| worker.name == "recycle-prewarm")
        .unwrap();
    state
        .pool
        .fetch_worker(
            &worker,
            SerializedRequest {
                method: "GET".into(),
                uri: "/warm".into(),
                headers: vec![],
                body: None,
                request_id: "recycle-prewarm-initial".into(),
                base_href: None,
            },
            Some(ExecutionKind::FetchHandler),
        )
        .await
        .unwrap();
    let app = build_pipeline(state.clone());

    let (status, json, text) = send(
        app,
        "POST",
        "/api/admin/workers/recycle-prewarm/recycle?version=1.0.0",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert!(json["recycled"].as_u64().unwrap() >= 1, "{json}");
    assert_eq!(json["prewarm"], "scheduled");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let group = state
            .pool
            .get_metrics()
            .worker_groups
            .into_iter()
            .find(|group| group.name == "recycle-prewarm" && group.version == "1.0.0");
        if group.is_some_and(|group| group.total_processes == 1) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "minProcesses floor was not restored"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

// Mutation captured: removing the worker error log from either admin response
// leaves the per-worker errors array or summary object empty.
#[tokio::test]
async fn worker_error_endpoints_return_basic_shapes() {
    let state = root_state();
    state
        .server
        .worker_errors()
        .record("hello", "request-1", 502, "WORKER_ERROR", "boom");
    let app = build_pipeline(state);

    let (status, errors, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/hello/errors",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(errors["worker"], "hello");
    let entries = errors["errors"].as_array().expect("errors array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["code"], "WORKER_ERROR");
    assert_eq!(entries[0]["status"], 502);

    let (status, summary, text) = send(
        app,
        "GET",
        "/api/admin/workers/error-summary",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(summary["summary"]["hello"]["count"], 1);
    assert_eq!(
        summary["summary"]["hello"]["latest"]["code"],
        "WORKER_ERROR"
    );
}

// Mutation captured: skipping `validate_admin_mutation_security` allows the
// cross-site Origin mutation through as 200 instead of 403.
#[tokio::test]
async fn admin_mutation_rejects_cross_site_origin() {
    let app = build_pipeline(root_state());
    let (status, json, _text) = send_with_origin(
        app,
        "POST",
        "/api/admin/workers/hello/disable",
        "https://evil.local",
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "CSRF_DENIED");
}
