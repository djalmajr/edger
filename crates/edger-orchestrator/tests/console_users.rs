//! Contrato HTTP da gestão de usuários adicionais da console EdgeR
//! (root-only): endpoints, shape exato do registro esperado pelo cPanel
//! (`parseAdminUser` de `workers/core/cpanel/src/lib/api.ts`), gates de
//! permissão/namespace/worker, 401/403, revogação de sessão em
//! desativação/redução/reset/exclusão, erros de login genéricos e o offload
//! limitado dos cálculos Argon2 (spawn_blocking + slots compartilhados).
//!
//! Os testes injetam `ConnectInfo` (IP real do listener) como em produção.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use edger_core::WorkerManifest;
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, ApiKeyService, ConsoleAuthService, ControlAuth, ManifestIndex,
    OrchestratorState, ServerState, SESSION_PREFIX,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::{json, Value};
use tower::{Service, ServiceExt};

const ROOT_KEY: &str = "test-root";
const STRONG: &str = "Str0ng!Passw0rd";
const STRONG_2: &str = "An0ther!Passw0rd";
const OP_PASSWORD: &str = "Op3rator!Passw0rd";
const OP_PASSWORD_2: &str = "Op3rator!Passw0rd2";
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

fn state_with_console(console: Arc<ConsoleAuthService>) -> OrchestratorState {
    let mut auth = ControlAuth::with_static_key(ROOT_KEY);
    auth = auth.with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()));
    auth = auth.with_console_service(console);
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

fn seeded_console() -> Arc<ConsoleAuthService> {
    let service = Arc::new(ConsoleAuthService::in_memory().unwrap());
    service.seed_root_if_empty(Some(STRONG)).unwrap();
    service
}

fn json_body(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

/// Requisição de teste com IP REAL injetado (padrão produção: o listener
/// expõe `ConnectInfo`); `headers` são pares (name, value).
async fn send(
    app: &Router,
    ip: Ipv4Addr,
    method: &str,
    uri: &str,
    headers: Vec<(&str, String)>,
    body: Vec<u8>,
) -> (StatusCode, HeaderMap, Value, String) {
    let mut make = app
        .clone()
        .into_make_service_with_connect_info::<SocketAddr>();
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

fn bearer(token: &str) -> (&str, String) {
    ("authorization", format!("Bearer {token}"))
}

fn root_headers() -> Vec<(&'static str, String)> {
    host_headers()
        .into_iter()
        .chain(std::iter::once(bearer(ROOT_KEY)))
        .collect()
}

async fn login(app: &Router, ip: Ipv4Addr, username: &str, password: &str) -> (StatusCode, Value) {
    let (status, _h, json, _text) = send(
        app,
        ip,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&json!({ "username": username, "password": password })),
    )
    .await;
    (status, json)
}

/// Espelho do `parseAdminUser` do cPanel: o registro precisa ser INTEIRO —
/// qualquer campo ausente/fora do tipo falha a UI. Sem senha/hash nunca.
fn assert_admin_user_shape(value: &Value) {
    assert!(value.is_object(), "registro malformado: {value}");
    assert!(
        value.get("id").and_then(Value::as_i64).is_some(),
        "id: {value}"
    );
    assert!(
        value.get("username").and_then(Value::as_str).is_some(),
        "username: {value}"
    );
    assert!(
        value.get("role").and_then(Value::as_str).is_some(),
        "role: {value}"
    );
    assert!(
        value.get("isRoot").and_then(Value::as_bool).is_some(),
        "isRoot: {value}"
    );
    assert!(
        value.get("disabled").and_then(Value::as_bool).is_some(),
        "disabled: {value}"
    );
    assert!(
        value.get("createdAt").and_then(Value::as_i64).is_some(),
        "createdAt (epoch segundos): {value}"
    );
    for field in ["permissions", "namespaces", "workers"] {
        let array = value
            .get(field)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{field} ausente: {value}"));
        assert!(
            array.iter().all(Value::is_string),
            "{field} com entrada não-string: {value}"
        );
    }
    // Segredos nunca vazam no registro.
    assert!(value.get("password").is_none(), "senha vazou: {value}");
    assert!(value.get("passwordHash").is_none(), "hash vazou: {value}");
    assert!(value.get("hash").is_none(), "hash vazou: {value}");
}

#[tokio::test]
async fn user_lifecycle_over_http_with_exact_cpanel_shape() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(service.clone()));

    // LISTA: root aparece (a UI marca a linha como imutável) com shape
    // exato, sem segredos.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/users",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(
        json.get("users").and_then(Value::as_array).is_some(),
        "{json}"
    );
    let users = json["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_admin_user_shape(&users[0]);
    assert_eq!(users[0]["username"], "root");
    assert_eq!(users[0]["isRoot"], true);
    assert_eq!(users[0]["role"], "admin");
    assert_eq!(users[0]["disabled"], false);

    // CRIAÇÃO via SESSÃO root (a sessão também administra): shape exato do
    // `{user:{...}}`, 201, senha nunca no corpo.
    let (status, json) = login(&app, IP_A, "root", STRONG).await;
    assert_eq!(status, StatusCode::OK, "root session login");
    let root_session = json["token"].as_str().unwrap().to_string();
    assert!(root_session.starts_with(SESSION_PREFIX));
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&root_session)))
            .collect(),
        json_body(&json!({
            "username": "alice",
            "password": OP_PASSWORD,
            "permissions": ["workers:read", "keys:manage"],
            "namespaces": ["*"],
            "workers": ["*"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_admin_user_shape(&json["user"]);
    assert_eq!(json["user"]["username"], "alice");
    assert_eq!(json["user"]["isRoot"], false);
    assert_eq!(json["user"]["role"], "operator");
    assert_eq!(json["user"]["disabled"], false);
    assert_eq!(
        json["user"]["permissions"],
        json!(["workers:read", "keys:manage"])
    );
    let alice_id = json["user"]["id"].as_i64().unwrap();
    // A senha digitada NUNCA aparece na resposta.
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/users",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!text.contains(OP_PASSWORD));

    // OPERADOR LOGA e usa as permissões gravadas — e só elas.
    let (status, json) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let alice = json["token"].as_str().unwrap().to_string();
    let alice_headers = || {
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&alice)))
            .collect::<Vec<_>>()
    };
    // workers:read presente.
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/workers",
        alice_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Sem root: catálogo negado.
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/catalog",
        alice_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // keys:manage presente: cria key.
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/keys",
        alice_headers(),
        json_body(&json!({ "name": "k1", "permissions": ["workers:read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // OPERADOR NÃO GERENCIA USUÁRIOS (nem a si mesmo): 403 em tudo e banco
    // intacto.
    let (status, _h, _json, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        alice_headers(),
        json_body(&json!({
            "username": "bob", "password": OP_PASSWORD_2,
            "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    for (method, uri, body) in [
        (
            "PATCH",
            &format!("/api/admin/users/{alice_id}"),
            json!({ "disabled": true }),
        ),
        (
            "DELETE",
            &format!("/api/admin/users/{alice_id}"),
            Value::Null,
        ),
        (
            "POST",
            &format!("/api/admin/users/{alice_id}/reset-password"),
            json!({ "password": OP_PASSWORD_2 }),
        ),
    ] {
        let (status, _h, _j, text) = send(
            &app,
            IP_A,
            method,
            uri,
            alice_headers(),
            if body.is_null() {
                vec![]
            } else {
                json_body(&body)
            },
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {text}");
    }
    // Banco intacto: a operadora segue viva, logando e com as permissões.
    let (status, json) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{json}");

    // `egk_` com keys:manage NÃO vira root: 403 em todas as rotas de users.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/keys",
        root_headers(),
        json_body(&json!({ "name": "ci", "permissions": ["keys:manage", "workers:read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let egk = json["rawKey"].as_str().unwrap().to_string();
    let egk_headers = || {
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&egk)))
            .collect::<Vec<_>>()
    };
    let (status, _h, json, text) =
        send(&app, IP_A, "GET", "/api/admin/users", egk_headers(), vec![]).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(json["code"], "FORBIDDEN");
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        egk_headers(),
        json_body(&json!({
            "username": "bob", "password": OP_PASSWORD_2,
            "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // SEM credencial: 401 (gate fechado — root sintético não existe).
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/users",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["code"], "UNAUTHORIZED");

    // DESATIVAÇÃO: a sessão viva morre NA HORA (estado atual, sem cache) e o
    // login com senha certa também é negado; reativação devolve tudo.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "PATCH",
        &format!("/api/admin/users/{alice_id}"),
        root_headers(),
        json_body(&json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_admin_user_shape(&json["user"]);
    assert_eq!(json["user"]["disabled"], true);
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/workers",
        alice_headers(),
        vec![],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "sessão desativada não pode mais entrar"
    );
    let (status, json) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{json}");
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "PATCH",
        &format!("/api/admin/users/{alice_id}"),
        root_headers(),
        json_body(&json!({ "disabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, _j) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "reativada");

    // REDUÇÃO DE ESCOPO IMEDIATA: a mesma sessão perde `keys:manage` na hora
    // (e a revogação por redução também — sessão nova reflete o escopo).
    let (status, json) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let alice = json["token"].as_str().unwrap().to_string();
    let alice_headers = || {
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&alice)))
            .collect::<Vec<_>>()
    };
    let (status, _h, _json, text) = send(
        &app,
        IP_A,
        "PATCH",
        &format!("/api/admin/users/{alice_id}"),
        root_headers(),
        json_body(&json!({ "permissions": ["workers:read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    // A redução revogou a sessão (privilégio antigo não segue em cache).
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/workers",
        alice_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Sessão nova carrega apenas o escopo reduzido.
    let (status, json) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let alice = json["token"].as_str().unwrap().to_string();
    let alice_headers = || {
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&alice)))
            .collect::<Vec<_>>()
    };
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/workers",
        alice_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/keys",
        alice_headers(),
        json_body(&json!({ "name": "k2", "permissions": ["workers:read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    // Devolve a permissão (expansão não revoga).
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "PATCH",
        &format!("/api/admin/users/{alice_id}"),
        root_headers(),
        json_body(&json!({ "permissions": ["workers:read", "keys:manage"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/keys",
        alice_headers(),
        json_body(&json!({ "name": "k3", "permissions": ["workers:read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // RESET DE SENHA: sessão morre na mesma transação; senha antiga 401;
    // nova entra.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "POST",
        &format!("/api/admin/users/{alice_id}/reset-password"),
        root_headers(),
        json_body(&json!({ "password": OP_PASSWORD_2 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_admin_user_shape(&json["user"]);
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/workers",
        alice_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "reset revogou a sessão");
    let (status, _j) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _j) = login(&app, IP_A, "alice", OP_PASSWORD_2).await;
    assert_eq!(status, StatusCode::OK);

    // EXCLUSÃO: usuário e sessões saem; login 401; lista sem ela.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "DELETE",
        &format!("/api/admin/users/{alice_id}"),
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["deleted"], true);
    let (status, _j) = login(&app, IP_A, "alice", OP_PASSWORD_2).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/users",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["users"]
        .as_array()
        .unwrap()
        .iter()
        .all(|user| user["username"] == "root"));

    // Id inexistente: 404 honesto (não 500, não 403).
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "PATCH",
        "/api/admin/users/99999",
        root_headers(),
        json_body(&json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["code"], "NOT_FOUND");
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "DELETE",
        "/api/admin/users/99999",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn root_user_is_immutable_and_reserved_over_http() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(service));
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/users",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let root_id = json["users"][0]["id"].as_i64().unwrap();

    // Mutações no root: 403 (imutável), banco intacto.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "PATCH",
        &format!("/api/admin/users/{root_id}"),
        root_headers(),
        json_body(&json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(json["code"], "USER_IMMUTABLE");
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "POST",
        &format!("/api/admin/users/{root_id}/reset-password"),
        root_headers(),
        json_body(&json!({ "password": STRONG_2 })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "USER_IMMUTABLE");
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "DELETE",
        &format!("/api/admin/users/{root_id}"),
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Root segue intacto e autenticando (a senha NÃO foi resetada).
    let (status, _j) = login(&app, IP_A, "root", STRONG_2).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _j) = login(&app, IP_A, "root", STRONG).await;
    assert_eq!(status, StatusCode::OK);

    // Criação com username reservado: 400 (case-insensitive).
    for username in ["root", "ROOT", "RoOt"] {
        let (status, _h, json, text) = send(
            &app,
            IP_A,
            "POST",
            "/api/admin/users",
            root_headers(),
            json_body(&json!({
                "username": username, "password": STRONG_2,
                "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{username}: {text}");
        assert_eq!(json["code"], "VALIDATION_ERROR");
    }
}

#[tokio::test]
async fn user_creation_validation_rejects_hostile_inputs_over_http() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(service));

    // Usernames hostis: 400, banco intacto.
    for username in [
        "a",              // 1 char
        &"a".repeat(33),  // 33 chars
        "Alice",          // maiúscula (charset só minúscula)
        "al ice",         // espaço
        "ali\u{00e7}ice", // Unicode fora do charset
        "al\x1bice",      // caractere de controle
        ".lead",          // pontuação inicial
        "-lead",          // pontuação inicial
    ] {
        let (status, _h, json, text) = send(
            &app,
            IP_A,
            "POST",
            "/api/admin/users",
            root_headers(),
            json_body(&json!({
                "username": username, "password": OP_PASSWORD,
                "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{username}: {text}");
        assert_eq!(json["code"], "VALIDATION_ERROR", "{username}: {text}");
    }

    // Senha fraca: 400 (antes de qualquer hash).
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        root_headers(),
        json_body(&json!({
            "username": "alice", "password": "weak",
            "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "VALIDATION_ERROR");

    // Permissão desconhecida e `*` indevido: 400.
    for permissions in [
        json!(["bogus:perm"]),
        json!(["*"]),
        json!(["workers:read", "*"]),
    ] {
        let (status, _h, json, text) = send(
            &app,
            IP_A,
            "POST",
            "/api/admin/users",
            root_headers(),
            json_body(&json!({
                "username": "alice", "password": OP_PASSWORD,
                "permissions": permissions, "namespaces": ["*"], "workers": ["*"]
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{permissions}: {text}");
        assert_eq!(json["code"], "VALIDATION_ERROR", "{permissions}");
    }

    // Escopos vazios / entradas vazias / listas ausentes: 400.
    for body in [
        json!({ "username": "alice", "password": OP_PASSWORD, "permissions": [], "namespaces": ["*"], "workers": ["*"] }),
        json!({ "username": "alice", "password": OP_PASSWORD, "permissions": ["workers:read"], "namespaces": [], "workers": ["*"] }),
        json!({ "username": "alice", "password": OP_PASSWORD, "permissions": ["workers:read"], "namespaces": ["*"], "workers": [] }),
        json!({ "username": "alice", "password": OP_PASSWORD, "permissions": ["workers:read"], "namespaces": [""], "workers": ["*"] }),
        json!({ "username": "alice", "password": OP_PASSWORD, "namespaces": ["*"], "workers": ["*"] }),
    ] {
        let (status, _h, json, text) = send(
            &app,
            IP_A,
            "POST",
            "/api/admin/users",
            root_headers(),
            json_body(&body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {text}");
        assert_eq!(json["code"], "VALIDATION_ERROR", "{body}");
    }

    // Campo desconhecido: 400 (deny_unknown_fields).
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        root_headers(),
        json_body(&json!({
            "username": "alice", "password": OP_PASSWORD,
            "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"],
            "role": "admin"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Banco intacto depois de tudo: só o root na lista.
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/users",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["users"].as_array().unwrap().len(), 1);

    // Duplicado: cria uma vez (201), depois 409.
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        root_headers(),
        json_body(&json!({
            "username": "alice", "password": OP_PASSWORD,
            "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        root_headers(),
        json_body(&json!({
            "username": "alice", "password": OP_PASSWORD,
            "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["code"], "USERNAME_TAKEN");

    // Corpo acima do limite (4 KiB): rejeitado (413/400), sem 500.
    let huge = json!({
        "username": "alice",
        "password": OP_PASSWORD,
        "permissions": ["workers:read"],
        "namespaces": ["a".repeat(2048)],
        "workers": ["b".repeat(2048)]
    });
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        root_headers(),
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
async fn operator_session_can_change_own_password_without_root_name() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(service));
    // Operadora com senha própria.
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        root_headers(),
        json_body(&json!({
            "username": "alice", "password": OP_PASSWORD,
            "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, json) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let alice = json["token"].as_str().unwrap().to_string();

    // me/password funciona para QUALQUER usuário de sessão (não só root).
    let (status, headers, json, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&alice)))
            .collect(),
        json_body(&json!({ "current": OP_PASSWORD, "new": OP_PASSWORD_2 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let fresh = json["token"].as_str().expect("novo token").to_string();
    assert!(fresh.starts_with(SESSION_PREFIX));
    assert_ne!(fresh, alice);
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );

    // Sessão antiga morre; a nova vive; senha antiga 401, nova 200.
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&alice)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/workers",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&fresh)))
            .collect(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _j) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _j) = login(&app, IP_A, "alice", OP_PASSWORD_2).await;
    assert_eq!(status, StatusCode::OK);

    // O root segue intacto (a troca não tocou nele).
    let (status, _j) = login(&app, IP_A, "root", STRONG).await;
    assert_eq!(status, StatusCode::OK);

    // Senha atual errada: 401 genérica; nova fraca: 400.
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&fresh)))
            .collect(),
        json_body(&json!({ "current": "Wrong!Pass1", "new": STRONG_2 })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["code"], "UNAUTHORIZED");
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/me/password",
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&fresh)))
            .collect(),
        json_body(&json!({ "current": OP_PASSWORD_2, "new": "weak" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "VALIDATION_ERROR");

    // Credencial não-sessão (root key / egk_): 403.
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/me/password",
        root_headers(),
        json_body(&json!({ "current": STRONG, "new": STRONG_2 })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "FORBIDDEN");
}

#[tokio::test]
async fn login_errors_remain_generic_and_db_stays_intact() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(service));
    // Cria a operadora.
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        root_headers(),
        json_body(&json!({
            "username": "alice", "password": OP_PASSWORD,
            "permissions": ["workers:read"], "namespaces": ["*"], "workers": ["*"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // As três classes de falha (usuário inexistente, senha errada, usuário
    // desativado) devolvem corpo IDÊNTICO — nada revela o motivo.
    let (status, json_unknown) = login(&app, IP_A, "ghost", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, json_wrong) = login(&app, IP_A, "alice", "Wrong!Pass1").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/users",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let alice_id = json["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|user| user["username"] == "alice")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "PATCH",
        &format!("/api/admin/users/{alice_id}"),
        root_headers(),
        json_body(&json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, json_disabled) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json_unknown, json_wrong, "corpos devem ser idênticos");
    assert_eq!(json_wrong, json_disabled, "corpos devem ser idênticos");
    assert_eq!(json_wrong["code"], "UNAUTHORIZED");

    // Banco intacto: nada mudou de fato além da desativação explícita — a
    // senha continua a mesma e reativar devolve o acesso.
    let (status, _h, _j, _t) = send(
        &app,
        IP_A,
        "PATCH",
        &format!("/api/admin/users/{alice_id}"),
        root_headers(),
        json_body(&json!({ "disabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _j) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let _ = alice_id;
}

#[tokio::test]
async fn scoped_operator_cannot_touch_out_of_scope_workers_over_http() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(service));
    // Carol: pode TOGGLE workers, mas o escopo de workers é ["missing"].
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/users",
        root_headers(),
        json_body(&json!({
            "username": "carol", "password": OP_PASSWORD,
            "permissions": ["workers:read", "workers:toggle"],
            "namespaces": ["*"],
            "workers": ["missing"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, json) = login(&app, IP_A, "carol", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let carol = json["token"].as_str().unwrap().to_string();
    let carol_headers = || {
        host_headers()
            .into_iter()
            .chain(std::iter::once(bearer(&carol)))
            .collect::<Vec<_>>()
    };
    // O worker `hello` NÃO está no escopo dela: invisível (404, não 200).
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/workers/hello/disable",
        carol_headers(),
        vec![],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "escopo de worker oculta o recurso"
    );
    assert_eq!(json["code"], "NOT_FOUND");
    // A listagem só traz o que o escopo permite.
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/workers",
        carol_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["workers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|worker| worker["name"] != "hello"));
    // O principal da sessão carrega exatamente o que foi gravado.
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/session",
        carol_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["principal"]["isRoot"], false);
    assert_eq!(json["principal"]["name"], "carol");
    assert_eq!(json["principal"]["workers"], json!(["missing"]));
    assert_eq!(
        json["principal"]["permissions"],
        json!(["workers:read", "workers:toggle"])
    );
}

// --------------------------------------------------------------------------
// Offload limitado dos cálculos Argon2 (P2 da revisão da fatia root):
// login lento NUNCA roda na thread do runtime e o limite compartilhado de
// slots garante que excesso de tentativas NÃO abre fila ilimitada de hashes
// de ~19 MiB. Falha do limite/tarefa nega (503), nunca autentica.
// --------------------------------------------------------------------------

#[tokio::test]
async fn hash_slot_limit_denies_when_saturated_and_releases_on_drop() {
    // Limite mínimo (2 slots) e timeout curto para o teste ser rápido.
    let service = Arc::new(
        ConsoleAuthService::in_memory()
            .unwrap()
            .with_hash_concurrency(2)
            .with_hash_acquire_timeout(Duration::from_millis(300)),
    );
    service.seed_root_if_empty(Some(STRONG)).unwrap();
    let app = build_pipeline(state_with_console(service.clone()));

    // Segura os DOIS slots: nenhum cálculo de senha pode começar.
    let slots = service.hash_slots().clone();
    let p1 = slots.clone().acquire_owned().await.unwrap();
    let p2 = slots.clone().acquire_owned().await.unwrap();

    // Login sob saturação: 503 CONSOLE_BUSY após o timeout (NUNCA autentica,
    // NUNCA 200 e NUNCA 401 — o cálculo simplesmente não andou).
    let started = Instant::now();
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&json!({ "username": "root", "password": STRONG })),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert_eq!(json["code"], "CONSOLE_BUSY");
    assert!(
        elapsed >= Duration::from_millis(250),
        "esperou o timeout: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "não pode travar: {elapsed:?}"
    );
    // Retry-After presente (sinal honesto de "tente de novo").
    let retry_after = _h
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .expect("Retry-After em segundos");
    assert!(retry_after >= 1);

    // Excesso de tentativas NÃO inicia hashes sem limite: 8 logins
    // concorrentes sob saturação — TODOS 503 (nenhum entra em fila).
    let mut handles = (0..8u8)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move {
                let (status, _h, _j, _t) = send(
                    &app,
                    IP_B,
                    "POST",
                    "/api/admin/login",
                    host_headers(),
                    json_body(&json!({ "username": "root", "password": STRONG })),
                )
                .await;
                status
            })
        })
        .collect::<Vec<_>>();
    let mut statuses = Vec::new();
    for handle in handles.drain(..) {
        statuses.push(handle.await.unwrap());
    }
    assert!(
        statuses
            .iter()
            .all(|status| *status == StatusCode::SERVICE_UNAVAILABLE),
        "nenhum login sob saturação pode autenticar: {statuses:?}"
    );

    // Rotas leves seguem respondendo durante a saturação (o runtime não
    // bloqueia — os hashes rodam na pool de blocking).
    let (status, _h, _j, _t) = send(&app, IP_A, "GET", "/livez", host_headers(), vec![]).await;
    assert_eq!(status, StatusCode::OK);

    // Solta os slots: o MESMO login então autentica (o limite é transitório,
    // não uma negação permanente).
    drop(p1);
    drop(p2);
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&json!({ "username": "root", "password": STRONG })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["token"].as_str().is_some());
}

#[tokio::test]
async fn slow_logins_do_not_block_light_routes_and_serialize_under_limit() {
    // 2 slots de hash: os logins (Argon2 real, centenas de ms cada) se
    // serializam na pool de blocking — e o runtime continua servindo
    // `/livez` nesse intervalo.
    let service = Arc::new(
        ConsoleAuthService::in_memory()
            .unwrap()
            .with_hash_concurrency(2)
            .with_hash_acquire_timeout(Duration::from_secs(5)),
    );
    service.seed_root_if_empty(Some(STRONG)).unwrap();
    let app = build_pipeline(state_with_console(service));

    // Custo de referência: um único login errado (Argon2 real).
    let started = Instant::now();
    let (status, _j) = login(&app, IP_A, "root", "Wrong!Pass1").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let single = started.elapsed();

    // 6 logins errados CONCORRENTES (3 rodadas com o limite de 2) + 3
    // consultas a `/livez` no meio: nenhuma pode esperar por Argon2.
    let livez = app.clone();
    let livez_task = tokio::spawn(async move {
        let mut latencies = Vec::new();
        for _ in 0..3 {
            let started = Instant::now();
            let (status, _h, _j, _t) =
                send(&livez, IP_B, "GET", "/livez", host_headers(), vec![]).await;
            assert_eq!(status, StatusCode::OK);
            latencies.push(started.elapsed());
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        latencies
    });
    let started = Instant::now();
    let mut handles = (0..6u8)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move {
                let (status, _j) = login(&app, IP_A, "root", "Wrong!Pass1").await;
                status
            })
        })
        .collect::<Vec<_>>();
    let mut login_statuses = Vec::new();
    for handle in handles.drain(..) {
        login_statuses.push(handle.await.unwrap());
    }
    let total = started.elapsed();
    let livez_latencies = livez_task.await.unwrap();

    // Todos os 6 negados (senha errada) — e a serialização pelo limite
    // prova que a fila de hashes é bounded: 3 rodadas >= 2x o custo unitário
    // (sem limite, 6 cores+ executariam tudo em paralelo ~1x).
    assert!(
        login_statuses
            .iter()
            .all(|status| *status == StatusCode::UNAUTHORIZED),
        "{login_statuses:?}"
    );
    assert!(
        total >= 2 * single,
        "serialização pelo limite: total={total:?} single={single:?}"
    );
    // `/livez` respondeu rápido em todos os pontos (runtime não bloqueou).
    for latency in &livez_latencies {
        assert!(
            *latency < Duration::from_secs(2),
            "/livez demorou {latency:?} durante logins lentos"
        );
    }
}
