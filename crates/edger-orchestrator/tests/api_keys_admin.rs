//! Gestão de api-keys pelo Admin API: ciclo de vida, anti-escalada e escopo.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::WorkerManifest;
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, ApiKeyService, ControlAuth, ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::{json, Value};
use tower::ServiceExt;

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

fn keyed_state() -> OrchestratorState {
    let auth = ControlAuth::with_static_key(ROOT_KEY)
        .with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()));
    let mut index = ManifestIndex::new();
    insert_worker(&mut index, "hello");
    insert_worker(&mut index, "other");

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
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
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
async fn key_lifecycle_create_use_revoke_delete() {
    let state = keyed_state();
    let app = build_pipeline(state);

    // Sem credencial: nem lista.
    let (status, _, _) = send(app.clone(), "GET", "/api/admin/keys", None, Body::empty()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Root cria; rawKey aparece UMA vez, com o prefixo do preview coerente.
    let (status, created, text) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({
            "name": "studio",
            "permissions": ["workers:read", "workers:invoke"],
            "workers": ["hello"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let raw_key = created["rawKey"].as_str().expect("rawKey once");
    assert!(raw_key.starts_with("egk_"));
    assert!(raw_key.starts_with(created["key"]["keyPrefix"].as_str().unwrap()));
    let id = created["key"]["id"].as_u64().unwrap();

    // A key criada autentica de verdade no /session, como principal escopado.
    let (status, session, _) = send(
        app.clone(),
        "GET",
        "/api/admin/session",
        Some(raw_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session["principal"]["name"], "studio");
    assert_eq!(session["principal"]["isRoot"], false);
    assert_eq!(session["principal"]["workers"], json!(["hello"]));

    // Lista mascara: nunca rawKey, nunca hash.
    let (status, listed, text) = send(
        app.clone(),
        "GET",
        "/api/admin/keys",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["keys"].as_array().unwrap().len(), 1);
    assert!(!text.contains(raw_key));
    assert!(!text.to_lowercase().contains("hash"));

    // Key viva não deleta (409); revoga (terminal) e a credencial morre.
    let (status, body, _) = send(
        app.clone(),
        "DELETE",
        &format!("/api/admin/keys/{id}"),
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "KEY_NOT_REVOKED");

    let (status, _, _) = send(
        app.clone(),
        "POST",
        &format!("/api/admin/keys/{id}/revoke"),
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send(
        app.clone(),
        "GET",
        "/api/admin/session",
        Some(raw_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = send(
        app.clone(),
        "DELETE",
        &format!("/api/admin/keys/{id}"),
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Id desconhecido é 404 nos dois verbos.
    let (status, _, _) = send(
        app.clone(),
        "POST",
        "/api/admin/keys/9999/revoke",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn anti_escalation_blocks_grants_beyond_creator() {
    let state = keyed_state();
    let app = build_pipeline(state);

    let (_, manager, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({
            "name": "gerente",
            "permissions": ["keys:manage", "workers:read"],
            "workers": ["hello"],
        }),
    )
    .await;
    let manager_key = manager["rawKey"].as_str().unwrap();

    // Permission que o gerente não tem: 403 KEY_GRANT_DENIED.
    let (status, body, _) = create_key(
        app.clone(),
        manager_key,
        json!({ "name": "escalada", "permissions": ["workers:install"] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "KEY_GRANT_DENIED");

    // Worker fora do escopo do gerente: negado. O default do body é ["*"],
    // que o gerente (escopado a "hello") também não pode conceder.
    let (status, body, _) = create_key(
        app.clone(),
        manager_key,
        json!({ "name": "ampla", "permissions": ["workers:read"] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "KEY_GRANT_DENIED");

    // Subconjunto literal passa.
    let (status, _, text) = create_key(
        app.clone(),
        manager_key,
        json!({ "name": "leitura", "permissions": ["workers:read"], "workers": ["hello"] }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{text}");

    // Uma key SEM keys:manage não gerencia keys (anti-escalada do Studio).
    let (_, reader, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({ "name": "so-leitura", "permissions": ["workers:read"] }),
    )
    .await;
    let reader_key = reader["rawKey"].as_str().unwrap();
    let (status, _, _) = send(
        app.clone(),
        "GET",
        "/api/admin/keys",
        Some(reader_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn worker_scope_filters_inventory_and_gates_mutations() {
    let state = keyed_state();
    // Um erro de cada worker, para o resumo agregado ter o que esconder.
    state
        .server
        .worker_errors()
        .record("hello", "req-1", 500, "BOOM", "hello quebrou");
    state
        .server
        .worker_errors()
        .record("other", "req-2", 500, "BOOM", "segredo do vizinho");
    let app = build_pipeline(state);

    let (_, scoped, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({
            "name": "escopada",
            "permissions": ["workers:read", "workers:promote"],
            "workers": ["hello"],
        }),
    )
    .await;
    let scoped_key = scoped["rawKey"].as_str().unwrap();

    // Inventário só mostra o worker do escopo.
    let (status, listed, _) = send(
        app.clone(),
        "GET",
        "/api/admin/workers",
        Some(scoped_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = listed["workers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|worker| worker["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["hello"]);

    // enable com workers:promote no worker do escopo funciona…
    let (status, _, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/hello/enable?version=1.0.0",
        Some(scoped_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // …e o worker FORA do escopo é invisível (404, não 403: não vaza que existe).
    let (status, _, _) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/other/enable?version=1.0.0",
        Some(scoped_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A rota de erros lia pelo NOME cru: com `workers:read` e escopo em
    // `hello`, ela respondia `200` com os erros de `other`. Mensagem e stack
    // de outro worker é justamente o que o escopo existe para não entregar.
    let (status, _, _) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/hello/errors",
        Some(scoped_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // O resumo é agregado — não recebe nome no path — e por isso passava
    // inteiro: nome de cada worker com erro e a última mensagem de cada um.
    let (status, summary, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/error-summary",
        Some(scoped_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let vistos: Vec<&str> = summary["summary"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(vistos, vec!["hello"], "resumo vazou worker alheio: {text}");
    assert!(!text.contains("segredo do vizinho"), "{text}");

    // Root continua vendo os dois no mesmo resumo.
    let (_, summary, _) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/error-summary",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(summary["summary"].as_object().unwrap().len(), 2);

    let (status, _, text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/other/errors",
        Some(scoped_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
}

#[tokio::test]
async fn observability_needs_permission_not_root() {
    let state = keyed_state();
    let app = build_pipeline(state);

    let (_, observer, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({ "name": "observador", "permissions": ["observability:read"] }),
    )
    .await;
    let observer_key = observer["rawKey"].as_str().unwrap();
    let (status, _, text) = send(
        app.clone(),
        "GET",
        "/api/admin/observability/events",
        Some(observer_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    let (_, blind, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({ "name": "cego", "permissions": ["workers:read"] }),
    )
    .await;
    let blind_key = blind["rawKey"].as_str().unwrap();
    let (status, _, _) = send(
        app.clone(),
        "GET",
        "/api/admin/observability/events",
        Some(blind_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A permission de observabilidade deixou de ser root-only nesta versão. Sem
/// filtro de escopo isso vira o caminho mais fácil para ler o vizinho: o
/// evento carrega worker, namespace e mensagem, e a rota devolvia o store
/// inteiro para qualquer key com `observability:read`.
#[tokio::test]
async fn observability_events_stay_inside_the_key_scope() {
    let state = keyed_state();
    let evento = |worker: &str| edger_orchestrator::observability::OperationalEventInput {
        source: edger_orchestrator::observability::OperationalEventSource::Runtime,
        kind: "dispatch".into(),
        level: edger_orchestrator::observability::OperationalEventLevel::Info,
        namespace: None,
        worker: Some(worker.into()),
        version: Some("1.0.0".into()),
        process_id: None,
        request_id: Some(format!("req-{worker}")),
        trace_id: None,
        outcome: Some("ok".into()),
        status: Some(200),
        duration_ms: Some(1),
        code: None,
        message: None,
        truncated: None,
        dropped_count: None,
    };
    state.server.operational_events().record(evento("hello"));
    state.server.operational_events().record(evento("other"));
    let app = build_pipeline(state);

    let (_, escopada, _) = create_key(
        app.clone(),
        ROOT_KEY,
        json!({
            "name": "observador-escopado",
            "permissions": ["observability:read"],
            "workers": ["hello"],
        }),
    )
    .await;
    let escopada_key = escopada["rawKey"].as_str().unwrap();

    let (status, body, text) = send(
        app.clone(),
        "GET",
        "/api/admin/observability/events",
        Some(escopada_key),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let workers: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["worker"].as_str())
        .collect();
    assert_eq!(
        workers,
        vec!["hello"],
        "vazou evento fora do escopo: {text}"
    );

    // Root continua vendo os dois: o filtro é do escopo, não do endpoint.
    let (_, body, _) = send(
        app.clone(),
        "GET",
        "/api/admin/observability/events",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    let mut todos: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["worker"].as_str())
        .collect();
    todos.sort_unstable();
    assert_eq!(todos, vec!["hello", "other"]);
}

#[tokio::test]
async fn keys_endpoints_are_503_without_store() {
    // Instância sem store (ex.: boot falhou em abrir o SQLite): gestão
    // indisponível é um 503 explícito, não um 404 mentiroso.
    let auth = ControlAuth::with_static_key(ROOT_KEY);
    let mut index = ManifestIndex::new();
    insert_worker(&mut index, "hello");
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    let app = build_pipeline(OrchestratorState {
        server,
        pool,
        index,
        auth,
    });

    let (status, body, _) = send(
        app.clone(),
        "GET",
        "/api/admin/keys",
        Some(ROOT_KEY),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "KEYS_STORE_UNAVAILABLE");
}

#[tokio::test]
async fn browser_mutation_requires_same_origin() {
    let state = keyed_state();
    let app = build_pipeline(state);

    // Um POST vindo "de browser" (Origin) com host divergente cai no CSRF.
    let request = Request::builder()
        .method("POST")
        .uri("/api/admin/keys")
        .header("authorization", format!("Bearer {ROOT_KEY}"))
        .header("origin", "https://evil.local")
        .header("host", "edger.local")
        .body(Body::from(
            json!({ "name": "csrf", "permissions": ["workers:read"] }).to_string(),
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
