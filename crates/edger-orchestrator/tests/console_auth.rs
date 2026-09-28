//! Contrato HTTP do login por senha da console EdgeR (root user + sessões
//! `ses-` persistentes): endpoints, rate limit por IP real, origem,
//! convivência com root key/`egk_`/OIDC e falha fechada do store.
//!
//! Os testes injetam `ConnectInfo` (IP real do listener) como em produção —
//! `X-Forwarded-For` do cliente nunca entra no bucket de rate limit.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use edger_core::WorkerManifest;
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, load_seed_password, ApiKeyService, ConsoleAuthService, ControlAuth,
    ControlAuthConfig, ManifestIndex, OrchestratorState, ServerState, SESSION_PREFIX,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::{json, Value};
use tower::{Service, ServiceExt};

const ROOT_KEY: &str = "test-root";
const STRONG: &str = "Str0ng!Passw0rd";
const STRONG_2: &str = "An0ther!Passw0rd";
const IP_A: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
const IP_B: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 2);

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn insert_worker(index: &mut ManifestIndex, name: &str) {
    index
        .insert(
            PathBuf::from(format!("/workers/{name}")),
            WorkerManifest {
                name: name.into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
}

fn state_with_console(console: Option<Arc<ConsoleAuthService>>) -> OrchestratorState {
    let mut auth = ControlAuth::with_static_key(ROOT_KEY);
    auth = auth.with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()));
    if let Some(console) = console {
        auth = auth.with_console_service(console);
    }
    let mut index = ManifestIndex::new();
    insert_worker(&mut index, "hello");
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

/// Open mode completo: sem root key, sem OIDC, sem store de console.
fn open_state() -> OrchestratorState {
    let mut index = ManifestIndex::new();
    insert_worker(&mut index, "hello");
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    let state = OrchestratorState {
        server,
        pool,
        index,
        auth: ControlAuth::new(ControlAuthConfig::default()),
    };
    assert!(state.auth.is_open());
    state
}

fn seeded_console() -> Arc<ConsoleAuthService> {
    let service = Arc::new(ConsoleAuthService::in_memory().unwrap());
    service.seed_root_if_empty(Some(STRONG)).unwrap();
    service
}

fn json_body(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn login_body(username: &str, password: &str) -> Value {
    json!({ "username": username, "password": password })
}

/// Requisição de teste com IP REAL injetado (padrão produção: o listener
/// expõe `ConnectInfo`); `headers` são pares (name, value).
async fn send_with_ip(
    app: Router,
    ip: Ipv4Addr,
    method: &str,
    uri: &str,
    headers: Vec<(&str, String)>,
    body: Vec<u8>,
) -> (StatusCode, HeaderMap, Value, String) {
    let mut make = app.into_make_service_with_connect_info::<SocketAddr>();
    let service = make
        .call(SocketAddr::new(IpAddr::V4(ip), 54321))
        .await
        .unwrap();
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let response = service
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, response_headers, json, text)
}

fn host_headers() -> Vec<(&'static str, String)> {
    vec![("host", "edger.local".into())]
}

fn bearer_header(token: &str) -> (&str, String) {
    ("authorization", format!("Bearer {token}"))
}

async fn login(app: Router, ip: Ipv4Addr, username: &str, password: &str) -> (StatusCode, Value) {
    let (status, _headers, json, _text) = send_with_ip(
        app,
        ip,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body(username, password)),
    )
    .await;
    (status, json)
}

#[tokio::test]
async fn login_options_reports_store_state_without_secrets() {
    // Semeado: ambos true.
    let app = build_pipeline(state_with_console(Some(seeded_console())));
    let (status, _h, json, text) = send_with_ip(
        app,
        IP_A,
        "GET",
        "/api/admin/login-options",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json, json!({ "passwordEnabled": true, "rootSeeded": true }));

    // Store vazio (sem semente): ambos false.
    let empty = Arc::new(ConsoleAuthService::in_memory().unwrap());
    let app = build_pipeline(state_with_console(Some(empty)));
    let (status, _h, json, _text) = send_with_ip(
        app,
        IP_A,
        "GET",
        "/api/admin/login-options",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({ "passwordEnabled": false, "rootSeeded": false })
    );

    // Sem store de console (só root key): ambos false.
    let app = build_pipeline(state_with_console(None));
    let (status, _h, json, _text) = send_with_ip(
        app,
        IP_A,
        "GET",
        "/api/admin/login-options",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({ "passwordEnabled": false, "rootSeeded": false })
    );
}

#[tokio::test]
async fn login_success_issues_opaque_session_token_with_no_store_caching() {
    let app = build_pipeline(state_with_console(Some(seeded_console())));
    let (status, headers, json, text) = send_with_ip(
        app,
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let token = json["token"].as_str().expect("token").to_string();
    assert!(token.starts_with(SESSION_PREFIX));
    // 32 bytes de CSPRNG em base64url = 43 chars.
    assert_eq!(token.len(), SESSION_PREFIX.len() + 43);
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    // Segredos não vazam na resposta.
    assert!(!text.contains(STRONG));
}

#[tokio::test]
async fn login_failures_are_generic_and_do_not_reveal_root_existence() {
    let seeded = build_pipeline(state_with_console(Some(seeded_console())));
    let (status, _h, json_seeded, _t) = send_with_ip(
        seeded.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("admin", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Usuário desconhecido e senha errada: corpo IDÊNTICO (nada revela se
    // root existe).
    let (status, _h, json_wrong, _t) = send_with_ip(
        seeded,
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", "Wrong!Pass123")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json_seeded, json_wrong);
    assert_eq!(json_seeded["code"], "UNAUTHORIZED");

    // Instância SEM root semeado: a mesma falha genérica (com dummy de
    // timing) — o oráculo não revela a ausência de root.
    let unseeded = build_pipeline(state_with_console(Some(Arc::new(
        ConsoleAuthService::in_memory().unwrap(),
    ))));
    let (status, _h, json_unseeded, _t) = send_with_ip(
        unseeded,
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", "qualquer-coisa-1!")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json_unseeded, json_wrong);
}

#[tokio::test]
async fn login_bad_body_is_400_and_limited() {
    let app = build_pipeline(state_with_console(Some(seeded_console())));
    // JSON malformado.
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        b"{not-json".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Campo desconhecido (deno_unknown_fields).
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&json!({ "username": "root", "password": STRONG, "extra": true })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Campo ausente.
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&json!({ "username": "root" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Corpo acima do limite (JSON limitado).
    let huge = json!({ "username": "root", "password": "a".repeat(8192) });
    let (status, _h, _j, _t) = send_with_ip(
        app,
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&huge),
    )
    .await;
    assert!(
        matches!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE | StatusCode::BAD_REQUEST
        ),
        "corpo acima do 4 KiB deve ser rejeitado, veio {status}"
    );
}

#[tokio::test]
async fn session_authenticates_admin_endpoints_via_bearer_and_x_api_key() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(Some(service.clone())));
    let (status, _h, json, text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let token = json["token"].as_str().unwrap().to_string();

    // Bearer.
    let (status, _h, json, text) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&token)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(json["workers"].is_array());

    // X-API-Key (CPanel já envia X-API-Key).
    let (status, _h, _json, text) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(("x-api-key", token.clone())))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // /api/admin/session: a sessão vira root_principal enquanto válida.
    let (status, _h, json, text) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/session",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&token)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["principal"]["isRoot"], true);
    assert_eq!(json["principal"]["name"], "root");

    // Root only: a sessão (root) acessa.
    let (status, _h, _json, text) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/catalog",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&token)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // Sessão desconhecida (prefixo certo, inexistente): 401.
    let (status, _h, _json, _t) = send_with_ip(
        app,
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header("ses-nao-existe")))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_expiry_fails_closed_on_admin_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("console.db");
    let service = Arc::new(ConsoleAuthService::open(&db).unwrap());
    service.seed_root_if_empty(Some(STRONG)).unwrap();
    let app = build_pipeline(state_with_console(Some(service.clone())));
    let (status, _h, json, text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let token = json["token"].as_str().unwrap().to_string();

    // Sessão viva.
    let (status, _h, _json, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&token)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Envelhecer além do TTL "por fora" do relógio.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE console_sessions SET expires_at = 1", [])
        .unwrap();
    drop(conn);

    // Expirada: falha fechada (401) e a linha é removida (não ressuscita).
    let (status, _h, _json, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&token)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM console_sessions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);

    // Sem o token, root key segue (convivência).
    let (status, _h, _json, _t) = send_with_ip(
        app,
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(ROOT_KEY)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn logout_revokes_only_session_tokens() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(Some(service)));
    let (status, _h, json, _text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = json["token"].as_str().unwrap().to_string();

    // Logout via X-API-Key (padrão CPanel): 204 e a sessão morre.
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/logout",
        host_headers()
            .into_iter()
            .chain(std::iter::once(("x-api-key", token.clone())))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(("x-api-key", token.clone())))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Root key NÃO é revogada pelo logout: 204 (logout local) e segue viva.
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/logout",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(ROOT_KEY)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(ROOT_KEY)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // egk_ key também: 204 sem revogação e segue viva.
    let (status, _h, json, text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/keys",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(ROOT_KEY)))
            .collect(),
        json_body(&json!({ "name": "ci", "permissions": ["workers:read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let egk = json["rawKey"].as_str().unwrap().to_string();
    assert!(egk.starts_with("egk_"));
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/logout",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&egk)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&egk)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Sem credencial: 401.
    let (status, _h, _j, _t) = send_with_ip(
        app,
        IP_A,
        "POST",
        "/api/admin/logout",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn open_mode_logout_ignores_session_shaped_credentials_without_console_store() {
    let app = build_pipeline(open_state());
    for (name, value) in [
        ("x-api-key", "ses-open-mode-test".to_string()),
        ("authorization", "Bearer ses-open-mode-test".to_string()),
    ] {
        let (status, _headers, _json, _text) = send_with_ip(
            app.clone(),
            IP_A,
            "POST",
            "/api/admin/logout",
            host_headers()
                .into_iter()
                .chain(std::iter::once((name, value)))
                .collect(),
            vec![],
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
}

#[tokio::test]
async fn me_password_rotates_sessions_and_returns_new_token() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(Some(service)));
    let (status, _h, json, _text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let old = json["token"].as_str().unwrap().to_string();

    let change = json!({ "current": STRONG, "new": STRONG_2 });
    let (status, headers, json, text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&old)))
            .collect(),
        json_body(&change),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let fresh = json["token"].as_str().expect("novo token").to_string();
    assert!(fresh.starts_with(SESSION_PREFIX));
    assert_ne!(fresh, old);
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );

    // Sessão antiga morre; a nova vive (a troca devolve o token novo).
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&old)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&fresh)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Senha antiga não entra mais; a nova entra.
    let (status, _j) = login(app.clone(), IP_A, "root", STRONG).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _j) = login(app, IP_A, "root", STRONG_2).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn me_password_requires_session_credential() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(Some(service)));
    let (status, _h, json, _text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = json["token"].as_str().unwrap().to_string();
    let change = json!({ "current": STRONG, "new": STRONG_2 });

    // Root key NÃO troca a senha (a troca vem da sessão, não do token).
    let (status, _h, json, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(ROOT_KEY)))
            .collect(),
        json_body(&change),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "FORBIDDEN");

    // Sessão revogada: 401 (a transação revalidaria no commit; aqui já cai
    // antes do body).
    send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/logout",
        host_headers()
            .into_iter()
            .chain(std::iter::once(("x-api-key", token.clone())))
            .collect(),
        vec![],
    )
    .await;
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&token)))
            .collect(),
        json_body(&change),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Sem credencial: 401.
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers(),
        json_body(&change),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Sessão válida: senha atual errada → 401 genérica; nova fraca → 400.
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let live = _j["token"].as_str().unwrap().to_string();
    let (status, _h, json, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&live)))
            .collect(),
        json_body(&json!({ "current": "Wrong!Pass123", "new": STRONG_2 })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["code"], "UNAUTHORIZED");
    let (status, _h, json, _t) = send_with_ip(
        app,
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&live)))
            .collect(),
        json_body(&json!({ "current": STRONG, "new": "weak" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn forged_origin_is_rejected_on_login_and_password_change() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(Some(service)));
    let (status, _h, json, _text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = json["token"].as_str().unwrap().to_string();

    // Login com origin forjado: 403 mesmo com corpo válido.
    let (status, _h, json, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers()
            .into_iter()
            .chain(std::iter::once(("origin", "https://evil.example".into())))
            .collect(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "CSRF_DENIED");

    // Browser sem origin mas com sec-fetch cross-site: negado (falha fechado).
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers()
            .into_iter()
            .chain(std::iter::once(("sec-fetch-site", "cross-site".into())))
            .collect(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Same-origin: passa.
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers()
            .into_iter()
            .chain(std::iter::once(("origin", "https://edger.local".into())))
            .collect(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // me/password com origin forjado: 403.
    let (status, _h, json, _t) = send_with_ip(
        app,
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers()
            .into_iter()
            .chain(
                [
                    bearer_header(&token),
                    ("origin", "https://evil.example".into()),
                ]
                .into_iter(),
            )
            .collect(),
        json_body(&json!({ "current": STRONG, "new": STRONG_2 })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "CSRF_DENIED");
}

#[tokio::test]
async fn login_rate_limit_is_per_real_ip_and_forged_xff_cannot_rotate_bucket() {
    // Budget pequeno (5 falhas / 10 min) para o teste não pagar 20 Argon2.
    let service =
        Arc::new(ConsoleAuthService::in_memory_with_limiter(Duration::from_secs(600), 5).unwrap());
    service.seed_root_if_empty(Some(STRONG)).unwrap();
    let app = build_pipeline(state_with_console(Some(service)));

    // IP A esgota o budget — cada tentativa com X-Forwarded-For FORGADO e
    // diferente (ataque de rotação de bucket).
    for i in 0..5u8 {
        let (status, _h, _j, _t) = send_with_ip(
            app.clone(),
            IP_A,
            "POST",
            "/api/admin/login",
            host_headers()
                .into_iter()
                .chain(std::iter::once((
                    "x-forwarded-for",
                    format!("203.0.113.{i}"),
                )))
                .collect(),
            json_body(&login_body("root", "Wrong!Pass123")),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "tentativa {i}");
    }
    // IP A: budget esgotado — até senha CORreta vira 429 + Retry-After.
    let (status, headers, json, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers()
            .into_iter()
            .chain(std::iter::once(("x-forwarded-for", "203.0.113.250".into())))
            .collect(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["code"], "RATE_LIMITED");
    let retry_after = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .expect("Retry-After em segundos");
    assert!((1..=600).contains(&retry_after));

    // IP B (outra conexão real): segue livre — o XFF forjado do A não
    // rotacionou bucket, e o budget é por IP real.
    let (status, _h, _j, _t) = send_with_ip(
        app,
        IP_B,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn login_without_connect_info_fails_closed() {
    // Sem `ConnectInfo` (teste isolado sem listener): o extractor do axum
    // rejeita ANTES do handler (500 de configuração de servidor) — nenhuma
    // tentativa de credencial é feita e nenhuma sessão é emitida. Em
    // produção o listener (`into_make_service_with_connect_info`) sempre
    // fornece o IP real, então esse caminho só existe em testes isolados.
    let app = build_pipeline(state_with_console(Some(seeded_console())));
    let request = Request::builder()
        .method("POST")
        .uri("/api/admin/login")
        .header("host", "edger.local")
        .body(Body::from(json_body(&login_body("root", STRONG))))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // O mesmo vale para a troca de senha.
    let request = Request::builder()
        .method("POST")
        .uri("/api/admin/me/password")
        .header("host", "edger.local")
        .body(Body::from(vec![]))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn open_mode_preserved_when_no_credential_and_login_is_unavailable() {
    let state = open_state();
    assert!(state.auth.is_open());
    let app = build_pipeline(state);

    // Open mode segue: /api/admin/workers sem credencial (root sintético).
    let (status, _h, _json, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Login sem store: 503 honesto (não 404 mentiroso).
    let (status, _h, json, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json["code"], "CONSOLE_STORE_UNAVAILABLE");

    // login-options sem store: disponibilidade falsa, sem segredos.
    let (status, _h, json, _t) = send_with_ip(
        app,
        IP_A,
        "GET",
        "/api/admin/login-options",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({ "passwordEnabled": false, "rootSeeded": false })
    );
}

#[tokio::test]
async fn root_key_egk_and_session_coexist() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(Some(service)));

    let (status, _h, json, _text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{_text}");
    let session = json["token"].as_str().unwrap().to_string();

    // egk_ key: autentica, NUNCA é root.
    let (status, _h, json, text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/keys",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(ROOT_KEY)))
            .collect(),
        json_body(&json!({ "name": "ci", "permissions": ["workers:read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let egk = json["rawKey"].as_str().unwrap().to_string();
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&egk)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/catalog",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&egk)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A sessão (root) e a root key seguem vivas ao mesmo tempo, nas duas
    // direções.
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/catalog",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&session)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/catalog",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(ROOT_KEY)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // A key egk_ não é afetada pela sessão (e vice-versa).
    let (status, _h, _j, _t) = send_with_ip(
        app,
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&egk)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn db_failure_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("console.db");
    let service = Arc::new(ConsoleAuthService::open(&db).unwrap());
    service.seed_root_if_empty(Some(STRONG)).unwrap();
    let app = build_pipeline(state_with_console(Some(service)));
    let (status, _h, json, _text) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{_text}");
    let token = json["token"].as_str().unwrap().to_string();

    // Dano no banco (drop das tabelas — hostilidade/classe "falha do DB").
    // Sessão (filha) antes do usuário (pai): o FK entre elas exige isso.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("DROP TABLE console_sessions; DROP TABLE console_users;")
        .unwrap();
    drop(conn);

    // Login: erro de store → 503 (NUNCA autentica).
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&login_body("root", STRONG)),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // Sessão: lookup com erro → None → 401.
    let (status, _h, _j, _t) = send_with_ip(
        app.clone(),
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer_header(&token)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // login-options: erro de store → 503.
    let (status, _h, _j, _t) = send_with_ip(
        app,
        IP_A,
        "GET",
        "/api/admin/login-options",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn bootstrap_seeds_once_from_file_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("console.db");
    let pw1 = dir.path().join("pw1");
    std::fs::write(&pw1, format!("{STRONG}\n")).unwrap();
    let pw2 = dir.path().join("pw2");
    std::fs::write(&pw2, format!("{STRONG_2}\n")).unwrap();

    // Boot 1 (banco vazio + semente válida): semeia.
    let seed = load_seed_password(&pw1).unwrap().unwrap();
    let service = ConsoleAuthService::open(&db).unwrap();
    assert!(service.seed_root_if_empty(Some(&seed)).unwrap());

    // Boot 2 (banco com root + semente DIFERENTE): ignora (sem sobrescrever).
    let seed2 = load_seed_password(&pw2).unwrap().unwrap();
    let service = ConsoleAuthService::open(&db).unwrap();
    assert!(!service.seed_root_if_empty(Some(&seed2)).unwrap());
    let ip = IpAddr::from(IP_A);
    assert!(service.login(ip, "root", STRONG).is_ok());
    assert!(service.login(ip, "root", STRONG_2).is_err());

    // Boot sem semente em banco vazio: nenhum usuário criado.
    let db3 = dir.path().join("empty.db");
    let service3 = ConsoleAuthService::open(&db3).unwrap();
    assert!(!service3.seed_root_if_empty(None).unwrap());
    assert!(!service3.has_root_user().unwrap());

    // Arquivo de senha inválido/vazio/fora da política falha o boot.
    let pw_blank = dir.path().join("blank");
    std::fs::write(&pw_blank, "  \n").unwrap();
    assert!(load_seed_password(&pw_blank).is_err());
    let pw_weak = dir.path().join("weak");
    std::fs::write(&pw_weak, "weakpassword").unwrap();
    assert!(load_seed_password(&pw_weak).is_err());
    assert!(load_seed_password(&dir.path().join("missing")).is_err());
}
