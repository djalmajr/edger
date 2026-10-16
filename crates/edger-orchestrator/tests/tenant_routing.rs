//! Tenant availability must be decided before any worker invocation.

use std::fs;
use std::io::Write;
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

fn message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["message"]
        .as_str()
        .unwrap()
        .to_string()
}

/// EDG-31: request completo (status + headers + corpo), para comparar
/// `content-type` e `content-length` com os de um nome inexistente do mesmo
/// tamanho.
async fn request_full(
    app: Router,
    path: &str,
    host: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::builder().uri(path);
    if let Some(host) = host {
        req = req.header("host", host);
    }
    let response = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn header_opt(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// EDG-31: a resposta da origem escondida deve ser indistinguível da de um
/// nome inexistente do MESMO TAMANHO — mesmo status, `content-type`,
/// `content-length` e corpo idêntico salvo o nome.
fn assert_unknown_shape(
    hidden: &(StatusCode, HeaderMap, String),
    unknown: &(StatusCode, HeaderMap, String),
    hidden_name: &str,
    unknown_name: &str,
    label: &str,
) {
    assert_eq!(hidden.0, unknown.0, "{label}: status");
    assert_eq!(
        header_opt(&hidden.1, "content-type"),
        header_opt(&unknown.1, "content-type"),
        "{label}: content-type"
    );
    assert_eq!(
        header_opt(&hidden.1, "content-length"),
        header_opt(&unknown.1, "content-length"),
        "{label}: content-length"
    );
    assert_eq!(
        hidden.2,
        unknown.2.replace(unknown_name, hidden_name),
        "{label}: the hidden origin must be indistinguishable from an unknown worker"
    );
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

// EDG-31: pin de versão inexistente num app escondido (hosts sem
// nameRoute), com o tenant routing LIGADO: a resposta é exatamente a de um
// worker inexistente do mesmo tamanho de nome e o identify NAO é chamado —
// a falha acontece na resolução, antes do gate de tenant.
#[tokio::test]
async fn hidden_app_pin_failure_answers_unknown_worker_with_tenant_routing_on() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder) = fixture(root.path(), true, Some(client));

    // `app` (3 chars) vs `zzz` (3 chars): content-length idêntica.
    for (path, unknown) in [
        ("/app@9.9.9", "/zzz@9.9.9"),
        ("/app@9.9.9/x", "/zzz@9.9.9/x"),
    ] {
        let hidden = request_full(app.clone(), path, Some("gateway.example.com")).await;
        let unknown_resp = request_full(app.clone(), unknown, Some("gateway.example.com")).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(code(&hidden.2), "NOT_FOUND", "{path}");
        assert_unknown_shape(&hidden, &unknown_resp, "app", "zzz", path);
    }

    // O identify nunca foi chamado (a falha é anterior ao gate) e o worker
    // nunca foi invocado.
    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-31: a seleção ponderada que falha ANTES do guard (a versão da policy
// vira staged no disco depois do carregamento) num app escondido, com o
// tenant routing LIGADO: o 503 da rota indisponível responde como worker
// inexistente do mesmo tamanho de nome e o identify não é chamado.
#[tokio::test]
async fn hidden_app_weighted_failure_answers_unknown_worker_with_tenant_routing_on() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder) = fixture_weighted(root.path(), Some(client));

    // A versão da policy vira staged no disco depois do carregamento: a
    // seleção falha com ROUTING_UNAVAILABLE antes do guard EDG-27.
    fs::write(
        root.path().join("app").join(".edger-revision"),
        "test-rev\nstaged=true\n",
    )
    .unwrap();

    let hidden = request_full(app.clone(), "/app/probe", Some("gateway.example.com")).await;
    let unknown = request_full(app.clone(), "/zzz/probe", Some("gateway.example.com")).await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert_eq!(code(&hidden.2), "NOT_FOUND");
    assert_unknown_shape(
        &hidden,
        &unknown,
        "app",
        "zzz",
        "seleção ponderada indisponível",
    );

    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-31 (F1/F2): helpers de zip + fixtures de app instalada pelo control
// plane, para os cenários de versão aberta desabilitada/interna/staged e de
// alias (base de plugin / homepage) com tenant routing ligado.

fn zip_package(files: &[(&str, &str)]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

/// EDG-31: pacote zip do worker `app` com o manifest yaml completo.
fn app_zip(manifest: &str) -> Vec<u8> {
    zip_package(&[
        ("manifest.yaml", manifest),
        ("index.ts", "export default () => new Response('ok');"),
    ])
}

async fn install_zip(app: &Router, manifest: &str, staged: bool) -> (StatusCode, String) {
    let uri = if staged {
        "/api/admin/workers/install?staged=true"
    } else {
        "/api/admin/workers/install"
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/zip")
                .header("authorization", "Bearer root-key")
                .body(Body::from(app_zip(manifest)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn disable_version(app: &Router, version: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/admin/workers/app/disable?version={version}"))
                .header("authorization", "Bearer root-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// EDG-31 (F1/F2): fixture com tenant routing E routing ponderados ligados;
/// a app `app` entra por install via zip (versões múltiplas no mesmo bucket).
fn fixture_zip_root(
    root: &std::path::Path,
    client: Option<TenantIdentityClient>,
) -> (Router, Recorder, edger_orchestrator::ManifestIndex) {
    let index = load_manifests_from_roots(&[], None, &[root.to_path_buf()]).unwrap();
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
            index: index.clone(),
            auth: ControlAuth::with_static_key("root-key"),
        }),
        recorder,
        index,
    )
}

// EDG-31 (F1): versão aberta DESABILITADA ao lado da versão restrita, com o
// tenant routing LIGADO: pin inexistente, pin malformado e seleção
// ponderada indisponível respondem como worker inexistente do mesmo tamanho
// e o identify NUNCA é chamado.
#[tokio::test]
async fn hidden_app_with_disabled_open_version_answers_unknown_worker_with_tenant_routing_on() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder, index) = fixture_zip_root(root.path(), Some(client));

    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nhosts: [acme.example.com]\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"2.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = disable_version(&app, "2.0.0").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let policy = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    // Pin inexistente e pin malformado (era 400 PARSE_ERROR): iguais ao
    // inexistente do mesmo tamanho de nome.
    for (path, unknown) in [("/app@9.9.9", "/zzz@9.9.9"), ("/app@~bad", "/zzz@~bad")] {
        let hidden = request_full(app.clone(), path, Some("gateway.example.com")).await;
        let unknown_resp = request_full(app.clone(), unknown, Some("gateway.example.com")).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(code(&hidden.2), "NOT_FOUND", "{path}");
        assert_unknown_shape(&hidden, &unknown_resp, "app", "zzz", path);
    }

    // Seleção ponderada indisponível (a versão restrita vira staged no disco
    // depois do carregamento): igual ao inexistente.
    fs::write(
        root.path().join("app").join(".edger-revision"),
        "test-rev\nstaged=true\n",
    )
    .unwrap();
    let hidden = request_full(app.clone(), "/app/probe", Some("gateway.example.com")).await;
    let unknown = request_full(app.clone(), "/zzz/probe", Some("gateway.example.com")).await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert_eq!(code(&hidden.2), "NOT_FOUND");
    assert_unknown_shape(
        &hidden,
        &unknown,
        "app",
        "zzz",
        "seleção ponderada indisponível com versão aberta desabilitada",
    );

    // O identify nunca foi chamado e o worker nunca foi invocado.
    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-31 (F1): versão aberta INTERNA ao lado da versão restrita, com o
// tenant routing LIGADO: mesma indistinguibilidade do caso desabilitada —
// a versão interna não é alcançável por rota pública e não conta no
// agregado.
#[tokio::test]
async fn hidden_app_with_internal_open_version_answers_unknown_worker_with_tenant_routing_on() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder, index) = fixture_zip_root(root.path(), Some(client));

    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nhosts: [acme.example.com]\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"2.0.0\"\nvisibility: internal\nentrypoint: index.ts\nkind: fetch\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let policy = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    for (path, unknown) in [("/app@9.9.9", "/zzz@9.9.9"), ("/app@~bad", "/zzz@~bad")] {
        let hidden = request_full(app.clone(), path, Some("gateway.example.com")).await;
        let unknown_resp = request_full(app.clone(), unknown, Some("gateway.example.com")).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(code(&hidden.2), "NOT_FOUND", "{path}");
        assert_unknown_shape(&hidden, &unknown_resp, "app", "zzz", path);
    }

    fs::write(
        root.path().join("app").join(".edger-revision"),
        "test-rev\nstaged=true\n",
    )
    .unwrap();
    let hidden = request_full(app.clone(), "/app/probe", Some("gateway.example.com")).await;
    let unknown = request_full(app.clone(), "/zzz/probe", Some("gateway.example.com")).await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert_eq!(code(&hidden.2), "NOT_FOUND");
    assert_unknown_shape(
        &hidden,
        &unknown,
        "app",
        "zzz",
        "seleção ponderada indisponível com versão aberta interna",
    );

    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-31 (F1), controle: a staged HABILITADA e pública é alcançável por pin
// (e o pin atende, passando pelo gate de tenant normalmente), então a app
// NÃO fica escondida: o erro detalhado do pin inexistente é preservado.
#[tokio::test]
async fn hidden_app_with_staged_open_version_keeps_the_detailed_errors_with_tenant_routing_on() {
    let root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder, index) = fixture_zip_root(root.path(), Some(client));

    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nhosts: [acme.example.com]\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"2.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let policy = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    // O detalhe é preservado: a app não está escondida.
    let (status, body) =
        request_with_body(app.clone(), "/app@9.9.9", Some("gateway.example.com"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");
    assert_eq!(message(&body), "version 9.9.9 not available");

    // O pin alcança a staged aberta e passa pelo gate de tenant (identify
    // chamado, tenant no allowlist): a staged continua contando.
    let (status, _body) =
        request_with_body(app.clone(), "/app@2.0.0", Some("gateway.example.com"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(identity.0.lock().unwrap().2 >= 1);
    assert_eq!(recorder.0.lock().unwrap().len(), 1);
    task.abort();
}

// EDG-31 (F2): a base de plugin de uma app escondida responde EXATAMENTE o
// que responderia uma base não configurada no mesmo caminho, com o tenant
// routing LIGADO — e a mensagem não contém o nome real do app.
#[tokio::test]
async fn hidden_app_plugin_base_answers_like_an_unconfigured_base_with_tenant_routing_on() {
    let root = tempfile::tempdir().unwrap();
    let absent_root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder, _index) = fixture_zip_root(root.path(), Some(client.clone()));
    let (absent, _absent_recorder, _absent_index) =
        fixture_zip_root(absent_root.path(), Some(client));

    // App escondida: nome real `app` (3 chars), alias de plugin `/gw`.
    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nbase: /gw\nhosts: [acme.example.com]\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let hidden = request_full(app.clone(), "/gw/probe", Some("gateway.example.com")).await;
    let absent_resp = request_full(absent.clone(), "/gw/probe", Some("gateway.example.com")).await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert_eq!(hidden.0, absent_resp.0);
    assert_eq!(
        header_opt(&hidden.1, "content-type"),
        header_opt(&absent_resp.1, "content-type")
    );
    assert_eq!(
        header_opt(&hidden.1, "content-length"),
        header_opt(&absent_resp.1, "content-length")
    );
    assert_eq!(hidden.2, absent_resp.2);
    assert!(
        !message(&hidden.2).contains("app"),
        "o alias não revela o nome real do app: {}",
        hidden.2
    );

    // O identify nunca foi chamado (o guard recusa antes do gate) e nenhum
    // worker foi invocado.
    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-31 (F2): a homepage de uma app escondida responde EXATAMENTE o que
// responderia uma homepage não configurada, com o tenant routing LIGADO —
// e a mensagem não contém o nome real do app.
#[tokio::test]
async fn hidden_app_homepage_fallback_answers_like_an_unconfigured_homepage_with_tenant_routing_on()
{
    let root = tempfile::tempdir().unwrap();
    let absent_root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder, _index) = fixture_zip_root(root.path(), Some(client.clone()));
    let (absent, _absent_recorder, _absent_index) =
        fixture_zip_root(absent_root.path(), Some(client));

    // App escondida: nome real `app`, base `/` (homepage).
    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nbase: /\nhosts: [acme.example.com]\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let hidden = request_full(app.clone(), "//", Some("gateway.example.com")).await;
    let absent_resp = request_full(absent.clone(), "//", Some("gateway.example.com")).await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert_eq!(hidden.0, absent_resp.0);
    assert_eq!(
        header_opt(&hidden.1, "content-type"),
        header_opt(&absent_resp.1, "content-type")
    );
    assert_eq!(
        header_opt(&hidden.1, "content-length"),
        header_opt(&absent_resp.1, "content-length")
    );
    assert_eq!(hidden.2, absent_resp.2);
    assert!(
        !message(&hidden.2).contains("app"),
        "a homepage não revela o nome real: {}",
        hidden.2
    );

    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-31 (F2, emenda 2): a base de plugin composta SÓ pelo escopo (`/@acme`)
// responde EXATAMENTE o que responderia uma base não configurada no mesmo
// caminho, com o tenant routing LIGADO — no caminho exato e com barra final
// o parser do router falha e a ausência devolve o MESMO erro do parser
// (`400 PARSE_ERROR`, mesmo status, `content-type`, `content-length` e
// corpo), e no subcaminho responde `worker not found: @acme/<segmento>` —
// em nenhum caso o nome real do app aparece e o identify não é chamado.
#[tokio::test]
async fn hidden_app_scope_only_plugin_base_answers_like_an_unconfigured_base_with_tenant_routing_on(
) {
    let root = tempfile::tempdir().unwrap();
    let absent_root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder, _index) = fixture_zip_root(root.path(), Some(client.clone()));
    let (absent, _absent_recorder, _absent_index) =
        fixture_zip_root(absent_root.path(), Some(client));

    // App escondida: nome real `app`, base só de escopo `/@acme`.
    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nbase: /@acme\nhosts: [acme.example.com]\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    for path in ["/@acme", "/@acme/", "/@acme/probe/x"] {
        let hidden = request_full(app.clone(), path, Some("gateway.example.com")).await;
        let absent_resp = request_full(absent.clone(), path, Some("gateway.example.com")).await;
        assert_eq!(hidden.0, absent_resp.0, "{path}");
        assert_eq!(
            header_opt(&hidden.1, "content-type"),
            header_opt(&absent_resp.1, "content-type"),
            "{path}: content-type"
        );
        assert_eq!(
            header_opt(&hidden.1, "content-length"),
            header_opt(&absent_resp.1, "content-length"),
            "{path}: content-length"
        );
        assert_eq!(hidden.2, absent_resp.2, "{path}");
        assert!(
            !hidden.2.contains("app"),
            "a base só de escopo não revela o nome real do app: {path} {}",
            hidden.2
        );
    }

    // Formas exatas: o caminho exato responde o erro do parser (400) e o
    // subcaminho responde 404 com o nome montado do próprio caminho.
    let (status, _headers, body) =
        request_full(app.clone(), "/@acme", Some("gateway.example.com")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&body), "PARSE_ERROR");
    let (status, _headers, body) =
        request_full(app.clone(), "/@acme/probe/x", Some("gateway.example.com")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");
    assert_eq!(message(&body), "worker not found: @acme/probe");

    // O identify nunca foi chamado (o guard recusa antes do gate) e nenhum
    // worker foi invocado.
    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// EDG-31 (F2, emenda 2): a seleção ponderada que falha ANTES do guard (a
// versão da policy vira staged no disco depois do carregamento) numa base de
// plugin só de escopo (`/@acme`), com o tenant routing LIGADO: a ausência da
// base é a resposta (o 503 `application not available` nunca aparece) e o
// identify não é chamado.
#[tokio::test]
async fn hidden_app_weighted_failure_with_scope_only_base_answers_like_an_unconfigured_base_with_tenant_routing_on(
) {
    let root = tempfile::tempdir().unwrap();
    let absent_root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, recorder, index) = fixture_zip_root(root.path(), Some(client.clone()));
    let (absent, _absent_recorder, _absent_index) =
        fixture_zip_root(absent_root.path(), Some(client));

    // App escondida: nome real `app`, base só de escopo `/@acme`.
    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nbase: /@acme\nhosts: [acme.example.com]\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let policy = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    // A versão da policy vira staged no disco depois do carregamento: a
    // seleção falha com ROUTING_UNAVAILABLE antes do guard EDG-27.
    fs::write(
        root.path().join("app").join(".edger-revision"),
        "test-rev\nstaged=true\n",
    )
    .unwrap();

    for path in ["/@acme", "/@acme/", "/@acme/probe/x"] {
        let hidden = request_full(app.clone(), path, Some("gateway.example.com")).await;
        let absent_resp = request_full(absent.clone(), path, Some("gateway.example.com")).await;
        assert_eq!(hidden.0, absent_resp.0, "{path}");
        assert_eq!(
            header_opt(&hidden.1, "content-type"),
            header_opt(&absent_resp.1, "content-type"),
            "{path}: content-type"
        );
        assert_eq!(
            header_opt(&hidden.1, "content-length"),
            header_opt(&absent_resp.1, "content-length"),
            "{path}: content-length"
        );
        assert_eq!(hidden.2, absent_resp.2, "{path}");
        assert!(
            !hidden.2.contains("app"),
            "a seleção indisponível não revela o nome real do app: {path} {}",
            hidden.2
        );
    }

    assert_eq!(identity.0.lock().unwrap().2, 0);
    assert!(recorder.0.lock().unwrap().is_empty());
    task.abort();
}

// =====================================================================
// EDG-31 (emenda 3): equivalência por construção com tenant routing
// LIGADO
// =====================================================================
// A queda do alias escondido (o caminho original é resolvido de novo pelo
// router excluindo a base e despachado pelo fluxo normal) responde
// EXATAMENTE o Router sem a configuração daquele plugin — status, todos
// os headers (com `x-request-id` fixo) e corpo — nos dois gatilhos de
// recusa (guard e seleção ponderada inelegível), sem dispatch na app
// escondida e sem identify quando a ausência não alcança o gate.

/// Registra o nome de cada worker despachado: a queda do alias nunca pode
// despachar a app escondida.
#[derive(Clone, Default)]
struct DispatchLog(Arc<Mutex<Vec<String>>>);

/// Isolate que devolve um corpo COM O NOME do worker (distingue quais
/// workers atenderam); o registro do dispatch acontece em `create_isolate`.
struct LoggingMarkerIsolate {
    name: String,
}

#[derive(Clone)]
struct LoggingMarkerFactory(DispatchLog);

impl IsolateFactory for LoggingMarkerFactory {
    fn create_isolate(&self, worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        self.0 .0.lock().unwrap().push(worker_ref.name.clone());
        Box::new(LoggingMarkerIsolate {
            name: worker_ref.name.clone(),
        })
    }
}

#[async_trait]
impl Isolate for LoggingMarkerIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from(
                serde_json::to_vec(&json!({ "marker": self.name })).unwrap(),
            )),
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

/// Mesma fixture de `fixture_zip_root` (tenant routing e routing ponderados
// ligados), com a fábrica que registra o nome dos workers despachados.
fn marker_fixture_zip_root(
    root: &std::path::Path,
    client: Option<TenantIdentityClient>,
) -> (Router, DispatchLog, edger_orchestrator::ManifestIndex) {
    let index = load_manifests_from_roots(&[], None, &[root.to_path_buf()]).unwrap();
    let server = ServerState::new_unready();
    server.enable_tenant_routing();
    server.enable_weighted_routing();
    if let Some(client) = client {
        server.set_tenant_identity_client(client);
    }
    let log = DispatchLog::default();
    let pool = WorkerPool::with_factory(
        PoolConfig::default(),
        Arc::new(LoggingMarkerFactory(log.clone())),
    );
    server.mark_ready(pool.clone());
    (
        build_pipeline(OrchestratorState {
            server,
            pool,
            index: index.clone(),
            auth: ControlAuth::with_static_key("root-key"),
        }),
        log,
        index,
    )
}

/// `request_full` com `x-request-id` FIXO: os headers comparados incluem o
/// `x-request-id` ecoado pelo middleware.
async fn request_full_fixed(
    app: Router,
    path: &str,
    host: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::builder()
        .uri(path)
        .header("x-request-id", "security-review-fixed");
    if let Some(host) = host {
        req = req.header("host", host);
    }
    let response = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// A app escondida (`app`, base `base`, `hosts: [acme.example.com]`)
/// recusa a requisição na origem pública sem dono (trigger `guard` ou
/// `weighted`); a queda responde EXATAMENTE o Router sem a configuração
/// daquele plugin em cada `path` (status, todos os headers com `x-request-id`
/// fixo e corpo), sem dispatch na app escondida e sem identify (a ausência
/// não alcança o gate em nenhum desses cenários). `extra_manifests` são
/// instalados nos dois Routers (homepage pública, worker público `gw`…)..
async fn assert_alias_falls_through_to_absence_tenant_on(
    label: &str,
    base: &str,
    paths: &[&str],
    weighted: bool,
    extra_manifests: &[&str],
) {
    let root = tempfile::tempdir().unwrap();
    let absent_root = tempfile::tempdir().unwrap();
    let (client, identity, task) = start_identify().await;
    let (app, log, index) = marker_fixture_zip_root(root.path(), Some(client.clone()));
    let (absent, absent_log, _absent_index) =
        marker_fixture_zip_root(absent_root.path(), Some(client));

    let (status, body) = install_zip(
        &app,
        &format!(
            "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nbase: {base}\nhosts: [acme.example.com]\n"
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    for manifest in extra_manifests {
        let (status, body) = install_zip(&app, manifest, false).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (status, body) = install_zip(&absent, manifest, false).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    if weighted {
        // Gatilho de seleção ponderada inelegível: policy com a versão e a
        // versão vira staged no disco depois do carregamento — a seleção
        // falha ANTES do guard numa app escondida.
        let policy = parse_routing_policy(
            br#"{"name":"app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
        )
        .unwrap();
        persist_routing_policy(&index, &policy).unwrap();
        fs::write(
            root.path().join("app").join(".edger-revision"),
            "test-rev\nstaged=true\n",
        )
        .unwrap();
    }

    for path in paths {
        let hidden = request_full_fixed(app.clone(), path, Some("gateway.example.com")).await;
        let absent_resp =
            request_full_fixed(absent.clone(), path, Some("gateway.example.com")).await;
        assert_eq!(hidden.0, absent_resp.0, "{label} {base} {path}: status");
        assert_eq!(
            &hidden.1, &absent_resp.1,
            "{label} {base} {path}: todos os headers (x-request-id fixo)"
        );
        assert_eq!(hidden.2, absent_resp.2, "{label} {base} {path}: corpo");
    }

    // Zero dispatch na app escondida; o identify nunca é chamado (a
    // ausência não alcança o gate, e a queda não o alcança mais).
    assert!(
        log.0.lock().unwrap().iter().all(|name| name != "app"),
        "zero dispatch na app escondida: {label} {:?}",
        log.0.lock().unwrap()
    );
    assert!(
        absent_log
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|name| name != "app"),
        "o Router de ausência também não despacha a app escondida: {label}"
    );
    assert_eq!(
        identity.0.lock().unwrap().2,
        0,
        "{label}: o identify não pode ser chamado quando a ausência não alcança o gate"
    );
    task.abort();
}

// EDG-31 (emenda 3), tenant routing LIGADO, gatilho do guard: as bases
// `//`, `/@acme`, `/gw` e `/@acme/plugin` coincidem com o Router sem a
// base (status, todos os headers com `x-request-id` fixo e corpo).
#[tokio::test]
async fn hidden_plugin_base_fallthrough_matches_absence_guard_trigger_with_tenant_routing_on() {
    assert_alias_falls_through_to_absence_tenant_on("slash", "//", &["//", "///"], false, &[])
        .await;
    assert_alias_falls_through_to_absence_tenant_on(
        "scope",
        "/@acme",
        &["/@acme", "/@acme/", "/@acme/probe/x"],
        false,
        &[],
    )
    .await;
    assert_alias_falls_through_to_absence_tenant_on(
        "gw",
        "/gw",
        &["/gw", "/gw/", "/gw/x"],
        false,
        &[],
    )
    .await;
    assert_alias_falls_through_to_absence_tenant_on(
        "nsp",
        "/@acme/plugin",
        &["/@acme/plugin", "/@acme/plugin/probe"],
        false,
        &[],
    )
    .await;
}

// EDG-31 (emenda 3), tenant routing LIGADO, gatilho da seleção ponderada
// inelegível: as mesmas bases e caminhos coincidem com o Router sem a base.
#[tokio::test]
async fn hidden_plugin_base_fallthrough_matches_absence_weighted_trigger_with_tenant_routing_on() {
    assert_alias_falls_through_to_absence_tenant_on("slash-w", "//", &["//", "///"], true, &[])
        .await;
    assert_alias_falls_through_to_absence_tenant_on(
        "scope-w",
        "/@acme",
        &["/@acme", "/@acme/", "/@acme/probe/x"],
        true,
        &[],
    )
    .await;
    assert_alias_falls_through_to_absence_tenant_on(
        "gw-w",
        "/gw",
        &["/gw", "/gw/", "/gw/x"],
        true,
        &[],
    )
    .await;
    assert_alias_falls_through_to_absence_tenant_on(
        "nsp-w",
        "/@acme/plugin",
        &["/@acme/plugin", "/@acme/plugin/probe"],
        true,
        &[],
    )
    .await;
}

// EDG-31 (emenda 3), tenant routing LIGADO: COM UMA HOMEPAGE PÚBLICA
// CONFIGURADA, a queda serve a homepage como a ausência serve (a base só
// de barras cai no sinal `HOMEPAGE` tratado pelo próprio router), e para
// `/gw` a homepage só serve se a ausência servir (a ausência responde
// `worker not found: gw`; a queda responde o mesmo).
#[tokio::test]
async fn hidden_plugin_base_fallthrough_to_configured_homepage_with_tenant_routing_on() {
    let homepage =
        "name: homepage-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nbase: /\n";
    assert_alias_falls_through_to_absence_tenant_on(
        "home-slash",
        "//",
        &["//", "///", "/gw"],
        false,
        &[homepage],
    )
    .await;
    assert_alias_falls_through_to_absence_tenant_on(
        "home-slash-w",
        "//",
        &["//", "///", "/gw"],
        true,
        &[homepage],
    )
    .await;
    assert_alias_falls_through_to_absence_tenant_on("home-gw", "/gw", &["/gw"], false, &[homepage])
        .await;
    assert_alias_falls_through_to_absence_tenant_on(
        "home-gw-w",
        "/gw",
        &["/gw"],
        true,
        &[homepage],
    )
    .await;
}

// EDG-31 (emenda 3), tenant routing LIGADO: com um worker PÚBLICO chamado
// `gw` (sem `hosts`), a queda do alias escondido `/gw` despacha para ESSE
// worker — exatamente como a ausência faz — sem dispatch na app escondida
// e sem identify.
#[tokio::test]
async fn hidden_plugin_base_fallthrough_to_public_worker_gw_with_tenant_routing_on() {
    let gw = "name: gw\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n";
    assert_alias_falls_through_to_absence_tenant_on("pubgw", "/gw", &["/gw/x"], false, &[gw]).await;
    assert_alias_falls_through_to_absence_tenant_on("pubgw-w", "/gw", &["/gw/x"], true, &[gw])
        .await;

    // Forma explícita: a resposta é o dispatch no worker público `gw` (o
    // corpo carrega o nome dele).
    let root = tempfile::tempdir().unwrap();
    let (client, _identity, task) = start_identify().await;
    let (app, log, _index) = marker_fixture_zip_root(root.path(), Some(client));
    let (status, body) = install_zip(
        &app,
        "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nbase: /gw\nhosts: [acme.example.com]\n",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = install_zip(&app, gw, false).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, _headers, body) =
        request_full_fixed(app.clone(), "/gw/x", Some("gateway.example.com")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a queda despacha no worker público `gw`: {body}"
    );
    assert_eq!(body, r#"{"marker":"gw"}"#);
    assert_eq!(
        log.0.lock().unwrap().as_slice(),
        vec!["gw".to_string()],
        "só o worker público `gw` foi despachado"
    );
    task.abort();
}
