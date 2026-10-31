//! Contrato HTTP do item Sistema da console EdgeR (root-only, somente
//! leitura + revogação de sessão por id): `GET /api/admin/sessions`
//! (metadados SEM token/hash, `current` pela credencial, limite 1000 com
//! `truncated`), `DELETE /api/admin/sessions/{id}` (self-revoke negado,
//! id estrito de 32 hex minúsculos, revogação alheia invalida o token) e
//! `GET /api/admin/settings` (allowlist estrita da configuração atual).
//!
//! Os testes injetam `ConnectInfo` (IP real do listener) como em produção.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

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
const OP_PASSWORD: &str = "Op3rator!Passw0rd";
const IP_A: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);

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

/// Estado com root key estática + store de keys; `console` ausente simula
/// instância sem a feature (503 nas rotas de sessão; `passwordEnabled`
/// `false` nas configurações).
fn state_with_console(
    console: Option<Arc<ConsoleAuthService>>,
    server: &ServerState,
) -> OrchestratorState {
    let mut auth = ControlAuth::with_static_key(ROOT_KEY);
    auth = auth.with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()));
    if let Some(console) = console {
        auth = auth.with_console_service(console);
    }
    let mut index = ManifestIndex::new();
    insert_worker(&mut index, "hello");
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server: server.clone(),
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

fn session_headers(token: &str) -> Vec<(&str, String)> {
    host_headers()
        .into_iter()
        .chain(std::iter::once(bearer(token)))
        .collect()
}

async fn login(app: &Router, ip: Ipv4Addr, username: &str, password: &str) -> (StatusCode, Value) {
    let (status, _h, mut json, _text) = send(
        app,
        ip,
        "POST",
        "/api/admin/login",
        host_headers(),
        json_body(&json!({ "username": username, "password": password })),
    )
    .await;
    if status == StatusCode::OK && json["mustChangePassword"] == true {
        let intermediate = format!("{password}!Changed1");
        for (current, new) in [
            (password, intermediate.as_str()),
            (intermediate.as_str(), password),
        ] {
            let (status, _, rotated, text) = send(
                app,
                ip,
                "POST",
                "/api/admin/me/password",
                session_headers(json["token"].as_str().unwrap()),
                json_body(&json!({"current":current,"new":new})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{text}");
            json = rotated;
        }
        json["mustChangePassword"] = json!(false);
    }
    (status, json)
}

async fn create_user(app: &Router, username: &str) -> (StatusCode, Value, String) {
    let (status, _h, json, text) = send(
        app,
        IP_A,
        "POST",
        "/api/admin/users",
        root_headers(),
        json_body(&json!({
            "username": username,
            "password": OP_PASSWORD,
            "permissions": ["workers:read", "keys:manage"],
            "namespaces": ["*"],
            "workers": ["*"]
        })),
    )
    .await;
    (status, json, text)
}

/// O registro de sessão só pode conter os campos permitidos — sem token,
/// sem hash, sem "extra" inventado.
fn assert_session_shape(session: &Value) {
    let object = session
        .as_object()
        .unwrap_or_else(|| panic!("registro malformado: {session}"));
    assert_eq!(object.len(), 9, "campos permitidos: {session}");
    for key in [
        "id",
        "userId",
        "username",
        "role",
        "isRoot",
        "createdAt",
        "expiresAt",
        "current",
        "revocable",
    ] {
        assert!(object.contains_key(key), "faltando {key}: {session}");
    }
    let id = session["id"].as_str().expect("id: {session}");
    assert_eq!(id.len(), 32, "id opaco Uuid simple: {session}");
    assert!(
        id.bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
        "id hex minúsculo: {session}"
    );
    assert!(session["userId"].is_i64());
    assert!(session["createdAt"].is_i64(), "epoch segundos: {session}");
    assert!(session["expiresAt"].is_i64(), "epoch segundos: {session}");
    assert!(session["isRoot"].is_boolean());
    assert!(session["current"].is_boolean());
    assert!(session["revocable"].is_boolean());
}

#[tokio::test]
async fn sessions_gate_current_and_shape_over_http() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(
        Some(service),
        &ServerState::new_unready(),
    ));

    // 401 sem credencial — leitura E mutação.
    for (method, uri) in [
        ("GET", "/api/admin/sessions"),
        (
            "DELETE",
            "/api/admin/sessions/00000000000000000000000000000001",
        ),
    ] {
        let (status, _h, json, text) = send(&app, IP_A, method, uri, host_headers(), vec![]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}: {text}");
        assert_eq!(json["code"], "UNAUTHORIZED");
    }

    // Sessões root e da operadora (o root também administra pela sessão).
    let (status, json) = login(&app, IP_A, "root", STRONG).await;
    assert_eq!(status, StatusCode::OK, "root session login");
    let root_session = json["token"].as_str().unwrap().to_string();
    assert!(root_session.starts_with(SESSION_PREFIX));
    let (status, _json, text) = create_user(&app, "alice").await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, json) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let alice_session = json["token"].as_str().unwrap().to_string();

    // Operador NUNCA é root: 403 nem na leitura nem na mutação.
    for (method, uri) in [
        ("GET", "/api/admin/sessions"),
        (
            "DELETE",
            "/api/admin/sessions/00000000000000000000000000000001",
        ),
    ] {
        let (status, _h, json, text) = send(
            &app,
            IP_A,
            method,
            uri,
            session_headers(&alice_session),
            vec![],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {text}");
        assert_eq!(json["code"], "FORBIDDEN");
    }

    // `egk_` com `keys:manage` também NÃO é root: 403 nas duas direções.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/keys",
        root_headers(),
        json_body(&json!({ "name": "ci", "permissions": ["keys:manage"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let egk = json["rawKey"].as_str().unwrap().to_string();
    for (method, uri) in [
        ("GET", "/api/admin/sessions"),
        (
            "DELETE",
            "/api/admin/sessions/00000000000000000000000000000001",
        ),
    ] {
        let (status, _h, json, text) =
            send(&app, IP_A, method, uri, session_headers(&egk), vec![]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {text}");
        assert_eq!(json["code"], "FORBIDDEN");
    }

    // Root estático: 200, shape exato, `no-store`, `truncated` false e SEM
    // sessão corrente (token estático não tem sessão).
    let (status, headers, json, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/sessions",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        json.as_object().map(|object| object.len()),
        Some(2),
        "top: apenas sessions+truncated: {json}"
    );
    assert_eq!(json["truncated"], false);
    let sessions = json["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2, "root + operadora: {json}");
    for session in sessions {
        assert_session_shape(session);
        assert_eq!(session["current"], false, "root estático: {session}");
    }
    // O corpo nunca carrega token/hash (nem substring).
    assert!(!text.contains("token"), "segredo no corpo: {text}");
    assert!(!text.contains("hash"), "segredo no corpo: {text}");

    // Sessão root: exatamente UMA sessão corrente — a dela (root/admin).
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/sessions",
        session_headers(&root_session),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let sessions = json["sessions"].as_array().unwrap();
    let current: Vec<&Value> = sessions
        .iter()
        .filter(|session| session["current"] == true)
        .collect();
    assert_eq!(current.len(), 1, "uma única corrente: {json}");
    assert_eq!(current[0]["username"], "root");
    assert_eq!(current[0]["role"], "admin");
    assert_eq!(current[0]["isRoot"], true);
}

#[tokio::test]
async fn sessions_omit_expired_and_inactive_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("console.db");
    let service = Arc::new(ConsoleAuthService::open(&path).unwrap());
    service.seed_root_if_empty(Some(STRONG)).unwrap();
    let app = build_pipeline(state_with_console(
        Some(service.clone()),
        &ServerState::new_unready(),
    ));

    // Operadoras criadas; bob é DESATIVADO via API (a sessão dele, se
    // existisse, já morreria na desativação).
    let (status, _json, text) = create_user(&app, "alice").await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, user_json, text) = create_user(&app, "bob").await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let bob_id = user_json["user"]["id"].as_i64().unwrap();
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "PATCH",
        &format!("/api/admin/users/{bob_id}"),
        root_headers(),
        json_body(&json!({ "disabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // Sessões direto no banco (hash fake, sem Argon2):
    //  - alice (ativa), válida      -> APARECE
    //  - bob (inativa), válida      -> OMITIDA (usuário inativo)
    //  - root, expirada             -> OMITIDA (expiresAt <= now)
    let now: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let conn = rusqlite::Connection::open(&path).unwrap();
    let user_ids: Vec<(i64, String)> = conn
        .prepare("SELECT id, username FROM console_users ORDER BY username")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let root_id = user_ids
        .iter()
        .find(|(_, username)| username == "root")
        .unwrap()
        .0;
    let alice_id = user_ids
        .iter()
        .find(|(_, username)| username == "alice")
        .unwrap()
        .0;
    let bob_id = user_ids
        .iter()
        .find(|(_, username)| username == "bob")
        .unwrap()
        .0;
    let alive_id = format!("{:032x}", 101);
    let inactive_id = format!("{:032x}", 102);
    let expired_id = format!("{:032x}", 103);
    for (user_id, id, created_at, expires_at) in [
        (alice_id, &alive_id, now - 100, now + 3_600),
        (bob_id, &inactive_id, now - 200, now + 3_600),
        (root_id, &expired_id, now - 300, now - 60),
    ] {
        conn.execute(
            "INSERT INTO console_sessions (id, user_id, token_hash, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, user_id, format!("unit-{id}"), created_at, expires_at],
        )
        .unwrap();
    }
    drop(conn);

    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/sessions",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let sessions = json["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "só a linha viva da operadora: {json}");
    assert_session_shape(&sessions[0]);
    assert_eq!(sessions[0]["id"], alive_id);
    assert_eq!(sessions[0]["username"], "alice");
    assert_eq!(sessions[0]["isRoot"], false);
    assert_eq!(sessions[0]["role"], "operator");
    assert_eq!(sessions[0]["current"], false);
    assert!(!text.contains(&inactive_id), "inativa vazou: {text}");
    assert!(!text.contains(&expired_id), "expirada vazou: {text}");
}

#[tokio::test]
async fn revoke_session_by_id_over_http() {
    let service = seeded_console();
    let app = build_pipeline(state_with_console(
        Some(service),
        &ServerState::new_unready(),
    ));

    let (status, json) = login(&app, IP_A, "root", STRONG).await;
    assert_eq!(status, StatusCode::OK, "root session login");
    let root_session = json["token"].as_str().unwrap().to_string();
    let (status, _json, text) = create_user(&app, "alice").await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (status, json) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let alice_session = json["token"].as_str().unwrap().to_string();

    // Ids reais pelas próprias listagens (sem segredos).
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/sessions",
        session_headers(&root_session),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let sessions = json["sessions"].as_array().unwrap();
    let own_id = sessions
        .iter()
        .find(|session| session["current"] == true)
        .unwrap()
        .clone()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = sessions
        .iter()
        .find(|session| session["username"] == "alice")
        .unwrap()
        .clone()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // (1) Self-revoke: 403 FORBIDDEN (a sessão sai pelo logout) e a
    // credencial segue autenticada em seguida.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "DELETE",
        &format!("/api/admin/sessions/{own_id}"),
        session_headers(&root_session),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(json["code"], "FORBIDDEN");
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/sessions",
        session_headers(&root_session),
        vec![],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "self-revoke negado mantém auth: {text}"
    );

    // (2) Revogar a sessão ALHEIA: 200 {ok:true} e o token da operadora
    // deixa de autenticar na hora.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "DELETE",
        &format!("/api/admin/sessions/{alice_id}"),
        session_headers(&root_session),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json, json!({ "ok": true }));
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/session",
        session_headers(&alice_session),
        vec![],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "token revogado morre: {text}"
    );
    assert_eq!(json["code"], "UNAUTHORIZED");
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/sessions",
        session_headers(&root_session),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sessions = json["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "só a sessão root sobrou: {json}");
    assert_eq!(sessions[0]["id"], own_id);

    // (3) Id desconhecido no formato válido: 404 NOT_FOUND.
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "DELETE",
        &format!("/api/admin/sessions/{:032x}", 777),
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert_eq!(json["code"], "NOT_FOUND");

    // (4) Ids hostis (classe inteira): 400 VALIDATION_ERROR, nunca SQL.
    let hostile_ids = [
        "abc",
        "0000000000000000000000000000001",
        "000000000000000000000000000000111",
        "0000000000000000000000000000000g",
        "0000000000000000000000000000000G",
        "0000000000000000000000000000-001",
        "00000000000000000000000000000001%3B%20DROP%20TABLE%20console_sessions--",
    ];
    for id in hostile_ids {
        let (status, _h, json, text) = send(
            &app,
            IP_A,
            "DELETE",
            &format!("/api/admin/sessions/{id}"),
            root_headers(),
            vec![],
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "id hostil aceito: {id} {text}"
        );
        assert_eq!(json["code"], "VALIDATION_ERROR", "id hostil: {id}");
    }

    // (5) Origin hostil (requisição com cara de browser): 403 CSRF_DENIED —
    // e o par com MESMA origem passa o check de origem (cai no 403 de
    // self-revoke, provando que a origem foi o que importava).
    let hostile_origin = vec![
        ("host", "edger.local".to_string()),
        ("origin", "https://evil.example".to_string()),
        bearer(&root_session),
    ];
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "DELETE",
        &format!("/api/admin/sessions/{own_id}"),
        hostile_origin,
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(json["code"], "CSRF_DENIED");
    let same_origin = vec![
        ("host", "edger.local".to_string()),
        ("origin", "https://edger.local".to_string()),
        bearer(&root_session),
    ];
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "DELETE",
        &format!("/api/admin/sessions/{own_id}"),
        same_origin,
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "FORBIDDEN", "mesma origem passa o check CSRF");
}

#[tokio::test]
async fn settings_allowlist_and_flags() {
    // Flags off (default) + console presente: allowlist exata e valores
    // reais das políticas fixas.
    let service = seeded_console();
    let app = build_pipeline(state_with_console(
        Some(service),
        &ServerState::new_unready(),
    ));
    let (status, headers, json, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/settings",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let object = json.as_object().unwrap();
    assert_eq!(object.len(), 9, "allowlist estrita: {json}");
    for key in [
        "runtimeVersion",
        "tenantRoutingEnabled",
        "weightedRoutingEnabled",
        "passwordEnabled",
        "sessionTtlSeconds",
        "passwordMinLength",
        "passwordMaxLength",
        "loginWindowSeconds",
        "loginMaxFailures",
    ] {
        assert!(object.contains_key(key), "faltando {key}: {json}");
    }
    assert_eq!(json["runtimeVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["tenantRoutingEnabled"], false);
    assert_eq!(json["weightedRoutingEnabled"], false);
    assert_eq!(json["passwordEnabled"], true);
    assert_eq!(json["sessionTtlSeconds"], 7 * 24 * 60 * 60);
    assert_eq!(json["passwordMinLength"], 12);
    assert_eq!(json["passwordMaxLength"], 128);
    assert_eq!(json["loginWindowSeconds"], 900);
    assert_eq!(json["loginMaxFailures"], 20);

    // Gates: 401 sem credencial, 403 operador e `egk_` com keys:manage.
    let (status, _h, json, _t) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/settings",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(json["code"], "UNAUTHORIZED");
    let (_status, json) = login(&app, IP_A, "root", STRONG).await;
    let root_session = json["token"].as_str().unwrap().to_string();
    let (status, _json, text) = create_user(&app, "alice").await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (_status, json) = login(&app, IP_A, "alice", OP_PASSWORD).await;
    let alice_session = json["token"].as_str().unwrap().to_string();
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/settings",
        session_headers(&alice_session),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "POST",
        "/api/admin/keys",
        root_headers(),
        json_body(&json!({ "name": "ci", "permissions": ["keys:manage"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let egk = json["rawKey"].as_str().unwrap().to_string();
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/settings",
        session_headers(&egk),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    // Sessão root também lê.
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/settings",
        session_headers(&root_session),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // Flags on: o endpoint reflete o estado ATUAL do servidor.
    let server = ServerState::new_unready();
    server.enable_tenant_routing();
    server.enable_weighted_routing();
    let app = build_pipeline(state_with_console(Some(seeded_console()), &server));
    let (status, _h, json, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/settings",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["tenantRoutingEnabled"], true);
    assert_eq!(json["weightedRoutingEnabled"], true);
}

#[tokio::test]
async fn console_absent_sessions_503_settings_password_disabled() {
    let app = build_pipeline(state_with_console(None, &ServerState::new_unready()));

    // Sessões: 503 honesto (leitura E mutação) — gestão indisponível é
    // estado operacional, não 404 mentiroso.
    for (method, uri) in [
        ("GET", "/api/admin/sessions"),
        (
            "DELETE",
            "/api/admin/sessions/00000000000000000000000000000001",
        ),
    ] {
        let (status, _h, json, text) = send(&app, IP_A, method, uri, root_headers(), vec![]).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {uri}: {text}"
        );
        assert_eq!(json["code"], "CONSOLE_STORE_UNAVAILABLE");
    }
    // 401 sem credencial precede o 503 de store.
    let (status, _h, _j, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/sessions",
        host_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");

    // Configurações: 200 com `passwordEnabled` false (reflete login-options)
    // e os demais campos reais; nada de segredo.
    let (status, headers, json, text) = send(
        &app,
        IP_A,
        "GET",
        "/api/admin/settings",
        root_headers(),
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(json["passwordEnabled"], false);
    assert_eq!(json["tenantRoutingEnabled"], false);
    assert_eq!(json["weightedRoutingEnabled"], false);
    assert_eq!(json["sessionTtlSeconds"], 7 * 24 * 60 * 60);
    assert_eq!(json["runtimeVersion"], env!("CARGO_PKG_VERSION"));
}
