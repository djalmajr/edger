//! EDG-27: um worker com hosts próprios não responde pelas rotas públicas
//! sem dono.
//!
//! Uma versão que declara `hosts` é servida apenas nos seus próprios hosts
//! (salvo com o opt-in `nameRoute: true`). Num Host sem dono, a origem
//! pública — a rota por nome (`/<name>/…`, com ou sem pin `@version`
//! inclusive de staged e rollback), o fallback de homepage e a base de
//! plugin — responde `404 NOT_FOUND` com a mesma forma de worker
//! inexistente, para não vazar a existência do app nem contornar as
//! proteções que o app põe no ingress do próprio domínio. A regra é
//! decidida pela versão que vai de fato receber o dispatch (a seleção
//! ponderada pode escolher outra versão do app lógico) e acontece na
//! preparação do dispatch, antes de qualquer dispatch — sem mexer nas
//! métricas de request do worker. Requisições internas (invoke autenticado
//! do control plane, cron com Bearer root) seguem resolvendo qualquer
//! worker, e o roteamento por host próprio (vhost) não muda.

use std::fs;
use std::io::Write;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerManifest,
    WorkerRef,
};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, parse_routing_policy, persist_routing_policy,
    resolve_route, ControlAuth, ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

const HOSTS: &str = "hosts:\n  - zero.example\n";
const HOSTS_NAME_ROUTE: &str = "hosts:\n  - zero.example\nnameRoute: true\n";

struct MarkerFactory;

impl IsolateFactory for MarkerFactory {
    fn create_isolate(&self, worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        let marker = fs::read_to_string(worker_ref.dir.join("marker.txt"))
            .unwrap_or_else(|_| "missing-marker".into());
        Box::new(MarkerIsolate { marker })
    }
}

struct MarkerIsolate {
    marker: String,
}

#[async_trait::async_trait]
impl Isolate for MarkerIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let payload = serde_json::to_vec(&serde_json::json!({ "marker": self.marker })).unwrap();
        Ok(SerializedResponse {
            status: 200,
            headers: Vec::new(),
            body: Some(bytes::Bytes::from(payload)),
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
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(
            SerializedRequest {
                method: "GET".into(),
                uri: "/".into(),
                headers: Vec::new(),
                body: None,
                request_id: "marker".into(),
                base_href: None,
            },
            config,
        )
        .await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }
}

fn state_with_marker_factory(root: std::path::PathBuf) -> OrchestratorState {
    let server = ServerState::new_unready();
    state_with_marker_factory_inner(root, server)
}

fn state_with_marker_factory_weighted(root: std::path::PathBuf) -> OrchestratorState {
    let server = ServerState::new_unready();
    server.enable_weighted_routing();
    state_with_marker_factory_inner(root, server)
}

fn state_with_marker_factory_inner(
    root: std::path::PathBuf,
    server: ServerState,
) -> OrchestratorState {
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(MarkerFactory));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

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

fn install_zip(version: &str, marker: &str, manifest_extra: &str) -> Vec<u8> {
    worker_zip("zero-app", version, marker, manifest_extra)
}

fn worker_zip(name: &str, version: &str, marker: &str, manifest_extra: &str) -> Vec<u8> {
    worker_zip_with_visibility(name, version, marker, "public", manifest_extra)
}

fn worker_zip_with_visibility(
    name: &str,
    version: &str,
    marker: &str,
    visibility: &str,
    manifest_extra: &str,
) -> Vec<u8> {
    zip_package(&[
        (
            "manifest.yaml",
            &format!(
                "name: {name}\nversion: \"{version}\"\nvisibility: {visibility}\nentrypoint: index.ts\nkind: fetch\n{manifest_extra}"
            ),
        ),
        ("index.ts", "export default () => new Response('ok');"),
        ("marker.txt", marker),
    ])
}

async fn install(
    app: &Router,
    zip: Vec<u8>,
    staged: bool,
) -> (StatusCode, serde_json::Value, String) {
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
                .header("authorization", "Bearer test-root")
                .body(Body::from(zip))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json, text)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (StatusCode, String) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get(app: &Router, uri: &str, host: Option<&str>) -> (StatusCode, String) {
    let headers: Vec<(&str, &str)> = host.map(|host| vec![("host", host)]).unwrap_or_default();
    send(app, "GET", uri, &headers, Vec::new()).await
}

fn marker(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["marker"]
        .as_str()
        .unwrap()
        .to_string()
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

/// EDG-31: GET completo (status + headers + corpo), para comparar
/// `content-type` e `content-length` com os de um nome inexistente do mesmo
/// tamanho.
async fn get_full(app: &Router, uri: &str, host: Option<&str>) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::builder().method("GET").uri(uri);
    if let Some(host) = host {
        request = request.header("host", host);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
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
        header_value(&hidden.1, "content-type"),
        header_value(&unknown.1, "content-type"),
        "{label}: content-type"
    );
    assert_eq!(
        header_value(&hidden.1, "content-length"),
        header_value(&unknown.1, "content-length"),
        "{label}: content-length"
    );
    assert_eq!(
        hidden.2,
        unknown.2.replace(unknown_name, hidden_name),
        "{label}: the hidden origin must be indistinguishable from an unknown worker"
    );
}

// Rota por nome num Host sem dono (e sem header Host): 404 na mesma forma de
// worker inexistente; o host próprio continua servindo o app.
#[tokio::test]
async fn hosted_worker_is_hidden_on_the_name_route_but_serves_its_own_host() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, installed, text) =
        install(&app, install_zip("1.0.0", "zero-one", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(installed["defaultVersion"], "1.0.0");

    // Rota por nome: Host sem dono e sem header Host.
    for host in [Some("other.example"), None] {
        let (status, body) = get(&app, "/zero-app/probe", host).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{host:?}");
        assert_eq!(code(&body), "NOT_FOUND");
    }
    // Pin de versão (a mesma recusa vale para @version, inclusive de
    // rollback).
    let (status, body) = get(&app, "/zero-app@1.0.0/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");

    // Mesma forma de um worker inexistente: a existência do app não vaza.
    let (unknown_status, unknown_body) =
        get(&app, "/no-such-worker/probe", Some("other.example")).await;
    assert_eq!(unknown_status, StatusCode::NOT_FOUND);
    let (hidden_status, hidden_body) = get(&app, "/zero-app/probe", Some("other.example")).await;
    assert_eq!(hidden_status, unknown_status);
    assert_eq!(code(&hidden_body), code(&unknown_body));

    // Host próprio (vhost, regra D6): inalterado.
    let (status, body) = get(&app, "/probe", Some("zero.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "zero-one");
}

// `nameRoute: true` mantém a rota por nome aberta para o worker com hosts.
#[tokio::test]
async fn name_route_opt_in_keeps_the_name_route_open() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) = install(
        &app,
        install_zip("1.0.0", "wide-one", HOSTS_NAME_ROUTE),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    for host in [Some("other.example"), None] {
        let (status, body) = get(&app, "/zero-app/probe", host).await;
        assert_eq!(status, StatusCode::OK, "{host:?}");
        assert_eq!(marker(&body), "wide-one");
    }
    let (status, body) = get(&app, "/probe", Some("zero.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "wide-one");
}

// Worker sem `hosts`: a rota por nome responde como hoje.
#[tokio::test]
async fn worker_without_hosts_keeps_the_name_route() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "plain-one", ""), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    for host in [Some("other.example"), None] {
        let (status, body) = get(&app, "/zero-app/probe", host).await;
        assert_eq!(status, StatusCode::OK, "{host:?}");
        assert_eq!(marker(&body), "plain-one");
    }
}

// Requisições internas continuam alcançando o worker com hosts: header
// x-edger-internal com Bearer root (cron) e o caminho de invoke do control
// plane. O header sem credencial de root não conta como interna.
#[tokio::test]
async fn internal_requests_still_reach_a_hosted_worker() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "zero-one", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Interna autenticada: rota por nome num Host sem dono.
    let (status, body) = send(
        &app,
        "GET",
        "/zero-app/probe",
        &[
            ("host", "other.example"),
            ("x-edger-internal", "true"),
            ("authorization", "Bearer test-root"),
        ],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "zero-one");

    // Header de interna sem credencial de root: não é interna, segue oculta.
    let (status, body) = send(
        &app,
        "GET",
        "/zero-app/probe",
        &[("host", "other.example"), ("x-edger-internal", "true")],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");

    // Invoke do control plane (resolve_worker, sempre interna).
    let (status, body) = send(
        &app,
        "POST",
        "/api/admin/workers/zero-app/invoke/probe",
        &[("x-edger-control-authorization", "Bearer test-root")],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "zero-one");
}

// Versão staged com hosts: o pin @version pela rota por nome também é
// oculto (a ameaça cobre staged e rollback via @version); o host só muda de
// dono no promote (comportamento existente).
#[tokio::test]
async fn staged_version_pin_is_hidden_on_the_name_route() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, installed_v1, text) =
        install(&app, install_zip("1.0.0", "zero-one", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(installed_v1["staged"], false);
    let (status, installed_v2, text) =
        install(&app, install_zip("2.0.0", "zero-two", HOSTS), true).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(installed_v2["staged"], true);
    assert_eq!(installed_v2["defaultVersion"], "1.0.0");

    for path in ["/zero-app@2.0.0/probe", "/zero-app@1.0.0/probe"] {
        let (status, body) = get(&app, path, Some("other.example")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(code(&body), "NOT_FOUND");
    }

    // O staged não assume o domínio: o host continua servindo a v1.
    let (status, body) = get(&app, "/probe", Some("zero.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "zero-one");
}

// A recusa acontece na preparação do dispatch, antes de qualquer dispatch
// (depois da seleção final da versão, inclusive a ponderada): o worker que
// só recebeu 404 nunca aparece nas métricas de request; a primeira resposta
// real (pelo host próprio) registra exatamente `ok = 1`.
#[tokio::test]
async fn name_route_refusal_does_not_touch_worker_request_metrics() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "zero-one", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    for _ in 0..3 {
        let (status, _body) = get(&app, "/zero-app/probe", Some("other.example")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let (status, metrics) = send(
        &app,
        "GET",
        "/metrics",
        &[("authorization", "Bearer test-root")],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !metrics.contains("edger_worker_requests_total{worker=\"zero-app\""),
        "refusões na resolução não podem contar nas métricas do worker\n{metrics}"
    );

    let (status, body) = get(&app, "/probe", Some("zero.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "zero-one");
    let (status, metrics) = send(
        &app,
        "GET",
        "/metrics",
        &[("authorization", "Bearer test-root")],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        metrics.contains(
            "edger_worker_requests_total{worker=\"zero-app\",version=\"1.0.0\",namespace=\"\",outcome=\"ok\"} 1"
        ),
        "a resposta real pelo host próprio registra ok = 1\n{metrics}"
    );
}

// Nível de resolução: a regra EDG-27 NAO vive mais na resolução (decisão 3:
// a decisão é da versão que vai de fato receber o dispatch, porque a seleção
// ponderada pode abrir um default protegido). A resolução segue resolvendo o
// worker com hosts — inclusive pelo pin — e o vhost não muda.
#[test]
fn resolve_route_defers_the_hosts_rule_to_dispatch_preparation() {
    let mut index = ManifestIndex::new();
    let manifest = WorkerManifest {
        name: "hosted".into(),
        version: Some("1.0.0".into()),
        hosts: vec!["app.example.test".into()],
        ..Default::default()
    };
    index
        .insert(std::path::PathBuf::from("/w/hosted"), manifest)
        .unwrap();

    // A resolução devolve o worker; a recusa é aplicada no dispatch à
    // identidade final.
    let route = resolve_route("/hosted/x", None, &index).unwrap();
    match route {
        edger_orchestrator::ResolvedRoute::Worker { worker, .. } => {
            assert_eq!(worker.name, "hosted")
        }
        other => panic!("expected worker route, got {other:?}"),
    }
    let route = resolve_route("/hosted@1.0.0/x", None, &index).unwrap();
    match route {
        edger_orchestrator::ResolvedRoute::Worker { worker, .. } => {
            assert_eq!(worker.name, "hosted")
        }
        other => panic!("expected worker route, got {other:?}"),
    }

    // Vhost inalterado.
    let worker = index.worker_for_host("app.example.test").unwrap();
    assert_eq!(worker.name, "hosted");
}

// F1: uma homepage com hosts nao responde no fallback publico (caminho so
// de barras cai no `HOMEPAGE` quando o Host nao tem dono); a raiz `/`
// continua no redirect 307 do pipeline, o host proprio e as internas
// seguem atendidas, e uma homepage sem hosts segue no fallback.
#[tokio::test]
async fn hosted_homepage_is_hidden_on_the_public_fallback() {
    const HOME_HOSTS: &str = "base: /\nhosts:\n  - home.example\n";
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) = install(
        &app,
        worker_zip("home-app", "1.0.0", "home-one", HOME_HOSTS),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Caminho so de barras cai no fallback de homepage: 404 num Host sem
    // dono (e sem header Host), na mesma forma de worker inexistente.
    for host in [Some("other.example"), None] {
        let (status, body) = get(&app, "//", host).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{host:?}");
        assert_eq!(code(&body), "NOT_FOUND");
    }

    // Raiz comum: redirect do pipeline, inalterado.
    let (status, _body) = get(&app, "/", Some("other.example")).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);

    // Host proprio (vhost, D6): inalterado.
    let (status, body) = get(&app, "/probe", Some("home.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "home-one");

    // Interna: o fallback segue atendendo com Bearer root.
    let (status, body) = send(
        &app,
        "GET",
        "//",
        &[
            ("host", "other.example"),
            ("x-edger-internal", "true"),
            ("authorization", "Bearer test-root"),
        ],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "home-one");

    // Controle: homepage sem hosts segue no fallback publico.
    let plain_root = tempfile::tempdir().unwrap();
    let plain = build_pipeline(state_with_marker_factory(plain_root.path().to_path_buf()));
    let (status, _installed, text) = install(
        &plain,
        worker_zip("plain-home", "1.0.0", "plain-home-one", "base: /\n"),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, body) = get(&plain, "//", Some("other.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "plain-home-one");
}

// F2: a base de plugin de um worker com hosts — base igual ao nome ou base
// diferente — nao responde num Host sem dono; o opt-in `nameRoute: true`,
// as internas e o host proprio seguem atendendo.
#[tokio::test]
async fn hosted_plugin_base_is_hidden_on_the_public_route() {
    const PLUGIN_HOSTS: &str = "base: /zero-app\nhosts:\n  - zero.example\n";
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) = install(
        &app,
        worker_zip("zero-app", "1.0.0", "plugin-one", PLUGIN_HOSTS),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Base coincidente com o nome (a branch de plugin precede a nominal):
    // 404 num Host sem dono (e sem header Host).
    for host in [Some("other.example"), None] {
        let (status, body) = get(&app, "/zero-app/probe", host).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{host:?}");
        assert_eq!(code(&body), "NOT_FOUND");
    }

    // Interna: a base de plugin segue atendendo com Bearer root.
    let (status, body) = send(
        &app,
        "GET",
        "/zero-app/probe",
        &[
            ("host", "other.example"),
            ("x-edger-internal", "true"),
            ("authorization", "Bearer test-root"),
        ],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "plugin-one");

    // Host proprio (vhost, D6): inalterado.
    let (status, body) = get(&app, "/zero-app/probe", Some("zero.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "plugin-one");

    // Controle nameRoute: true: a base de plugin segue aberta.
    let opt_root = tempfile::tempdir().unwrap();
    let opt = build_pipeline(state_with_marker_factory(opt_root.path().to_path_buf()));
    let (status, _installed, text) = install(
        &opt,
        worker_zip(
            "zero-app",
            "1.0.0",
            "plugin-wide",
            "base: /zero-app\nhosts:\n  - zero.example\nnameRoute: true\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, body) = get(&opt, "/zero-app/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "plugin-wide");

    // Base diferente: mesma recusa, mesmo host proprio inalterado.
    let gw_root = tempfile::tempdir().unwrap();
    let gw = build_pipeline(state_with_marker_factory(gw_root.path().to_path_buf()));
    let (status, _installed, text) = install(
        &gw,
        worker_zip(
            "zero-app",
            "1.0.0",
            "gw-one",
            "base: /gw\nhosts:\n  - gw.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    for host in [Some("other.example"), None] {
        let (status, body) = get(&gw, "/gw/probe", host).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{host:?}");
        assert_eq!(code(&body), "NOT_FOUND");
    }
    let (status, body) = get(&gw, "/gw/probe", Some("gw.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "gw-one");
}

// F3: a selecao ponderada pode escolher uma versao com hosts e sem opt-in —
// a regra vale para a versao que vai de fato receber o dispatch, entao a
// protegida nunca atende num Host sem dono (o host proprio segue servindo a
// versao escolhida pela ponderada).
#[tokio::test]
async fn weighted_selection_of_a_hosted_version_is_refused_on_the_public_route() {
    let root = tempfile::tempdir().unwrap();
    let state = state_with_marker_factory_weighted(root.path().to_path_buf());
    let index = state.index.clone();
    let app = build_pipeline(state);

    // v1 declara hosts (protegida); v2 declara hosts com opt-in
    // (nameRoute: true) e e o default aberto.
    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "w-one", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _installed, text) =
        install(&app, install_zip("2.0.0", "w-two", HOSTS_NAME_ROUTE), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // 100% do trafego para a v1 protegida: qualquer cohort a escolhe.
    let policy = parse_routing_policy(
        br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    // Origem publica sem dono: a protegida nunca atende — o default aberto
    // nao autoriza a versao escolhida (100% do peso aponta para ela, com ou
    // sem cohort).
    let probes: [Vec<(&str, &str)>; 4] = [
        vec![("host", "other.example")],
        vec![],
        vec![
            ("host", "other.example"),
            (
                "cookie",
                "edger_cohort=11111111-1111-4111-8111-111111111111",
            ),
        ],
        vec![(
            "cookie",
            "edger_cohort=22222222-2222-4222-8222-222222222222",
        )],
    ];
    for headers in probes {
        let (status, body) = send(&app, "GET", "/zero-app/probe", &headers, Vec::new()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{headers:?}");
        assert_eq!(code(&body), "NOT_FOUND");
    }

    // Host proprio: a vhost segue servindo a versao protegida (D6) — a
    // ponderada restringe as candidatas as versoes que declaram o host.
    let (status, body) = get(&app, "/probe", Some("zero.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "w-one");

    // O caso inverso: a versao escolhida aberta segue atendendo.
    let open_policy = parse_routing_policy(
        br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"2.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &open_policy).unwrap();
    let (status, body) = get(&app, "/zero-app/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "w-two");
}

// Decisao 3: a regra e decidida pela versao escolhida. Default protegida e
// versao ponderada com opt-in: a escolhida decide (200) — o guard antigo de
// resolucao responderia 404 aqui; sem re-selecao, o default protegido
// segue recusado.
#[tokio::test]
async fn weighted_selection_is_decided_by_the_chosen_version() {
    let root = tempfile::tempdir().unwrap();
    let state = state_with_marker_factory_weighted(root.path().to_path_buf());
    let index = state.index.clone();
    let app = build_pipeline(state);

    // v1 declara hosts com opt-in (aberta); v2 declara hosts sem opt-in
    // (protegida) e e o default.
    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "w-one", HOSTS_NAME_ROUTE), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _installed, text) =
        install(&app, install_zip("2.0.0", "w-two", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(_installed["defaultVersion"], "2.0.0");

    // Sem re-selecao: o default protegido responde 404 na origem publica.
    let (status, body) = get(&app, "/zero-app/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");

    // Ponderada 100% para a v1 com opt-in: a versao escolhida decide — 200
    // mesmo com o default protegido.
    let policy = parse_routing_policy(
        br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();
    let (status, body) = get(&app, "/zero-app/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "w-one");
}

// Controle standalone (decisao 3): vhost proprio (D6) e requisicoes internas
// seguem atendendo o worker com hosts nos tres caminhos publicos — nominal,
// fallback de homepage e base de plugin. Sem asserções de recusa: este teste
// e o controle da prova de mutacao da origem (vhost marcada por
// `required_host`).
#[tokio::test]
async fn vhost_and_internal_dispatches_still_reach_hosted_workers() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "ctl-nominal", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _installed, text) = install(
        &app,
        worker_zip(
            "home-app",
            "1.0.0",
            "ctl-home",
            "base: /\nhosts:\n  - home.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _installed, text) = install(
        &app,
        worker_zip(
            "gw-app",
            "1.0.0",
            "ctl-gw",
            "base: /gw\nhosts:\n  - gw.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Vhost proprio (D6): todos os caminhos atendem no host do app.
    let (status, body) = get(&app, "/probe", Some("zero.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "ctl-nominal");
    let (status, body) = get(&app, "/probe", Some("home.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "ctl-home");
    let (status, body) = get(&app, "/probe", Some("gw.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "ctl-gw");

    // Interna (Bearer root): os tres caminhos atendem num Host sem dono.
    for (uri, expected) in [
        ("/zero-app/probe", "ctl-nominal"),
        ("//", "ctl-home"),
        ("/gw/probe", "ctl-gw"),
    ] {
        let (status, body) = send(
            &app,
            "GET",
            uri,
            &[
                ("host", "other.example"),
                ("x-edger-internal", "true"),
                ("authorization", "Bearer test-root"),
            ],
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(marker(&body), expected, "{uri}");
    }
}

// EDG-31: pin de versão inexistente num app escondido (todas as versões
// indexadas declaram `hosts` sem `nameRoute: true`), numa origem pública
// sem dono: a resposta é exatamente a de um worker inexistente do mesmo
// tamanho de nome — status, code, content-type, content-length e corpo —
// em vez do `version 9.9.9 not available` que revelava a existência do app.
#[tokio::test]
async fn pinned_missing_version_of_a_hidden_app_answers_unknown_worker() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "pin-hidden", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // `zero-app` (8 chars) vs `fake-app` (8 chars): content-length idêntica.
    for host in [Some("other.example"), None] {
        let hidden = get_full(&app, "/zero-app@9.9.9/probe", host).await;
        let unknown = get_full(&app, "/fake-app@9.9.9/probe", host).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{host:?}");
        assert_eq!(code(&hidden.2), "NOT_FOUND", "{host:?}");
        assert_unknown_shape(
            &hidden,
            &unknown,
            "zero-app",
            "fake-app",
            &format!("pin inexistente em app escondida {host:?}"),
        );
    }
}

// EDG-31: a seleção ponderada que falha ANTES do guard (a versão da policy
// vira staged no disco depois do carregamento) num app escondido, numa
// origem sem dono: o 503 `application not available` da rota indisponível
// responde como worker inexistente do mesmo tamanho de nome.
#[tokio::test]
async fn weighted_selection_failure_of_a_hidden_app_answers_unknown_worker() {
    let root = tempfile::tempdir().unwrap();
    let state = state_with_marker_factory_weighted(root.path().to_path_buf());
    let index = state.index.clone();
    let app = build_pipeline(state);

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "w-hidden", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    let policy = parse_routing_policy(
        br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    // A versão vira staged no disco depois do carregamento: a seleção falha
    // com ROUTING_UNAVAILABLE antes do guard EDG-27.
    fs::write(
        root.path().join("zero-app").join(".edger-revision"),
        "test-rev\nstaged=true\n",
    )
    .unwrap();

    for host in [Some("other.example"), None] {
        let hidden = get_full(&app, "/zero-app/probe", host).await;
        let unknown = get_full(&app, "/fake-app/probe", host).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{host:?}");
        assert_eq!(code(&hidden.2), "NOT_FOUND", "{host:?}");
        assert_unknown_shape(
            &hidden,
            &unknown,
            "zero-app",
            "fake-app",
            &format!("seleção ponderada indisponível em app escondida {host:?}"),
        );
    }
}

// EDG-31, controle: o erro detalhado sobrevive quando a app NAO está
// escondida — sem `hosts` e com `nameRoute: true` alguma versão seria
// admitida pelo guard, então o pin inexistente mantém o diagnóstico
// `version 9.9.9 not available` (o detalhe serve a quem alcança o app).
#[tokio::test]
async fn pinned_missing_version_keeps_the_detailed_error_when_not_hidden() {
    // App sem `hosts`: nada escondido, o detalhe sobrevive.
    let plain_root = tempfile::tempdir().unwrap();
    let plain = build_pipeline(state_with_marker_factory(plain_root.path().to_path_buf()));
    let (status, _installed, text) =
        install(&plain, install_zip("1.0.0", "plain-pin", ""), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, body) = get(&plain, "/zero-app@9.9.9/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");
    assert_eq!(message(&body), "version 9.9.9 not available");

    // App com `nameRoute: true`: uma versão indexada é admitida pelo guard,
    // o detalhe sobrevive.
    let wide_root = tempfile::tempdir().unwrap();
    let wide = build_pipeline(state_with_marker_factory(wide_root.path().to_path_buf()));
    let (status, _installed, text) = install(
        &wide,
        install_zip("1.0.0", "wide-pin", HOSTS_NAME_ROUTE),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, body) = get(&wide, "/zero-app@9.9.9/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");
    assert_eq!(message(&body), "version 9.9.9 not available");
}

// EDG-31, controle: o próprio host do worker mantém o erro detalhado — a
// falha da seleção ponderada (a versão da policy não declara o host) é o
// 503 ROUTING_UNAVAILABLE, não o 404 de worker inexistente.
#[tokio::test]
async fn weighted_selection_failure_on_the_own_host_keeps_the_detailed_error() {
    let root = tempfile::tempdir().unwrap();
    let state = state_with_marker_factory_weighted(root.path().to_path_buf());
    let index = state.index.clone();
    let app = build_pipeline(state);

    // v1 declara zero.example (serve o host); v2 declara só gw.example.
    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "own-one", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _installed, text) = install(
        &app,
        worker_zip("zero-app", "2.0.0", "own-two", "hosts:\n  - gw.example\n"),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // O install novo promove o default para 2.0.0; devolve a v1 (que
    // declara o host) ao default para o host rotear ao app.
    let (status, body) = send(
        &app,
        "POST",
        "/api/admin/workers/zero-app/promote?version=1.0.0",
        &[("authorization", "Bearer test-root")],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let policy = parse_routing_policy(
        br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"2.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    // Host próprio: a seleção falha (2.0.0 não declara zero.example), mas o
    // contexto é autorizado — o 503 detalhado sobrevive.
    let (status, body) = get(&app, "/probe", Some("zero.example")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "ROUTING_UNAVAILABLE");
}

// EDG-31, controle: os contextos autorizados mantêm o erro detalhado num app
// escondido — a interna de data plane (Bearer root + x-edger-internal)
// mantém o `version 9.9.9 not available` e o invoke do control plane com
// header de versão mantém o erro específico da versão.
#[tokio::test]
async fn internal_requests_keep_the_detailed_error_for_a_missing_pin() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "int-pin", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Interna de data plane: o erro detalhado sobrevive.
    let (status, body) = send(
        &app,
        "GET",
        "/zero-app@9.9.9/probe",
        &[
            ("host", "other.example"),
            ("x-edger-internal", "true"),
            ("authorization", "Bearer test-root"),
        ],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");
    assert_eq!(message(&body), "version 9.9.9 not available");

    // Invoke do control plane com versão inexistente: o erro específico da
    // versão sobrevive (não é o 404 genérico de worker inexistente).
    let (status, body) = send(
        &app,
        "POST",
        "/api/admin/workers/zero-app/invoke/probe",
        &[
            ("x-edger-control-authorization", "Bearer test-root"),
            ("x-edger-worker-version", "9.9.9"),
        ],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body.contains("9.9.9"),
        "o invoke interno mantém o erro específico da versão\n{body}"
    );
}

// EDG-31 (F1): uma versão aberta DESABILITADA ao lado de versões públicas
// restritas não "desesconde" a app — o agregado só conta as versões que
// alguma rota pública por nome ou pin consegue alcançar (habilitadas e
// públicas; desabilitadas e internas nunca contam). Pin inexistente, pin
// malformado (`PARSE_ERROR`) e seleção ponderada indisponível têm a mesma
// resposta de um worker inexistente do mesmo tamanho.
#[tokio::test]
async fn disabled_open_version_of_a_hidden_app_keeps_the_unknown_worker_errors() {
    let root = tempfile::tempdir().unwrap();
    let state = state_with_marker_factory_weighted(root.path().to_path_buf());
    let index = state.index.clone();
    let app = build_pipeline(state);

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "f1-dis-hidden", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _installed, text) =
        install(&app, install_zip("2.0.0", "f1-dis-open", ""), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // A versão aberta é desabilitada pelo endpoint real de administração.
    let (status, body) = send(
        &app,
        "POST",
        "/api/admin/workers/zero-app/disable?version=2.0.0",
        &[("authorization", "Bearer test-root")],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Pin inexistente e pin malformado (era 400 PARSE_ERROR): iguais ao
    // inexistente do mesmo tamanho de nome.
    for (hidden_path, unknown_path) in [
        ("/zero-app@9.9.9/probe", "/fake-app@9.9.9/probe"),
        ("/zero-app@~bad/probe", "/fake-app@~bad/probe"),
    ] {
        let hidden = get_full(&app, hidden_path, Some("other.example")).await;
        let unknown = get_full(&app, unknown_path, Some("other.example")).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{hidden_path}");
        assert_eq!(code(&hidden.2), "NOT_FOUND", "{hidden_path}");
        assert_unknown_shape(&hidden, &unknown, "zero-app", "fake-app", hidden_path);
    }

    // Seleção ponderada indisponível (a versão restrita vira staged no disco
    // depois do carregamento): igual ao inexistente.
    let policy = parse_routing_policy(
        br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();
    fs::write(
        root.path().join("zero-app").join(".edger-revision"),
        "test-rev\nstaged=true\n",
    )
    .unwrap();

    let hidden = get_full(&app, "/zero-app/probe", Some("other.example")).await;
    let unknown = get_full(&app, "/fake-app/probe", Some("other.example")).await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert_eq!(code(&hidden.2), "NOT_FOUND");
    assert_unknown_shape(
        &hidden,
        &unknown,
        "zero-app",
        "fake-app",
        "seleção ponderada indisponível com versão aberta desabilitada",
    );
}

// EDG-31 (F1): uma versão aberta INTERNA ao lado de versões públicas
// restritas também não "desesconde" a app — a visibilidade interna não é
// alcançável por nenhuma rota pública, então a versão não conta no
// agregado. Mesmos oráculos do caso desabilitada.
#[tokio::test]
async fn internal_open_version_of_a_hidden_app_keeps_the_unknown_worker_errors() {
    let root = tempfile::tempdir().unwrap();
    let state = state_with_marker_factory_weighted(root.path().to_path_buf());
    let index = state.index.clone();
    let app = build_pipeline(state);

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "f1-int-hidden", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _installed, text) = install(
        &app,
        worker_zip_with_visibility("zero-app", "2.0.0", "f1-int-internal", "internal", ""),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    for (hidden_path, unknown_path) in [
        ("/zero-app@9.9.9/probe", "/fake-app@9.9.9/probe"),
        ("/zero-app@~bad/probe", "/fake-app@~bad/probe"),
    ] {
        let hidden = get_full(&app, hidden_path, Some("other.example")).await;
        let unknown = get_full(&app, unknown_path, Some("other.example")).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{hidden_path}");
        assert_eq!(code(&hidden.2), "NOT_FOUND", "{hidden_path}");
        assert_unknown_shape(&hidden, &unknown, "zero-app", "fake-app", hidden_path);
    }

    let policy = parse_routing_policy(
        br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();
    fs::write(
        root.path().join("zero-app").join(".edger-revision"),
        "test-rev\nstaged=true\n",
    )
    .unwrap();

    let hidden = get_full(&app, "/zero-app/probe", Some("other.example")).await;
    let unknown = get_full(&app, "/fake-app/probe", Some("other.example")).await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert_eq!(code(&hidden.2), "NOT_FOUND");
    assert_unknown_shape(
        &hidden,
        &unknown,
        "zero-app",
        "fake-app",
        "seleção ponderada indisponível com versão aberta interna",
    );
}

// EDG-31 (F1), controle: a staged HABILITADA e pública É alcançável por pin
// (inclusive `@latest`), então continua contando no agregado — a app NÃO
// fica escondida e o erro detalhado do pin inexistente é preservado.
#[tokio::test]
async fn staged_open_version_keeps_the_detailed_errors_for_the_pin() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) =
        install(&app, install_zip("1.0.0", "f1-stg-hidden", HOSTS), false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, installed, text) =
        install(&app, install_zip("2.0.0", "f1-stg-open", ""), true).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(installed["staged"], true);

    // O detalhe é preservado: a app não está escondida (a staged aberta é
    // alcançável por pin e seria admitida pelo guard).
    let (status, body) = get(&app, "/zero-app@9.9.9/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");
    assert_eq!(message(&body), "version 9.9.9 not available");

    // O pin alcança a staged aberta — inclusive @latest: por isso a regra
    // de "escondido" precisa contá-la.
    let (status, body) = get(&app, "/zero-app@2.0.0/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "f1-stg-open");
    let (status, body) = get(&app, "/zero-app@latest/probe", Some("other.example")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(marker(&body), "f1-stg-open");
}

// EDG-31 (F2): a base de plugin de uma app escondida responde EXATAMENTE o
// que responderia uma base não configurada no mesmo caminho — a mensagem
// não contém o nome real do app (carrega o segmento do alias, como a
// ausência).
#[tokio::test]
async fn hidden_app_plugin_base_answers_exactly_like_an_unconfigured_base() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            "f2-gw",
            "base: /gw\nhosts:\n  - gw.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Ausência: nenhum app com base /gw configurada.
    let absent_root = tempfile::tempdir().unwrap();
    let absent = build_pipeline(state_with_marker_factory(absent_root.path().to_path_buf()));

    for host in [Some("other.example"), None] {
        let hidden = get_full(&app, "/gw/probe", host).await;
        let absent_resp = get_full(&absent, "/gw/probe", host).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{host:?}");
        assert_eq!(hidden.0, absent_resp.0, "{host:?}");
        assert_eq!(
            header_value(&hidden.1, "content-type"),
            header_value(&absent_resp.1, "content-type"),
            "{host:?}: content-type"
        );
        assert_eq!(
            header_value(&hidden.1, "content-length"),
            header_value(&absent_resp.1, "content-length"),
            "{host:?}: content-length"
        );
        assert_eq!(hidden.2, absent_resp.2, "{host:?}");
        assert!(
            !hidden.2.contains("zero-app"),
            "o alias não revela o nome real do app: {}",
            hidden.2
        );
    }
}

// EDG-31 (F2): a homepage de uma app escondida responde EXATAMENTE o que
// responderia uma homepage não configurada — a mensagem não contém o nome
// real do app.
#[tokio::test]
async fn hidden_app_homepage_fallback_answers_exactly_like_an_unconfigured_homepage() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) = install(
        &app,
        worker_zip(
            "home-app",
            "1.0.0",
            "f2-home",
            "base: /\nhosts:\n  - home.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Ausência: nenhum app com base / configurada.
    let absent_root = tempfile::tempdir().unwrap();
    let absent = build_pipeline(state_with_marker_factory(absent_root.path().to_path_buf()));

    for host in [Some("other.example"), None] {
        let hidden = get_full(&app, "//", host).await;
        let absent_resp = get_full(&absent, "//", host).await;
        assert_eq!(hidden.0, StatusCode::NOT_FOUND, "{host:?}");
        assert_eq!(hidden.0, absent_resp.0, "{host:?}");
        assert_eq!(
            header_value(&hidden.1, "content-type"),
            header_value(&absent_resp.1, "content-type"),
            "{host:?}: content-type"
        );
        assert_eq!(
            header_value(&hidden.1, "content-length"),
            header_value(&absent_resp.1, "content-length"),
            "{host:?}: content-length"
        );
        assert_eq!(hidden.2, absent_resp.2, "{host:?}");
        assert!(
            !hidden.2.contains("home-app"),
            "a homepage não revela o nome real do app: {}",
            hidden.2
        );
    }
}

// EDG-31 (F2, emenda 2): a base de plugin composta SÓ pelo escopo (`/@acme`)
// responde EXATAMENTE o que responderia uma base não configurada no mesmo
// caminho: no caminho exato e com barra final o parser do router falha e a
// ausência devolve o MESMO erro do parser (`400 PARSE_ERROR`, mesmo status,
// código, `content-type`, `content-length` e corpo), e no subcaminho
// responde `worker not found: @acme/<segmento>` — em nenhum caso o nome
// real do app aparece.
#[tokio::test]
async fn hidden_app_scope_only_plugin_base_answers_exactly_like_an_unconfigured_base() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            "f2-scope",
            "base: /@acme\nhosts:\n  - acme.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Ausência: nenhum app com base /@acme configurada.
    let absent_root = tempfile::tempdir().unwrap();
    let absent = build_pipeline(state_with_marker_factory(absent_root.path().to_path_buf()));

    for path in ["/@acme", "/@acme/", "/@acme/probe/x"] {
        for host in [Some("other.example"), None] {
            let hidden = get_full(&app, path, host).await;
            let absent_resp = get_full(&absent, path, host).await;
            assert_eq!(hidden.0, absent_resp.0, "{path} {host:?}");
            assert_eq!(
                header_value(&hidden.1, "content-type"),
                header_value(&absent_resp.1, "content-type"),
                "{path} {host:?}: content-type"
            );
            assert_eq!(
                header_value(&hidden.1, "content-length"),
                header_value(&absent_resp.1, "content-length"),
                "{path} {host:?}: content-length"
            );
            assert_eq!(hidden.2, absent_resp.2, "{path} {host:?}");
            assert!(
                !hidden.2.contains("zero-app"),
                "a base só de escopo não revela o nome real do app: {path} {}",
                hidden.2
            );
        }
    }

    // Formas exatas: o caminho exato responde o erro do parser (400) e o
    // subcaminho responde 404 com o nome montado do próprio caminho.
    let (status, body) = get(&app, "/@acme", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(code(&body), "PARSE_ERROR");
    let (status, body) = get(&app, "/@acme/probe/x", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(code(&body), "NOT_FOUND");
    assert_eq!(message(&body), "worker not found: @acme/probe");
}

// EDG-31 (F2, emenda 2): base namespaced `/@acme/plugin` — caminho exato,
// barra final e subcaminho respondem EXATAMENTE como a ausência da base.
#[tokio::test]
async fn hidden_app_namespaced_plugin_base_subpaths_answer_like_an_unconfigured_base() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            "f2-nsp",
            "base: /@acme/plugin\nhosts:\n  - acme.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Ausência: nenhum app com base /@acme/plugin configurada.
    let absent_root = tempfile::tempdir().unwrap();
    let absent = build_pipeline(state_with_marker_factory(absent_root.path().to_path_buf()));

    for path in ["/@acme/plugin", "/@acme/plugin/", "/@acme/plugin/probe/x"] {
        for host in [Some("other.example"), None] {
            let hidden = get_full(&app, path, host).await;
            let absent_resp = get_full(&absent, path, host).await;
            assert_eq!(hidden.0, absent_resp.0, "{path} {host:?}");
            assert_eq!(
                header_value(&hidden.1, "content-type"),
                header_value(&absent_resp.1, "content-type"),
                "{path} {host:?}: content-type"
            );
            assert_eq!(
                header_value(&hidden.1, "content-length"),
                header_value(&absent_resp.1, "content-length"),
                "{path} {host:?}: content-length"
            );
            assert_eq!(hidden.2, absent_resp.2, "{path} {host:?}");
            assert!(
                !hidden.2.contains("zero-app"),
                "o alias não revela o nome real do app: {path} {}",
                hidden.2
            );
        }
    }

    let (status, body) = get(&app, "/@acme/plugin/probe/x", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(message(&body), "worker not found: @acme/plugin");
}

// EDG-31 (F2, emenda 2): base `/gw` — caminho exato, barra final e
// subcaminho respondem EXATAMENTE como a ausência da base.
#[tokio::test]
async fn hidden_app_plugin_base_subpaths_answer_like_an_unconfigured_base() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_marker_factory(root.path().to_path_buf()));

    let (status, _installed, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            "f2-gw-subs",
            "base: /gw\nhosts:\n  - gw.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Ausência: nenhum app com base /gw configurada.
    let absent_root = tempfile::tempdir().unwrap();
    let absent = build_pipeline(state_with_marker_factory(absent_root.path().to_path_buf()));

    for path in ["/gw", "/gw/", "/gw/probe/x"] {
        for host in [Some("other.example"), None] {
            let hidden = get_full(&app, path, host).await;
            let absent_resp = get_full(&absent, path, host).await;
            assert_eq!(hidden.0, absent_resp.0, "{path} {host:?}");
            assert_eq!(
                header_value(&hidden.1, "content-type"),
                header_value(&absent_resp.1, "content-type"),
                "{path} {host:?}: content-type"
            );
            assert_eq!(
                header_value(&hidden.1, "content-length"),
                header_value(&absent_resp.1, "content-length"),
                "{path} {host:?}: content-length"
            );
            assert_eq!(hidden.2, absent_resp.2, "{path} {host:?}");
            assert!(
                !hidden.2.contains("zero-app"),
                "o alias não revela o nome real do app: {path} {}",
                hidden.2
            );
        }
    }

    let (status, body) = get(&app, "/gw/probe/x", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(message(&body), "worker not found: gw");
}

// EDG-31 (F2, emenda 2): a seleção ponderada que falha ANTES do guard (a
// versão da policy vira staged no disco depois do carregamento) numa base de
// plugin só de escopo (`/@acme`) responde EXATAMENTE como a ausência da
// base — o 503 `application not available` da rota indisponível nunca
// aparece.
#[tokio::test]
async fn hidden_app_weighted_failure_with_scope_only_base_answers_like_an_unconfigured_base() {
    let root = tempfile::tempdir().unwrap();
    let state = state_with_marker_factory_weighted(root.path().to_path_buf());
    let index = state.index.clone();
    let app = build_pipeline(state);

    let (status, _installed, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            "w-scope",
            "base: /@acme\nhosts:\n  - zero.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    let policy = parse_routing_policy(
        br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    // A versão vira staged no disco depois do carregamento: a seleção falha
    // com ROUTING_UNAVAILABLE antes do guard EDG-27.
    fs::write(
        root.path().join("zero-app").join(".edger-revision"),
        "test-rev\nstaged=true\n",
    )
    .unwrap();

    let absent_root = tempfile::tempdir().unwrap();
    let absent = build_pipeline(state_with_marker_factory_weighted(
        absent_root.path().to_path_buf(),
    ));

    for path in ["/@acme", "/@acme/", "/@acme/probe/x"] {
        for host in [Some("other.example"), None] {
            let hidden = get_full(&app, path, host).await;
            let absent_resp = get_full(&absent, path, host).await;
            assert_eq!(hidden.0, absent_resp.0, "{path} {host:?}");
            assert_eq!(
                header_value(&hidden.1, "content-type"),
                header_value(&absent_resp.1, "content-type"),
                "{path} {host:?}: content-type"
            );
            assert_eq!(
                header_value(&hidden.1, "content-length"),
                header_value(&absent_resp.1, "content-length"),
                "{path} {host:?}: content-length"
            );
            assert_eq!(hidden.2, absent_resp.2, "{path} {host:?}");
            assert!(
                !hidden.2.contains("zero-app"),
                "a seleção indisponível não revela o nome real do app: {path} {}",
                hidden.2
            );
        }
    }
}

// =====================================================================
// EDG-31 (emenda 3): equivalência por construção
// =====================================================================
// Quando um alias de plugin recusa a requisição na origem, o pipeline
// resolve o MESMO caminho original de novo pelo router EXCLUINDO a base,
// e despacha o resultado pelo fluxo normal (seleção ponderada, guard e
// tenant gate da rota nova). A resposta é exatamente a do Router sem a
// configuração daquele plugin: a próxima base que casar, a rota por nome,
// a homepage ou sua ausência, o sinal `HOMEPAGE` tratado pelo próprio
// router, `PARSE_ERROR` etc. Os testes abaixo comparam status, TODOS os
// headers (com `x-request-id` fixo) e corpo contra o Router sem a base,
// nos dois gatilhos de recusa (guard e seleção ponderada inelegível).

const FIXED_REQUEST_ID: &str = "security-review-fixed";

/// Registra o nome de cada worker despachado: a queda do alias nunca pode
/// despachar a app escondida.
#[derive(Clone, Default)]
struct DispatchLog(Arc<std::sync::Mutex<Vec<String>>>);

/// Fábrica como `MarkerFactory` (o corpo é o `marker.txt` do worker
/// despachado), registrando também o nome de cada worker despachado.
struct LoggingMarkerFactory(DispatchLog);

impl IsolateFactory for LoggingMarkerFactory {
    fn create_isolate(&self, worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        self.0 .0.lock().unwrap().push(worker_ref.name.clone());
        let marker = fs::read_to_string(worker_ref.dir.join("marker.txt"))
            .unwrap_or_else(|_| "missing-marker".into());
        Box::new(MarkerIsolate { marker })
    }
}

fn logging_state_inner(
    root: std::path::PathBuf,
    weighted: bool,
) -> (OrchestratorState, DispatchLog) {
    let log = DispatchLog::default();
    let server = ServerState::new_unready();
    if weighted {
        server.enable_weighted_routing();
    }
    let pool = WorkerPool::with_factory(
        PoolConfig::default(),
        Arc::new(LoggingMarkerFactory(log.clone())),
    );
    server.mark_ready(pool.clone());
    (
        OrchestratorState {
            server,
            pool,
            index: load_manifests_from_dirs(&[root]).unwrap(),
            auth: ControlAuth::with_static_key("test-root"),
        },
        log,
    )
}

fn logging_state(root: std::path::PathBuf) -> (OrchestratorState, DispatchLog) {
    logging_state_inner(root, false)
}

fn logging_state_weighted(root: std::path::PathBuf) -> (OrchestratorState, DispatchLog) {
    logging_state_inner(root, true)
}

/// GET completo com `x-request-id` FIXO: os headers comparados incluem o
/// `x-request-id` ecoado (o middleware de request-id copia o valor da
/// requisição para a resposta).
async fn get_full_fixed(
    app: &Router,
    uri: &str,
    host: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", FIXED_REQUEST_ID);
    if let Some(host) = host {
        request = request.header("host", host);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// Equivalência por construção: a resposta do Router COM o alias escondido
/// deve ser indistinguível da do Router SEM a configuração daquele plugin
/// na mesma URL — status, todos os headers (com `x-request-id` fixo) e
/// corpo.
fn assert_same_full_response(
    label: &str,
    hidden: &(StatusCode, HeaderMap, String),
    absent: &(StatusCode, HeaderMap, String),
) {
    assert_eq!(hidden.0, absent.0, "{label}: status");
    assert_eq!(
        &hidden.1, &absent.1,
        "{label}: todos os headers (x-request-id fixo)"
    );
    assert_eq!(hidden.2, absent.2, "{label}: corpo");
}

/// A app escondida (`zero-app`, base `base`, `hosts`) recusa a requisição
/// na origem pública sem dono (trigger `guard` ou `weighted`); a queda
/// responde EXATAMENTE o Router sem a configuração daquele plugin em cada
/// `path`, e a app escondida nunca recebe dispatch. `extra_zips` são
/// instalados nos dois Routers (homepage pública, worker público `gw`…).
async fn assert_alias_falls_through_to_absence(
    label: &str,
    base: &str,
    paths: &[&str],
    weighted: bool,
    extra_zips: &[Vec<u8>],
) {
    let root = tempfile::tempdir().unwrap();
    let (state, log) = if weighted {
        logging_state_weighted(root.path().to_path_buf())
    } else {
        logging_state(root.path().to_path_buf())
    };
    let index = state.index.clone();
    let app = build_pipeline(state);

    let (status, _installed, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            &format!("em3-{label}"),
            &format!("base: {base}\nhosts:\n  - gw.example\n"),
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    for zip in extra_zips {
        let (status, _installed, text) = install(&app, zip.clone(), false).await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
    }
    if weighted {
        // Gatilho de seleção ponderada inelegível: policy com a versão e a
        // versão vira staged no disco depois do carregamento — a seleção
        // falha ANTES do guard numa app escondida.
        let policy = parse_routing_policy(
            br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
        )
        .unwrap();
        persist_routing_policy(&index, &policy).unwrap();
        fs::write(
            root.path().join("zero-app").join(".edger-revision"),
            "test-rev\nstaged=true\n",
        )
        .unwrap();
    }

    // Ausência: o Router sem a configuração daquele plugin (e sem a app
    // escondida), com os mesmos workers extras.
    let absent_root = tempfile::tempdir().unwrap();
    let (absent_state, _absent_log) = if weighted {
        logging_state_weighted(absent_root.path().to_path_buf())
    } else {
        logging_state(absent_root.path().to_path_buf())
    };
    let absent = build_pipeline(absent_state);
    for zip in extra_zips {
        let (status, _installed, text) = install(&absent, zip.clone(), false).await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
    }

    for path in paths {
        let hidden = get_full_fixed(&app, path, None).await;
        let absent_resp = get_full_fixed(&absent, path, None).await;
        assert_same_full_response(&format!("{label} {base} {path}"), &hidden, &absent_resp);
        assert!(
            !hidden.2.contains("zero-app"),
            "o alias não revela o nome real do app: {label} {path} {}",
            hidden.2
        );
    }

    // Zero dispatch na app escondida (a queda nunca a alcança).
    assert!(
        log.0.lock().unwrap().iter().all(|name| name != "zero-app"),
        "zero dispatch na app escondida: {label} {:?}",
        log.0.lock().unwrap()
    );
}

// EDG-31 (emenda 3), gatilho do guard: bases `//`, `/@acme`, `/gw` e
// `/@acme/plugin` — as respostas da queda coincidem com as do Router sem a
// base (status, todos os headers com `x-request-id` fixo e corpo), incluindo
// o sinal `HOMEPAGE` da base só de barras (a ausência responde
// `404 NOT_FOUND` "no homepage configured", não um `500 HOMEPAGE`).
#[tokio::test]
async fn hidden_plugin_base_refusal_falls_through_to_absence_guard_trigger() {
    assert_alias_falls_through_to_absence("slash", "//", &["//", "///"], false, &[]).await;
    assert_alias_falls_through_to_absence(
        "scope",
        "/@acme",
        &["/@acme", "/@acme/", "/@acme/probe/x"],
        false,
        &[],
    )
    .await;
    assert_alias_falls_through_to_absence("gw", "/gw", &["/gw", "/gw/", "/gw/x"], false, &[]).await;
    assert_alias_falls_through_to_absence(
        "nsp",
        "/@acme/plugin",
        &["/@acme/plugin", "/@acme/plugin/probe"],
        false,
        &[],
    )
    .await;
}

// EDG-31 (emenda 3), gatilho da seleção ponderada inelegível (a versão da
// policy vira staged no disco depois do carregamento): as mesmas bases e
// caminhos coincidem com o Router sem a base.
#[tokio::test]
async fn hidden_plugin_base_refusal_falls_through_to_absence_weighted_trigger() {
    assert_alias_falls_through_to_absence("slash-w", "//", &["//", "///"], true, &[]).await;
    assert_alias_falls_through_to_absence(
        "scope-w",
        "/@acme",
        &["/@acme", "/@acme/", "/@acme/probe/x"],
        true,
        &[],
    )
    .await;
    assert_alias_falls_through_to_absence("gw-w", "/gw", &["/gw", "/gw/", "/gw/x"], true, &[])
        .await;
    assert_alias_falls_through_to_absence(
        "nsp-w",
        "/@acme/plugin",
        &["/@acme/plugin", "/@acme/plugin/probe"],
        true,
        &[],
    )
    .await;
}

// EDG-31 (emenda 3): COM UMA HOMEPAGE PÚBLICA CONFIGURADA, a queda serve a
// homepage como a ausência serve — a base só de barras (`//`) cai no sinal
// `HOMEPAGE` tratado pelo próprio router (a homepage responde, nos dois
// gatilhos); e para `/gw` a homepage só serve se a ausência servir: a
// ausência responde `worker not found: gw` (o nome resolve na rota
// nominal), então a queda também responde isso — não serve a homepage nem
// vaza o sinal.
#[tokio::test]
async fn hidden_plugin_base_falls_through_to_the_configured_homepage() {
    let homepage_zip = worker_zip("homepage-app", "1.0.0", "em3-homepage", "base: /\n");
    let extras = [homepage_zip.clone()];
    // Base `//`: `//` e `///` servem a homepage como a ausência serve;
    // `/gw` (a base não casa com o caminho) responde a rota nominal.
    assert_alias_falls_through_to_absence(
        "home-slash",
        "//",
        &["//", "///", "/gw"],
        false,
        &extras,
    )
    .await;
    assert_alias_falls_through_to_absence(
        "home-slash-w",
        "//",
        &["//", "///", "/gw"],
        true,
        &extras,
    )
    .await;
    // Base `/gw`: a ausência responde 404 no caminho (o nome `gw` resolve
    // como nome), então a queda NUNCA serve a homepage — e coincide.
    assert_alias_falls_through_to_absence("home-gw", "/gw", &["/gw"], false, &extras).await;
    assert_alias_falls_through_to_absence("home-gw-w", "/gw", &["/gw"], true, &extras).await;

    // Forma explícita: `//` com a base escondida serve a homepage (200 com o
    // marker dela) — não o `500 HOMEPAGE` do sinal interno exposto.
    let root = tempfile::tempdir().unwrap();
    let (state, _log) = logging_state(root.path().to_path_buf());
    let app = build_pipeline(state);
    let (status, _i, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            "em3-home-exp",
            "base: //\nhosts:\n  - gw.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _i, text) = install(&app, homepage_zip, false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, body) = get(&app, "//", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a queda serve a homepage: {status} {body}"
    );
    assert_eq!(marker(&body), "em3-homepage");
}

// EDG-31 (emenda 3): com um worker PÚBLICO chamado `gw` (sem `hosts`), a
// queda do alias escondido `/gw` despacha para ESSE worker — exatamente
// como a ausência faz (nos dois gatilhos), com zero dispatch na app
// escondida.
#[tokio::test]
async fn hidden_plugin_base_falls_through_to_the_public_worker_named_gw() {
    let gw_zip = worker_zip("gw", "1.0.0", "em3-gw-worker", "");
    let extras = [gw_zip.clone()];
    assert_alias_falls_through_to_absence("pubgw", "/gw", &["/gw/x"], false, &extras).await;
    assert_alias_falls_through_to_absence("pubgw-w", "/gw", &["/gw/x"], true, &extras).await;

    // Forma explícita: a resposta é o dispatch no worker público `gw` (o
    // marker dele), não na app escondida.
    let root = tempfile::tempdir().unwrap();
    let (state, log) = logging_state(root.path().to_path_buf());
    let app = build_pipeline(state);
    let (status, _i, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            "em3-pubgw-exp",
            "base: /gw\nhosts:\n  - gw.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _i, text) = install(&app, gw_zip, false).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, body) = get(&app, "/gw/x", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a queda despacha no worker público `gw`: {status} {body}"
    );
    assert_eq!(marker(&body), "em3-gw-worker");
    assert_eq!(
        log.0.lock().unwrap().as_slice(),
        vec!["gw".to_string()],
        "só o worker público `gw` foi despachado"
    );
}

// EDG-31 (emenda 3): os contextos autorizados NAO mudam — sem queda: o
// host próprio (vhost), a interna com root e a versão com `nameRoute: true`
// continuam servindo pelo alias.
#[tokio::test]
async fn alias_refusal_fallthrough_keeps_the_authorized_contexts() {
    // Host próprio: o alias no host do app é servido pelo vhost (D6), sem
    // queda.
    let root = tempfile::tempdir().unwrap();
    let (state, log) = logging_state(root.path().to_path_buf());
    let app = build_pipeline(state);
    let (status, _i, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            "em3-own-host",
            "base: /gw\nhosts:\n  - gw.example\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, body) = get(&app, "/gw/probe", Some("gw.example")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "o host próprio continua servindo pelo alias (vhost), sem queda: {status} {body}"
    );
    assert_eq!(marker(&body), "em3-own-host");
    assert_eq!(
        log.0.lock().unwrap().as_slice(),
        vec!["zero-app".to_string()],
        "a app escondida atende no próprio host"
    );

    // Interna com root: o alias continua servindo (o guard admite internas).
    let (status, body) = send(
        &app,
        "GET",
        "/gw/probe",
        &[
            ("host", "other.example"),
            ("x-edger-internal", "true"),
            ("authorization", "Bearer test-root"),
        ],
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a interna com root continua servindo pelo alias: {status} {body}"
    );
    assert_eq!(marker(&body), "em3-own-host");

    // nameRoute: true: a versão opta pela rota pública e o alias continua
    // servindo, sem queda.
    let root = tempfile::tempdir().unwrap();
    let (state, log) = logging_state(root.path().to_path_buf());
    let app = build_pipeline(state);
    let (status, _i, text) = install(
        &app,
        worker_zip(
            "zero-app",
            "1.0.0",
            "em3-name-route",
            "base: /gw\nhosts:\n  - gw.example\nnameRoute: true\n",
        ),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, body) = get(&app, "/gw/probe", Some("other.example")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "nameRoute: true continua servindo pelo alias público: {status} {body}"
    );
    assert_eq!(marker(&body), "em3-name-route");
    assert_eq!(
        log.0.lock().unwrap().as_slice(),
        vec!["zero-app".to_string()]
    );
}

// EDG-31 (emenda 3): uma SEGUNDA base escondida que também casa com o
// caminho é excluída na iteração seguinte — sem laço infinito: a resposta é
// a do router sem as bases (a rota nominal) e zero dispatch nas duas apps
// escondidas, nos dois gatilhos.
#[tokio::test]
async fn second_hidden_base_matching_the_path_is_excluded_on_the_next_iteration() {
    for weighted in [false, true] {
        let label = if weighted { "second-w" } else { "second" };
        let root = tempfile::tempdir().unwrap();
        let (state, log) = if weighted {
            logging_state_weighted(root.path().to_path_buf())
        } else {
            logging_state(root.path().to_path_buf())
        };
        let index = state.index.clone();
        let app = build_pipeline(state);

        // Duas apps escondidas cujas bases casam com `/gw/x`: a mais longa
        // (`/gw/x`) é tentada primeiro e recusada; a base `/gw` é tentada na
        // iteração seguinte e também recusada.
        let (status, _i, text) = install(
            &app,
            worker_zip(
                "zero-app",
                "1.0.0",
                "em3-second-a",
                "base: /gw\nhosts:\n  - a.example\n",
            ),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        let (status, _i, text) = install(
            &app,
            worker_zip(
                "second-app",
                "1.0.0",
                "em3-second-b",
                "base: /gw/x\nhosts:\n  - b.example\n",
            ),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{text}");
        if weighted {
            let policy = parse_routing_policy(
                br#"{"name":"zero-app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
            )
            .unwrap();
            persist_routing_policy(&index, &policy).unwrap();
            fs::write(
                root.path().join("zero-app").join(".edger-revision"),
                "test-rev\nstaged=true\n",
            )
            .unwrap();
        }

        // Ausência: Router sem as duas bases.
        let absent_root = tempfile::tempdir().unwrap();
        let (absent_state, _absent_log) = if weighted {
            logging_state_weighted(absent_root.path().to_path_buf())
        } else {
            logging_state(absent_root.path().to_path_buf())
        };
        let absent = build_pipeline(absent_state);

        let hidden = get_full_fixed(&app, "/gw/x", None).await;
        let absent_resp = get_full_fixed(&absent, "/gw/x", None).await;
        assert_same_full_response(&format!("{label} /gw/x"), &hidden, &absent_resp);
        assert_eq!(
            hidden.0,
            StatusCode::NOT_FOUND,
            "a resposta é a do router sem as bases"
        );
        assert_eq!(message(&hidden.2), "worker not found: gw");
        assert!(
            log.0
                .lock()
                .unwrap()
                .iter()
                .all(|name| name != "zero-app" && name != "second-app"),
            "zero dispatch nas duas apps escondidas ({label}): {:?}",
            log.0.lock().unwrap()
        );
    }
}
