//! `/api/mcp`: transporte JSON-RPC, subset de tools e o self-dispatch com a
//! credencial do chamador atravessando permissão e escopo do Admin API.

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

fn keyed_state() -> OrchestratorState {
    let auth = ControlAuth::with_static_key(ROOT_KEY)
        .with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()));
    let mut index = ManifestIndex::new();
    for name in ["hello", "other"] {
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

async fn rpc(app: Router, api_key: Option<&str>, payload: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/mcp")
        .header("content-type", "application/json");
    if let Some(key) = api_key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let response = app
        .oneshot(request.body(Body::from(payload.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn call(id: u64, tool: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    })
}

#[tokio::test]
async fn transport_handshake_and_tool_listing() {
    let app = build_pipeline(keyed_state());

    // Sem credencial: 401 de transporte, não resposta JSON-RPC.
    let (status, body) = rpc(
        app.clone(),
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "UNAUTHORIZED");

    let (status, body) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["serverInfo"]["name"], "edger");
    assert!(body["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("list_capabilities"));

    // tools/list expõe o subset HTTP — nada de tool local de filesystem.
    let (_, body) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"edger.install_worker"));
    assert!(names.contains(&"edger.create_api_key"));
    assert!(!names.contains(&"edger.write_worker_file"));
    assert!(!names.contains(&"edger.list_workers"));

    // Método desconhecido é erro JSON-RPC; notificação sozinha é 202.
    let (_, body) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"}),
    )
    .await;
    assert_eq!(body["error"]["code"], -32601);

    let (status, _) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn tools_dispatch_through_admin_with_caller_credential() {
    let state = keyed_state();
    let app = build_pipeline(state);

    // Root cria uma key escopada via TOOL (self-dispatch no REST).
    let (_, created) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        call(
            1,
            "edger.create_api_key",
            json!({
                "name": "agente",
                "permissions": ["workers:read"],
                "workers": ["hello"],
            }),
        ),
    )
    .await;
    let result = &created["result"];
    assert_eq!(result["isError"], false);
    let raw_key = result["structuredContent"]["rawKey"].as_str().unwrap();
    assert!(raw_key.starts_with("egk_"));

    // A key criada enxerga só o worker do escopo via tool de listagem.
    let (_, listed) = rpc(
        app.clone(),
        Some(raw_key),
        call(2, "edger.list_deployed_workers", json!({})),
    )
    .await;
    let workers = &listed["result"]["structuredContent"]["workers"];
    let names: Vec<&str> = workers
        .as_array()
        .unwrap()
        .iter()
        .map(|worker| worker["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["hello"]);

    // Tool recusada vira RESULT isError com _meta.status — não erro JSON-RPC.
    let (status, denied) = rpc(
        app.clone(),
        Some(raw_key),
        call(
            3,
            "edger.promote_worker",
            json!({"name": "hello", "version": "1.0.0"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = &denied["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["_meta"]["status"], 403);
    assert!(denied.get("error").is_none());

    // Anti-escalada atravessa o transporte: a key não gerencia keys.
    let (_, escalation) = rpc(
        app.clone(),
        Some(raw_key),
        call(4, "edger.list_api_keys", json!({})),
    )
    .await;
    assert_eq!(escalation["result"]["isError"], true);
    assert_eq!(escalation["result"]["_meta"]["status"], 403);

    // Batch nativo: duas chamadas, duas respostas, na ordem.
    let (_, batch) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        json!([
            {"jsonrpc": "2.0", "id": 10, "method": "ping"},
            call(11, "edger.list_deployed_workers", json!({})),
        ]),
    )
    .await;
    let replies = batch.as_array().unwrap();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["id"], 10);
    assert_eq!(replies[1]["id"], 11);
    assert_eq!(replies[1]["result"]["isError"], false);
}

#[tokio::test]
async fn http_install_rejects_local_arguments() {
    let app = build_pipeline(keyed_state());

    let (_, body) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        call(1, "edger.install_worker", json!({"zipPath": "/tmp/x.zip"})),
    )
    .await;
    let result = &body["result"];
    assert_eq!(result["isError"], true);
    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("zipBase64"));

    // force sem expectedRevision cai na mesma regra de CAS do stdio.
    let (_, body) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        call(
            2,
            "edger.install_worker",
            json!({"zipBase64": "AAAA", "force": true}),
        ),
    )
    .await;
    assert!(body["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("expectedRevision"));
}

#[tokio::test]
async fn revoked_key_loses_the_transport() {
    let app = build_pipeline(keyed_state());

    let (_, created) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        call(
            1,
            "edger.create_api_key",
            json!({"name": "efemera", "permissions": ["workers:read"]}),
        ),
    )
    .await;
    let raw_key = created["result"]["structuredContent"]["rawKey"]
        .as_str()
        .unwrap()
        .to_string();
    let id = created["result"]["structuredContent"]["key"]["id"]
        .as_u64()
        .unwrap();

    let (status, _) = rpc(
        app.clone(),
        Some(&raw_key),
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, revoked) = rpc(
        app.clone(),
        Some(ROOT_KEY),
        call(3, "edger.revoke_api_key", json!({"id": id})),
    )
    .await;
    assert_eq!(revoked["result"]["isError"], false);

    let (status, _) = rpc(
        app.clone(),
        Some(&raw_key),
        json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
