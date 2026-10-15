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
    // EDG-27: o app declara `hosts`, então num Host sem dono a rota por nome
    // responde 404 (a existência não vaza); as sondas de data plane usam o
    // host próprio (vhost) para chegar ao worker e passar pelo gate de
    // tenant. A rota por nome segue coberta pelo teste de pin abaixo
    // (worker sem `hosts`) e pelo teste de interna.
    fixture_manifest(
        root,
        enabled,
        client,
        "name: app\nversion: '1.0.0'\nentrypoint: index.ts\nkind: fetch\nhosts: [acme.example.com]\n",
    )
}

fn fixture_manifest(
    root: &std::path::Path,
    enabled: bool,
    client: Option<TenantIdentityClient>,
    manifest: &str,
) -> (Router, Recorder) {
    let dir = root.join("app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.yaml"), manifest).unwrap();
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

async fn request_with_body(
    app: Router,
    path: &str,
    host: Option<&str>,
    tenant_header: Option<&str>,
) -> (StatusCode, String) {
    let mut req = Request::builder().uri(path);
    if let Some(host) = host {
        req = req.header("host", host);
    }
    if let Some(tenant_header) = tenant_header {
        req = req.header("x-tenant-id", tenant_header);
    }
    let response = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn code(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string()
}

/// EDG-29: a hidden app must answer with the same error shape as an unknown
/// worker (same JSON keys); the `message` carries the name, so only the
/// shape and the `code` are compared.
fn same_error_format(body: &str, control: &str, label: &str) {
    let err = serde_json::from_str::<serde_json::Value>(body).unwrap();
    let ctl = serde_json::from_str::<serde_json::Value>(control).unwrap();
    let mut keys: Vec<&str> = err
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut ctl_keys: Vec<&str> = ctl
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    ctl_keys.sort();
    assert_eq!(
        keys, ctl_keys,
        "{label}: error shape must match an unknown worker"
    );
}

/// EDG-29: a weighted-routing fixture with tenant routing on — the single
/// version declares `hosts` (protected) and the policy sends 100% of the
/// traffic to it, so any cohort selects the protected version.
fn fixture_weighted(
    root: &std::path::Path,
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
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();
    let server = ServerState::new_unready();
    server.enable_tenant_routing();
    server.enable_weighted_routing();
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
    let status = request(app, "/app", Some("ACME.Example.Com:443"), Some("forged")).await;
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
            Some("ACME.Example.Com:443"),
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
        request(
            app.clone(),
            "/app@1.0.0",
            Some("ACME.Example.Com:443"),
            None
        )
        .await,
        StatusCode::NOT_FOUND
    );
    identity.0.lock().unwrap().0 = StatusCode::SERVICE_UNAVAILABLE;
    assert_eq!(
        request(app.clone(), "/app", Some("ACME.Example.Com:443"), None).await,
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
        request(app, "/app", Some("ACME.Example.Com:443"), None).await,
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

// EDG-27, decisão 4: a cobertura do gate de tenant no ramo `version_pinned`
// da rota por nome (pin `@version` num Host sem dono) usa um worker SEM
// `hosts`, para que a sonda passe pelo gate de tenant em vez da recusa
// EDG-27 — o teste de vhost acima segue cobrindo o app com hosts.
#[tokio::test]
async fn version_pinned_name_route_passes_the_tenant_gate_without_hosts() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder) = fixture_manifest(
        root.path(),
        true,
        Some(client),
        "name: app\nversion: '1.0.0'\nentrypoint: index.ts\nkind: fetch\n",
    );

    // Tenant identificado no allowlist: o pin pela rota por nome atende e
    // recebe o `x-tenant-id` verificado.
    assert_eq!(
        request(app.clone(), "/app@1.0.0", Some("gateway.example.com"), None).await,
        StatusCode::OK
    );
    {
        let requests = recorder.0.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(header(&requests[0], "x-tenant-id"), Some("acme".into()));
    }

    // Tenant fora do allowlist: o pin também falha fechado (404), sem
    // invocar o worker.
    identity.0.lock().unwrap().1 = "other".into();
    assert_eq!(
        request(app.clone(), "/app@1.0.0", Some("gateway.example.com"), None).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    task.abort();
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
                .header("host", "ACME.Example.Com:443")
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

// EDG-29: um app que a origem nao alcana (recusa EDG-27 da rota) responde
// como um worker inexistente — com tenant negado E com identify
// indisponivel — e o identify NAO e chamado: o guard da rota recusa antes
// do gate de tenant, entao o gate nunca revela a existencia do app
// (404/503 de tenant no lugar do 404 de worker inexistente).
#[tokio::test]
async fn hidden_worker_answers_unknown_worker_even_when_the_tenant_gate_would_fail() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder) = fixture(root.path(), true, Some(client));

    // (a) tenant negado (identificado fora do allowlist): o pin @version e a
    // rota por nome num Host sem dono tem a mesma resposta de um worker
    // inexistente.
    identity.0.lock().unwrap().1 = "other".into();
    for (path, unknown) in [
        ("/app@1.0.0", "/no-such-worker@1.0.0"),
        ("/app", "/no-such-worker"),
    ] {
        let (status, body) =
            request_with_body(app.clone(), path, Some("gateway.example.com"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(code(&body), "NOT_FOUND", "{path}");
        let (unknown_status, unknown_body) =
            request_with_body(app.clone(), unknown, Some("gateway.example.com"), None).await;
        assert_eq!(unknown_status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(status, unknown_status, "{path}");
        assert_eq!(code(&body), code(&unknown_body), "{path}");
        same_error_format(&body, &unknown_body, path);
    }

    // (b) identify indisponivel: o MESMO 404 NOT_FOUND (e nao o 503 do
    // gate) e a MESMA forma.
    identity.0.lock().unwrap().0 = StatusCode::SERVICE_UNAVAILABLE;
    for (path, unknown) in [
        ("/app@1.0.0", "/no-such-worker@1.0.0"),
        ("/app", "/no-such-worker"),
    ] {
        let (status, body) =
            request_with_body(app.clone(), path, Some("gateway.example.com"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(code(&body), "NOT_FOUND", "{path}");
        let (_unknown_status, unknown_body) =
            request_with_body(app.clone(), unknown, Some("gateway.example.com"), None).await;
        same_error_format(&body, &unknown_body, path);
    }

    // (c) o identify nunca foi chamado: o guard recusa antes do gate.
    assert_eq!(identity.0.lock().unwrap().2, 0);
    // E sem dispatch: o worker nunca foi invocado.
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-29, fallback de homepage: um homepage com hosts (sem nameRoute) tem a
// mesma resposta de worker inexistente num Host sem dono, com ou sem
// identify, e sem chamar o identify.
#[tokio::test]
async fn hidden_homepage_fallback_never_reaches_the_tenant_gate() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder) = fixture_manifest(
        root.path(),
        true,
        Some(client),
        "name: app\nversion: '1.0.0'\nentrypoint: index.ts\nkind: fetch\nbase: /\nhosts: [acme.example.com]\n",
    );

    identity.0.lock().unwrap().1 = "other".into();
    let (status, body) =
        request_with_body(app.clone(), "//", Some("gateway.example.com"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");

    identity.0.lock().unwrap().0 = StatusCode::SERVICE_UNAVAILABLE;
    let (status, body) =
        request_with_body(app.clone(), "//", Some("gateway.example.com"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");

    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-29, selecao ponderada: a versao escolhida e protegida (hosts sem
// nameRoute) e a origem e publica sem dono — a resposta e o 404 de worker
// inexistente com o identify indisponivel (e nao o 503 do gate), e o
// identify nao e chamado.
#[tokio::test]
async fn weighted_selection_of_a_hidden_version_never_reaches_the_tenant_gate() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder) = fixture_weighted(root.path(), Some(client));

    // Identify indisponivel: 404 na forma de worker inexistente, nao 503.
    identity.0.lock().unwrap().0 = StatusCode::SERVICE_UNAVAILABLE;
    let (status, body) =
        request_with_body(app.clone(), "/app/probe", Some("gateway.example.com"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");

    // Tenant negado: o mesmo 404.
    identity.0.lock().unwrap().0 = StatusCode::OK;
    identity.0.lock().unwrap().1 = "other".into();
    let (status, body) =
        request_with_body(app.clone(), "/app/probe", Some("gateway.example.com"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");

    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-29, comportamento mantido: no proprio host do worker o gate de tenant
// segue falhando fechado com os proprios erros (404 TENANT_NOT_FOUND / 503
// TENANT_IDENTITY_UNAVAILABLE) — nao o 404 de worker inexistente.
#[tokio::test]
async fn tenant_gate_still_fails_closed_on_the_worker_own_host() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder) = fixture(root.path(), true, Some(client));

    identity.0.lock().unwrap().1 = "other".into();
    let (status, body) =
        request_with_body(app.clone(), "/probe", Some("ACME.Example.Com:443"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "TENANT_NOT_FOUND");

    identity.0.lock().unwrap().0 = StatusCode::SERVICE_UNAVAILABLE;
    let (status, body) =
        request_with_body(app.clone(), "/probe", Some("ACME.Example.Com:443"), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "TENANT_IDENTITY_UNAVAILABLE");

    // O worker nunca e invocado quando o gate recusa.
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-29, comportamento mantido: um worker com `nameRoute: true` segue
// passando pelo gate de tenant na rota por nome (o reorder nao bypassa o
// gate): erro do gate, nao o 404 de worker inexistente, e o identify
// realmente chamado.
#[tokio::test]
async fn name_route_opt_in_still_passes_the_tenant_gate() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder) = fixture_manifest(
        root.path(),
        true,
        Some(client),
        "name: app\nversion: '1.0.0'\nentrypoint: index.ts\nkind: fetch\nhosts: [acme.example.com]\nnameRoute: true\n",
    );

    identity.0.lock().unwrap().1 = "other".into();
    let (status, body) =
        request_with_body(app.clone(), "/app", Some("gateway.example.com"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "TENANT_NOT_FOUND");

    identity.0.lock().unwrap().0 = StatusCode::SERVICE_UNAVAILABLE;
    let (status, body) =
        request_with_body(app.clone(), "/app", Some("gateway.example.com"), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "TENANT_IDENTITY_UNAVAILABLE");

    // O gate de fato rodou (identify chamado) e o worker nao foi invocado.
    assert!(identity.0.lock().unwrap().2 >= 2);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}
