//! Cofre de segredos de app (EDG-38): rotas de admin, permissões, escopo,
//! falha fechada e a garantia de que o valor NUNCA aparece em resposta,
//! log ou evento.
//!
//! O valor nunca sai por estas rotas: a listagem traz só nomes e datas.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::WorkerManifest;
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    app_secrets::{AppSecretVault, MAX_SECRET_VALUE_BYTES},
    build_pipeline, load_master_key_file, ApiKeyService, ControlAuth, ManifestIndex,
    OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::{json, Value};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

/// Valor distintivo: se aparecer em qualquer saída capturada, o teste cai.
const SECRET_VALUE: &str = "s3cret-byok-master-zz9X-7f2a";

const ROOT_KEY: &str = "test-root";

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

/// Estado com o cofre PRONTO (chave mestra de 32 bytes). O `TempDir` mantém
/// o banco e a chave vivos até o fim do teste.
fn vaulted_state() -> (OrchestratorState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("secrets-key");
    std::fs::write(&key_file, [42u8; 32]).unwrap();
    let key = load_master_key_file(&key_file).unwrap();
    let vault = AppSecretVault::open(dir.path().join("app-secrets.db"), key).unwrap();
    (state_with_vault(vault), dir)
}

fn state_with_vault(vault: AppSecretVault) -> OrchestratorState {
    let auth = ControlAuth::with_static_key(ROOT_KEY)
        .with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()));
    let mut index = ManifestIndex::new();
    insert_worker(&mut index, "hello");
    insert_worker(&mut index, "other");

    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    server.set_app_secrets(Arc::new(vault));

    OrchestratorState {
        server,
        pool,
        index,
        auth,
    }
}

/// Estado SEM cofre (a instância subiu sem `EDGER_SECRETS_KEY_FILE`): as
/// rotas respondem 503 `SECRETS_UNAVAILABLE`.
fn unvaulted_state() -> OrchestratorState {
    let auth = ControlAuth::with_static_key(ROOT_KEY)
        .with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()));
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

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    api_key: Option<&str>,
    body: Body,
) -> (StatusCode, Value, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(key) = api_key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let response = app.oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed, text)
}

async fn write_secret(
    app: Router,
    key: &str,
    worker: &str,
    secret: &str,
    value: &str,
) -> (StatusCode, Value, String) {
    let body = Body::from(json!({ "value": value }).to_string());
    let request = Request::builder()
        .method("PUT")
        .uri(format!("/api/admin/workers/{worker}/secrets/{secret}"))
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed, text)
}

async fn create_key(app: Router, creator: &str, body: Value) -> (StatusCode, Value, String) {
    send(
        app,
        "POST",
        "/api/admin/keys",
        Some(creator),
        Body::from(body.to_string()),
    )
    .await
}

#[tokio::test]
async fn roundtrip_write_list_delete_never_returns_the_value() {
    let (state, _dir) = vaulted_state();
    let app = build_pipeline(state);

    // Escreve.
    let (status, written, write_text) = write_secret(
        app.clone(),
        ROOT_KEY,
        "pinar",
        "BYOK_MASTER_KEY",
        SECRET_VALUE,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{write_text}");
    assert_eq!(written["worker"], "pinar");
    assert_eq!(written["secret"], "BYOK_MASTER_KEY");
    assert!(
        !write_text.contains(SECRET_VALUE),
        "PUT response: {write_text}"
    );

    // Lista: só nome e datas — o valor não aparece.
    let (status, listed, list_text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/pinar/secrets",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list_text}");
    let entries = listed.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "BYOK_MASTER_KEY");
    assert!(entries[0].get("createdAt").is_some(), "{list_text}");
    assert!(entries[0].get("updatedAt").is_some(), "{list_text}");
    assert!(
        entries[0].get("value").is_none(),
        "the list must never carry the value: {list_text}"
    );
    assert!(
        !list_text.contains(SECRET_VALUE),
        "GET response: {list_text}"
    );

    // Rotação: PUT de novo vale no próximo spawn (aqui: sobrescreve).
    let (status, _, text) =
        write_secret(app.clone(), ROOT_KEY, "pinar", "BYOK_MASTER_KEY", "rotated").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET_VALUE));

    // Apaga.
    let (status, deleted, delete_text) = send(
        app.clone(),
        "DELETE",
        "/api/admin/workers/pinar/secrets/BYOK_MASTER_KEY",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{delete_text}");
    assert_eq!(deleted["status"], "deleted");
    assert!(
        !delete_text.contains(SECRET_VALUE),
        "DELETE response: {delete_text}"
    );

    // Vazio de novo; um segundo delete é 404 (o estado é do segredo).
    let (status, listed, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/pinar/secrets",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(listed.as_array().unwrap().is_empty(), "{text}");
    let (status, missing, _) = send(
        app,
        "DELETE",
        "/api/admin/workers/pinar/secrets/BYOK_MASTER_KEY",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["code"], "NOT_FOUND");
}

#[tokio::test]
async fn an_instance_without_a_master_key_answers_503_on_every_route() {
    let app = build_pipeline(unvaulted_state());

    let (status, body, text) =
        write_secret(app.clone(), ROOT_KEY, "pinar", "K", SECRET_VALUE).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert_eq!(body["code"], "SECRETS_UNAVAILABLE");
    assert!(!text.contains(SECRET_VALUE));

    let (status, body, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/pinar/secrets",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert_eq!(body["code"], "SECRETS_UNAVAILABLE");

    let (status, body, text) = send(
        app,
        "DELETE",
        "/api/admin/workers/pinar/secrets/K",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert_eq!(body["code"], "SECRETS_UNAVAILABLE");
}

#[tokio::test]
async fn write_requires_secrets_write_and_read_only_lists() {
    let (state, _dir) = vaulted_state();
    let app = build_pipeline(state);

    // Key com SÓ secrets:read: lista, não grava, não apaga.
    let (_, created, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({ "name": "reader", "permissions": ["secrets:read"], "workers": ["hello"] }),
    )
    .await;
    let reader = created["rawKey"].as_str().unwrap();

    let (status, body, text) = write_secret(app.clone(), reader, "hello", "K", SECRET_VALUE).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(body["code"], "FORBIDDEN");
    assert!(text.contains("secrets:write"), "{text}");

    let (status, body, text) = send(
        app.clone(),
        "DELETE",
        "/api/admin/workers/hello/secrets/K",
        Some(reader),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(body["code"], "FORBIDDEN");

    // Root escreve; a key de leitura enxerga a listagem.
    let (status, _, text) = write_secret(app.clone(), ROOT_KEY, "hello", "K", SECRET_VALUE).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, listed, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/hello/secrets",
        Some(reader),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(listed[0]["name"], "K");
    assert!(!text.contains(SECRET_VALUE));

    // Sem nenhuma permissão de segredos: nem lista.
    let (_, created, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({ "name": "plain", "permissions": ["workers:read"], "workers": ["hello"] }),
    )
    .await;
    let plain = created["rawKey"].as_str().unwrap();
    let (status, body, _) = send(
        app,
        "GET",
        "/api/admin/workers/hello/secrets",
        Some(plain),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "FORBIDDEN");
}

#[tokio::test]
async fn the_worker_scope_of_the_key_applies_to_the_route_name() {
    let (state, _dir) = vaulted_state();
    let app = build_pipeline(state);

    let (_, created, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({ "name": "pinar-agent", "permissions": ["secrets:read", "secrets:write"], "workers": ["pinar"] }),
    )
    .await;
    let agent = created["rawKey"].as_str().unwrap();

    // Fora do escopo: 403 (a rota não vira 404 mentiroso).
    let (status, body, text) = write_secret(app.clone(), agent, "hello", "K", SECRET_VALUE).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(body["code"], "FORBIDDEN");
    assert!(!text.contains(SECRET_VALUE));
    let (status, body, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/other/secrets",
        Some(agent),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(body["code"], "FORBIDDEN");

    // Dentro do escopo: funciona — e o worker `pinar` NÃO está instalado
    // (o app grava o segredo antes do install que o declara).
    let (status, _, text) =
        write_secret(app.clone(), agent, "pinar", "BYOK_MASTER_KEY", SECRET_VALUE).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, listed, text) = send(
        app,
        "GET",
        "/api/admin/workers/pinar/secrets",
        Some(agent),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(listed[0]["name"], "BYOK_MASTER_KEY");
    assert!(!text.contains(SECRET_VALUE));
}

/// (F1) o escopo de NAMESPACE da chave também vale para o `{name}` da
/// rota — o MESMO predicado único das outras rotas por worker
/// (`ManifestIndex::principal_can_access_worker_name`): uma key com
/// `namespaces: ["@acme"]` e `workers: ["*"]` não toca worker sem
/// namespace nem de outro namespace (403 nas três rotas), mesmo com
/// `workers: ["*"]`, e atua sobre `@acme/app`.
#[tokio::test]
async fn the_namespace_scope_of_the_key_applies_to_the_route_name() {
    let (state, _dir) = vaulted_state();
    let app = build_pipeline(state);

    let (_, created, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({
            "name": "acme-only",
            "permissions": ["secrets:read", "secrets:write"],
            "namespaces": ["@acme"],
            "workers": ["*"],
        }),
    )
    .await;
    let acme = created["rawKey"].as_str().unwrap();

    // Sem namespace (`pinar`) ou de outro namespace (`@other/x`): 403 nas
    // três rotas — o `workers: ["*"]` cobre o nome, mas o namespace não.
    for worker in ["pinar", "@other%2Fx"] {
        let (status, body, text) = write_secret(app.clone(), acme, worker, "K", SECRET_VALUE).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{worker} PUT: {text}");
        assert_eq!(body["code"], "FORBIDDEN", "{worker} PUT");
        assert!(!text.contains(SECRET_VALUE));
        let (status, body, text) = send(
            app.clone(),
            "GET",
            &format!("/api/admin/workers/{worker}/secrets"),
            Some(acme),
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{worker} GET: {text}");
        assert_eq!(body["code"], "FORBIDDEN", "{worker} GET");
        let (status, body, text) = send(
            app.clone(),
            "DELETE",
            &format!("/api/admin/workers/{worker}/secrets/K"),
            Some(acme),
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{worker} DELETE: {text}");
        assert_eq!(body["code"], "FORBIDDEN", "{worker} DELETE");
    }

    // `@acme/app`: o namespace confere — as três rotas funcionam (o worker
    // não está instalado: grava antes do install que o declara).
    let (status, _, text) = write_secret(
        app.clone(),
        acme,
        "@acme%2Fapp",
        "BYOK_MASTER_KEY",
        SECRET_VALUE,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET_VALUE));
    let (status, listed, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/@acme%2Fapp/secrets",
        Some(acme),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(listed[0]["name"], "BYOK_MASTER_KEY");
    let (status, _, text) = send(
        app,
        "DELETE",
        "/api/admin/workers/@acme%2Fapp/secrets/BYOK_MASTER_KEY",
        Some(acme),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
}

/// (F5) o `{name}` das rotas passa pela regra de nome de worker do
/// manifest (`name` ou `@scope/name`, sem caracteres de controle):
/// violação dá 400 nas três rotas. E o teto de 64 segredos por worker:
/// o 65º nome novo dá 409 `SECRETS_LIMIT`, sobrescrever um existente
/// continua funcionando.
#[tokio::test]
async fn the_worker_name_rule_and_the_per_worker_cap_guard_the_routes() {
    let (state, _dir) = vaulted_state();
    let app = build_pipeline(state);

    // Caractere de controle (CR) no nome: 400 nas três rotas.
    let (status, body, text) = send(
        app.clone(),
        "PUT",
        "/api/admin/workers/pina%0Dr/secrets/K",
        Some(ROOT_KEY),
        Body::from("{\"value\": \"v\"}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "PUT: {text}");
    assert_eq!(body["code"], "VALIDATION_ERROR");
    let (status, body, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/pina%0Dr/secrets",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "GET: {text}");
    assert_eq!(body["code"], "VALIDATION_ERROR");
    let (status, body, text) = send(
        app.clone(),
        "DELETE",
        "/api/admin/workers/pina%0Dr/secrets/K",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "DELETE: {text}");
    assert_eq!(body["code"], "VALIDATION_ERROR");

    // Barra que não é o formato `@scope/`: 400.
    let (status, body, text) = send(
        app.clone(),
        "PUT",
        "/api/admin/workers/foo%2Fbar/secrets/K",
        Some(ROOT_KEY),
        Body::from("{\"value\": \"v\"}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(body["code"], "VALIDATION_ERROR");

    // (R2) várias `/`, segmentos `.`/`..` e espaço nas bordas das partes:
    // 400 nas três rotas (`@acme/a/b` nunca é um nome de worker; `..` nunca
    // vira caminho; espaço nas bordas nunca faz parte do nome).
    for worker in [
        "@acme%2Fa%2Fb",
        "@acme%2F..%2Fvictim%2Fapp",
        "%20pinar",
        "pinar%20",
    ] {
        let (status, body, text) = send(
            app.clone(),
            "PUT",
            &format!("/api/admin/workers/{worker}/secrets/K"),
            Some(ROOT_KEY),
            Body::from("{\"value\": \"v\"}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{worker} PUT: {text}");
        assert_eq!(body["code"], "VALIDATION_ERROR", "{worker} PUT");
        let (status, body, text) = send(
            app.clone(),
            "GET",
            &format!("/api/admin/workers/{worker}/secrets"),
            Some(ROOT_KEY),
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{worker} GET: {text}");
        assert_eq!(body["code"], "VALIDATION_ERROR", "{worker} GET");
        let (status, body, text) = send(
            app.clone(),
            "DELETE",
            &format!("/api/admin/workers/{worker}/secrets/K"),
            Some(ROOT_KEY),
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{worker} DELETE: {text}");
        assert_eq!(body["code"], "VALIDATION_ERROR", "{worker} DELETE");
    }

    // Teto: 64 segredos por worker; o 65º NOME NOVO dá 409 `SECRETS_LIMIT`
    // e sobrescrever um existente continua valendo.
    for i in 0..64 {
        let (status, _, text) =
            write_secret(app.clone(), ROOT_KEY, "pinar", &format!("K{i:03}"), "v").await;
        assert_eq!(status, StatusCode::OK, "K{i:03}: {text}");
    }
    let (status, body, text) = write_secret(app.clone(), ROOT_KEY, "pinar", "K64", "v").await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(body["code"], "SECRETS_LIMIT");
    let (status, _, text) = write_secret(app.clone(), ROOT_KEY, "pinar", "K000", "v-again").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, listed, text) = send(
        app,
        "GET",
        "/api/admin/workers/pinar/secrets",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().unwrap().len(), 64, "{text}");
}

#[tokio::test]
async fn value_and_name_validation_reject_hostile_inputs() {
    let (state, _dir) = vaulted_state();
    let app = build_pipeline(state);

    // Valor vazio.
    let (status, body, text) = write_secret(app.clone(), ROOT_KEY, "pinar", "K", "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(body["code"], "VALIDATION_ERROR");

    // Valor acima de 16 KiB.
    let too_long = "x".repeat(MAX_SECRET_VALUE_BYTES + 1);
    let (status, body, text) = write_secret(app.clone(), ROOT_KEY, "pinar", "K", &too_long).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert_eq!(body["code"], "VALIDATION_ERROR");

    // Exatamente 16 KiB passa.
    let exact = "x".repeat(MAX_SECRET_VALUE_BYTES);
    let (status, _, text) = write_secret(app.clone(), ROOT_KEY, "pinar", "K", &exact).await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // Nome fora da regra do manifest (a mesma validação que S1 aplica).
    for bad in [
        "lower",
        "1ABC",
        "WITH-DASH",
        "EDGER_EGRESS_SOCKET",
        &"A".repeat(65),
    ] {
        let (status, body, text) = write_secret(app.clone(), ROOT_KEY, "pinar", bad, "v").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {text}");
        assert_eq!(body["code"], "VALIDATION_ERROR", "{bad}");
    }

    // (F3) corpo inválido — campo ausente, tipo errado ou JSON malformado:
    // 400 `VALIDATION_ERROR` com mensagem fixa que NUNCA ecoa o corpo.
    for (label, raw) in [
        ("missing field", "{}"),
        ("wrong type (number)", "{\"value\": 12345}"),
        ("malformed JSON", "not-json-at-all"),
    ] {
        let (status, parsed, text) = {
            let request = Request::builder()
                .method("PUT")
                .uri("/api/admin/workers/pinar/secrets/K")
                .header("authorization", format!("Bearer {ROOT_KEY}"))
                .header("content-type", "application/json")
                .body(Body::from(raw))
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, parsed, String::from_utf8_lossy(&bytes).into_owned())
        };
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {text}");
        assert_eq!(parsed["code"], "VALIDATION_ERROR", "{label}");
        assert_eq!(
            parsed["message"], "body must be {\"value\": \"<string>\"}",
            "the fixed message, without echoing the body ({label})"
        );
    }
    // (F3) o valor numérico nunca aparece na resposta (nem echo do corpo).
    let (_status, _, text) = {
        let request = Request::builder()
            .method("PUT")
            .uri("/api/admin/workers/pinar/secrets/K")
            .header("authorization", format!("Bearer {ROOT_KEY}"))
            .header("content-type", "application/json")
            .body(Body::from("{\"value\": 12345}"))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            Value::Null,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    };
    assert!(!text.contains("12345"), "no echo of the body: {text}");
}

/// (F3) um chamador SEM credencial recebe 401 ANTES de qualquer parse do
/// corpo: o body inválido nem é lido (o erro é de auth, não `VALIDATION_ERROR`).
#[tokio::test]
async fn an_unauthenticated_caller_gets_401_before_any_body_parse() {
    let (state, _dir) = vaulted_state();
    let app = build_pipeline(state);

    let request = Request::builder()
        .method("PUT")
        .uri("/api/admin/workers/pinar/secrets/K")
        .header("content-type", "application/json")
        .body(Body::from("{\"value\": 12345}"))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_ne!(
        parsed["code"], "VALIDATION_ERROR",
        "the body must not be parsed for an unauthenticated caller: {text}"
    );
}

#[tokio::test]
async fn anti_escalation_still_applies_to_the_new_permissions() {
    let (state, _dir) = vaulted_state();
    let app = build_pipeline(state);

    // Criador não-root SEM secrets:write não concede secrets:write.
    let (_, manager, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({ "name": "manager", "permissions": ["keys:manage", "secrets:read"] }),
    )
    .await;
    let manager = manager["rawKey"].as_str().unwrap();
    let (status, body, text) = create_key(
        app.clone(),
        manager,
        json!({ "name": "sneaky", "permissions": ["secrets:write"], "workers": ["hello"] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(body["code"], "KEY_GRANT_DENIED");

    // Criador com a permissão concede o subconjunto.
    let (_, writer, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({ "name": "writer", "permissions": ["keys:manage", "secrets:read", "secrets:write"] }),
    )
    .await;
    let writer = writer["rawKey"].as_str().unwrap();
    let (status, created, text) = create_key(
        app.clone(),
        writer,
        json!({ "name": "ok", "permissions": ["secrets:read"], "workers": ["hello"] }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(created["key"]["permissions"], json!(["secrets:read"]));
}

#[derive(Clone, Default)]
struct CapturedLogs {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8(self.buffer.lock().expect("log buffer").clone()).expect("utf8 logs")
    }
}

struct CapturedLogWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl std::io::Write for CapturedLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buffer
            .lock()
            .expect("log buffer")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CapturedLogWriter {
            buffer: self.buffer.clone(),
        }
    }
}

#[tokio::test]
async fn secret_events_carry_worker_name_and_actor_never_the_value() {
    let (state, _dir) = vaulted_state();
    let events = state.server.operational_events();
    let app = build_pipeline(state);

    let (status, _, text) = write_secret(
        app.clone(),
        ROOT_KEY,
        "pinar",
        "BYOK_MASTER_KEY",
        SECRET_VALUE,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, _, text) = send(
        app.clone(),
        "DELETE",
        "/api/admin/workers/pinar/secrets/BYOK_MASTER_KEY",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    let page = events.query(Default::default());
    let written = page
        .events
        .iter()
        .find(|event| event.kind == "secret.written")
        .expect("the write records a secret.written event");
    let deleted = page
        .events
        .iter()
        .find(|event| event.kind == "secret.deleted")
        .expect("the delete records a secret.deleted event");

    for event in [written, deleted] {
        assert_eq!(event.worker.as_deref(), Some("pinar"));
        assert!(
            event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("BYOK_MASTER_KEY")),
            "the event names the secret: {:?}",
            event.message
        );
        assert!(
            event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("api key id 0")),
            "the event carries the actor key id (root = 0): {:?}",
            event.message
        );
        let rendered = serde_json::to_string(event).unwrap();
        assert!(
            !rendered.contains(SECRET_VALUE),
            "the event must never carry the value: {rendered}"
        );
    }
}

#[tokio::test]
async fn the_secret_value_never_reaches_the_logs() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(true)
        .without_time()
        .with_writer(logs.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let (state, _dir) = vaulted_state();
    let app = build_pipeline(state);
    let (status, _, text) = write_secret(
        app.clone(),
        ROOT_KEY,
        "pinar",
        "BYOK_MASTER_KEY",
        SECRET_VALUE,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, _, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/pinar/secrets",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    drop(guard);

    let captured = logs.text();
    assert!(
        !captured.contains(SECRET_VALUE),
        "the value must never reach the logs:\n{captured}"
    );
}
