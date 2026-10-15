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
use axum::http::{Request, StatusCode};
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
    zip_package(&[
        (
            "manifest.yaml",
            &format!(
                "name: {name}\nversion: \"{version}\"\nvisibility: public\nentrypoint: index.ts\nkind: fetch\n{manifest_extra}"
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
