//! `/api/mcp` — o MCP do control plane por HTTP, no padrão do Studio:
//! POST-only, JSON-RPC 2.0 stateless (cada request autentica do zero, nada de
//! sessão), batch nativo, e falha de TOOL virando resultado `isError` com
//! `_meta.status` — erro JSON-RPC fica para parse/método/tool desconhecidos.
//!
//! O dispatch é SELF-DISPATCH in-process: cada tool monta um `Request` e o
//! entrega ao `admin_api::router()` via `tower::ServiceExt::oneshot`, com a
//! credencial ORIGINAL do chamador. Permissão, CSRF, escopo por worker e
//! contratos de deploy valem idênticos aos do REST — zero lógica duplicada,
//! zero cliente HTTP interno. O CSRF de browser é checado NA PORTA com os
//! headers originais (o oneshot interno não carrega Origin).

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use edger_core::mcp::{
    capability_contract, http_tool_descriptors, initialize_result, json_rpc_error, json_rpc_ok,
    parse_json_rpc, tool_error_result, tool_result, tools_list_result, HTTP_INSTRUCTIONS,
};
use edger_core::AdminErrorResponse;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tower::ServiceExt;

use crate::admin_api;
use crate::auth::extract_api_key;
use crate::pipeline::{OrchestratorState, ADMIN_CONTROL_AUTH_HEADER, ADMIN_WORKER_VERSION_HEADER};
use crate::security::validate_admin_mutation_security;

pub async fn handle(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Autenticação e CSRF na porta: um 401/403 aqui é de TRANSPORTE (o
    // chamador nem entrou), diferente de uma tool recusada lá dentro.
    let principal = match admin_api::authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(err) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(AdminErrorResponse {
                    code: err.code,
                    message: err.message,
                }),
            )
                .into_response();
        }
    };
    if let Err(err) = validate_admin_mutation_security("POST", &headers, &principal) {
        return (
            StatusCode::FORBIDDEN,
            Json(AdminErrorResponse {
                code: err.code,
                message: err.message,
            }),
        )
            .into_response();
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(err) => {
            return Json(json_rpc_error(None, -32700, &format!("parse error: {err}")))
                .into_response();
        }
    };

    if let Value::Array(messages) = payload {
        let mut replies = Vec::new();
        for message in &messages {
            if let Some(reply) = handle_message(&state, &headers, message).await {
                replies.push(reply);
            }
        }
        if replies.is_empty() {
            // Só notificações: 202 sem corpo, como o Studio.
            return StatusCode::ACCEPTED.into_response();
        }
        return Json(Value::Array(replies)).into_response();
    }

    match handle_message(&state, &headers, &payload).await {
        Some(reply) => Json(reply).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn handle_message(
    state: &OrchestratorState,
    headers: &HeaderMap,
    message: &Value,
) -> Option<Value> {
    let request = match parse_json_rpc(message) {
        Ok(request) => request,
        Err(error_reply) => return Some(error_reply),
    };
    let id = request.id.clone();
    let is_notification = request.is_notification;
    let reply = match request.method.as_str() {
        "initialize" => json_rpc_ok(
            id,
            initialize_result("edger", env!("CARGO_PKG_VERSION"), HTTP_INSTRUCTIONS),
        ),
        "ping" => json_rpc_ok(id, json!({})),
        "tools/list" => json_rpc_ok(id, tools_list_result(&http_tool_descriptors())),
        "tools/call" => {
            let result = handle_tool_call(state, headers, request.params).await;
            json_rpc_ok(id, result)
        }
        method if method.starts_with("notifications/") => return None,
        other => json_rpc_error(id, -32601, &format!("method not found: {other}")),
    };
    if is_notification {
        return None;
    }
    Some(reply)
}

async fn handle_tool_call(
    state: &OrchestratorState,
    headers: &HeaderMap,
    params: Option<Value>,
) -> Value {
    #[derive(Deserialize)]
    struct ToolCall {
        name: String,
        #[serde(default)]
        arguments: Value,
    }
    let Some(params) = params else {
        return tool_error_result("tools/call params are required", Some(400));
    };
    let call: ToolCall = match serde_json::from_value(params) {
        Ok(call) => call,
        Err(err) => {
            return tool_error_result(&format!("invalid tools/call params: {err}"), Some(400))
        }
    };
    match dispatch_tool(state, headers, &call.name, call.arguments).await {
        Ok(result) => result,
        Err(err) => tool_error_result(&err.message, err.status.map(|status| status.as_u16())),
    }
}

struct ToolError {
    message: String,
    status: Option<StatusCode>,
}

impl ToolError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: Some(StatusCode::BAD_REQUEST),
        }
    }
}

fn args_from<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, ToolError> {
    let value = if value.is_null() { json!({}) } else { value };
    serde_json::from_value(value)
        .map_err(|err| ToolError::bad_request(format!("invalid arguments: {err}")))
}

async fn dispatch_tool(
    state: &OrchestratorState,
    headers: &HeaderMap,
    name: &str,
    arguments: Value,
) -> Result<Value, ToolError> {
    match name {
        "edger.list_capabilities" => {
            // O contrato do transporte HTTP anuncia as tools DELE.
            let mut contract = capability_contract();
            contract["tools"] = tools_list_result(&http_tool_descriptors())["tools"].clone();
            Ok(tool_result(contract))
        }
        "edger.list_deployed_workers" => {
            admin_json(
                state,
                headers,
                "GET",
                "/api/admin/workers",
                Vec::new(),
                None,
            )
            .await
        }
        "edger.install_worker" => install_worker(state, headers, arguments).await,
        "edger.enable_worker" => worker_action(state, headers, arguments, "enable").await,
        "edger.disable_worker" => worker_action(state, headers, arguments, "disable").await,
        "edger.delete_worker" => delete_worker(state, headers, arguments).await,
        "edger.promote_worker" => promote_worker(state, headers, arguments).await,
        "edger.invoke_worker" => invoke_worker(state, headers, arguments).await,
        "edger.list_observability_events" => observability_events(state, headers, arguments).await,
        "edger.list_api_keys" => {
            admin_json(state, headers, "GET", "/api/admin/keys", Vec::new(), None).await
        }
        "edger.create_api_key" => {
            // O body da tool É o body do REST — a anti-escalada mora lá.
            let body = if arguments.is_null() {
                json!({})
            } else {
                arguments
            };
            admin_json(
                state,
                headers,
                "POST",
                "/api/admin/keys",
                Vec::new(),
                Some(Body::from(body.to_string())),
            )
            .await
        }
        "edger.revoke_api_key" => {
            #[derive(Deserialize)]
            struct RevokeArgs {
                id: u64,
            }
            let args: RevokeArgs = args_from(arguments)?;
            admin_json(
                state,
                headers,
                "POST",
                &format!("/api/admin/keys/{}/revoke", args.id),
                Vec::new(),
                None,
            )
            .await
        }
        other => Err(ToolError {
            message: format!("unknown tool: {other}"),
            status: Some(StatusCode::NOT_FOUND),
        }),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpInstallArgs {
    zip_base64: Option<String>,
    zip_path: Option<String>,
    package_name: Option<String>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    staged: bool,
    expected_revision: Option<String>,
}

async fn install_worker(
    state: &OrchestratorState,
    headers: &HeaderMap,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: HttpInstallArgs = args_from(arguments)?;
    if args.zip_path.is_some() {
        return Err(ToolError::bad_request(
            "zipPath is a local-transport tool argument; over HTTP send zipBase64",
        ));
    }
    let Some(encoded) = args.zip_base64 else {
        return Err(ToolError::bad_request("zipBase64 is required"));
    };
    let bytes = BASE64
        .decode(encoded.as_bytes())
        .map_err(|_| ToolError::bad_request("zipBase64 is not valid base64"))?;
    // CAS obrigatório do draft, ANTES de qualquer efeito (mesma regra do stdio).
    if args.force && args.expected_revision.is_none() {
        return Err(ToolError::bad_request(
            "force install requires expectedRevision (the revision returned by the last install/list) — it is the compare-and-swap that stops overlapping autosaves",
        ));
    }
    let mut query = Vec::new();
    if args.force {
        query.push(("force".into(), "true".into()));
    }
    if args.staged {
        query.push(("staged".into(), "true".into()));
    }
    let mut extra = Vec::new();
    if let Some(package_name) = args.package_name {
        extra.push(("x-edger-package-name".to_string(), package_name));
    }
    if let Some(revision) = args.expected_revision {
        extra.push(("x-edger-expected-revision".to_string(), revision));
    }
    admin_call(
        state,
        headers,
        "POST",
        "/api/admin/workers/install",
        query,
        extra,
        Some(Body::from(bytes)),
    )
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerActionArgs {
    name: String,
    version: Option<String>,
}

async fn worker_action(
    state: &OrchestratorState,
    headers: &HeaderMap,
    arguments: Value,
    action: &str,
) -> Result<Value, ToolError> {
    let args: WorkerActionArgs = args_from(arguments)?;
    let mut query = Vec::new();
    if let Some(version) = args.version {
        query.push(("version".into(), version));
    }
    admin_json(
        state,
        headers,
        "POST",
        &format!("/api/admin/workers/{}/{action}", args.name),
        query,
        None,
    )
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteWorkerArgs {
    name: String,
    version: Option<String>,
    #[serde(default)]
    all_versions: bool,
}

async fn delete_worker(
    state: &OrchestratorState,
    headers: &HeaderMap,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: DeleteWorkerArgs = args_from(arguments)?;
    let version = match (args.version, args.all_versions) {
        (Some(version), false) => Some(version),
        (None, true) => None,
        (None, false) => {
            return Err(ToolError::bad_request(
                "delete requires version or explicit allVersions: true",
            ));
        }
        (Some(_), true) => {
            return Err(ToolError::bad_request(
                "version and allVersions: true are mutually exclusive",
            ));
        }
    };
    let mut query = Vec::new();
    if let Some(version) = version {
        query.push(("version".into(), version));
    }
    admin_json(
        state,
        headers,
        "DELETE",
        &format!("/api/admin/workers/{}", args.name),
        query,
        None,
    )
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromoteWorkerArgs {
    name: String,
    version: String,
}

async fn promote_worker(
    state: &OrchestratorState,
    headers: &HeaderMap,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: PromoteWorkerArgs = args_from(arguments)?;
    admin_json(
        state,
        headers,
        "POST",
        &format!("/api/admin/workers/{}/promote", args.name),
        vec![("version".into(), args.version)],
        None,
    )
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InvokeWorkerArgs {
    name: String,
    version: Option<String>,
    #[serde(default = "default_invoke_path")]
    path: String,
    #[serde(default = "default_invoke_method")]
    method: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    query: BTreeMap<String, String>,
    body: Option<String>,
    body_base64: Option<String>,
}

fn default_invoke_path() -> String {
    "/".into()
}

fn default_invoke_method() -> String {
    "GET".into()
}

async fn invoke_worker(
    state: &OrchestratorState,
    headers: &HeaderMap,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: InvokeWorkerArgs = args_from(arguments)?;
    if args.body.is_some() && args.body_base64.is_some() {
        return Err(ToolError::bad_request(
            "body and bodyBase64 are mutually exclusive",
        ));
    }
    if args.path.contains('?') || args.path.contains('#') {
        return Err(ToolError::bad_request(
            "path must not contain a query string or fragment; use query",
        ));
    }
    let mut path = format!("/api/admin/workers/{}/invoke", args.name);
    let tail = args.path.trim_matches('/');
    if !tail.is_empty() {
        path.push('/');
        path.push_str(tail);
    }
    let query: Vec<(String, String)> = args.query.into_iter().collect();

    let mut extra = Vec::new();
    for (name, value) in args.headers {
        if name.eq_ignore_ascii_case(ADMIN_WORKER_VERSION_HEADER)
            || name.eq_ignore_ascii_case(ADMIN_CONTROL_AUTH_HEADER)
        {
            return Err(ToolError::bad_request(format!(
                "{name} is reserved for control-plane routing"
            )));
        }
        extra.push((name, value));
    }
    if let Some(version) = args.version {
        extra.push((ADMIN_WORKER_VERSION_HEADER.to_string(), version));
    }

    let body = match (args.body, args.body_base64) {
        (Some(text), None) => Some(Body::from(text)),
        (None, Some(encoded)) => {
            Some(Body::from(BASE64.decode(encoded.as_bytes()).map_err(
                |_| ToolError::bad_request("bodyBase64 is not valid base64"),
            )?))
        }
        _ => None,
    };

    let response = admin_dispatch(
        state,
        headers,
        &args.method,
        &path,
        query,
        extra,
        body,
        true,
    )
    .await?;
    // Invoke devolve a resposta CRUA do worker como dado — qualquer status é
    // resultado, não erro: um 404 do app é informação para o agente.
    let status = response.status().as_u16();
    let mut header_map = BTreeMap::<String, Vec<String>>::new();
    for (name, value) in response.headers() {
        header_map
            .entry(name.as_str().to_string())
            .or_default()
            .push(value.to_str().unwrap_or("<binary>").to_string());
    }
    let bytes = body_bytes(response).await?;
    let body_text = std::str::from_utf8(&bytes).ok().map(str::to_string);
    Ok(tool_result(json!({
        "status": status,
        "headers": header_map,
        "body": body_text,
        "bodyBase64": BASE64.encode(&bytes),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObservabilityEventsArgs {
    before: Option<u64>,
    limit: Option<u64>,
    since_ms: Option<u128>,
    until_ms: Option<u128>,
    namespace: Option<String>,
    worker: Option<String>,
    version: Option<String>,
    process_id: Option<String>,
    source: Option<String>,
    kind: Option<String>,
    level: Option<String>,
    outcome: Option<String>,
    status: Option<u16>,
    request_id: Option<String>,
    trace_id: Option<String>,
    cursor: Option<u64>,
}

async fn observability_events(
    state: &OrchestratorState,
    headers: &HeaderMap,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: ObservabilityEventsArgs = args_from(arguments)?;
    let mut query = Vec::<(String, String)>::new();
    let mut push_num = |name: &str, value: Option<u128>| {
        if let Some(value) = value {
            query.push((name.into(), value.to_string()));
        }
    };
    push_num("before", args.before.map(u128::from));
    push_num("limit", args.limit.map(u128::from));
    push_num("sinceMs", args.since_ms);
    push_num("untilMs", args.until_ms);
    push_num("status", args.status.map(u128::from));
    push_num("cursor", args.cursor.map(u128::from));
    for (name, value) in [
        ("namespace", args.namespace),
        ("worker", args.worker),
        ("version", args.version),
        ("processId", args.process_id),
        ("source", args.source),
        ("kind", args.kind),
        ("level", args.level),
        ("outcome", args.outcome),
        ("requestId", args.request_id),
        ("traceId", args.trace_id),
    ] {
        if let Some(value) = value {
            query.push((name.into(), value));
        }
    }
    admin_json(
        state,
        headers,
        "GET",
        "/api/admin/observability/events",
        query,
        None,
    )
    .await
}

/// Chamada admin cuja resposta JSON vira resultado de tool; status >= 400
/// vira `isError` com a mensagem do AdminErrorResponse.
async fn admin_json(
    state: &OrchestratorState,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    query: Vec<(String, String)>,
    body: Option<Body>,
) -> Result<Value, ToolError> {
    admin_call(state, headers, method, path, query, Vec::new(), body).await
}

async fn admin_call(
    state: &OrchestratorState,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    query: Vec<(String, String)>,
    extra_headers: Vec<(String, String)>,
    body: Option<Body>,
) -> Result<Value, ToolError> {
    let response = admin_dispatch(
        state,
        headers,
        method,
        path,
        query,
        extra_headers,
        body,
        false,
    )
    .await?;
    let status = response.status();
    let bytes = body_bytes(response).await?;
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        Ok(tool_result(value))
    } else {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("admin call failed with status {status}"));
        Err(ToolError {
            message,
            status: Some(status),
        })
    }
}

/// O coração do self-dispatch: um Request de verdade contra o router do
/// Admin API, com a credencial original do chamador no slot certo.
#[allow(clippy::too_many_arguments)]
async fn admin_dispatch(
    state: &OrchestratorState,
    headers: &HeaderMap,
    method: &str,
    path: &str,
    query: Vec<(String, String)>,
    extra_headers: Vec<(String, String)>,
    body: Option<Body>,
    invoke: bool,
) -> Result<Response, ToolError> {
    let mut uri = path.to_string();
    if !query.is_empty() {
        uri.push('?');
        let encoded: Vec<String> = query
            .iter()
            .map(|(name, value)| format!("{}={}", encode_query(name), encode_query(value)))
            .collect();
        uri.push_str(&encoded.join("&"));
    }

    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(credential) = extract_api_key(headers) {
        let value = format!("Bearer {credential}");
        if invoke {
            // O slot do invoke é dedicado: o Authorization comum viaja para o
            // WORKER; a credencial do control plane vai no header próprio.
            builder = builder.header(ADMIN_CONTROL_AUTH_HEADER, &value);
        } else {
            builder = builder.header("authorization", &value);
        }
    }
    if path.ends_with("/install") {
        builder = builder.header("content-type", "application/zip");
    } else if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    for (name, value) in extra_headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ToolError::bad_request(format!("invalid header name: {name}")))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|_| ToolError::bad_request("invalid header value"))?;
        builder = builder.header(name, value);
    }

    let request = builder
        .body(body.unwrap_or_else(Body::empty))
        .map_err(|err| ToolError::bad_request(format!("invalid tool request: {err}")))?;

    let router = admin_api::router().with_state(state.clone());
    router.oneshot(request).await.map_err(|_| ToolError {
        message: "admin dispatch failed".into(),
        status: Some(StatusCode::INTERNAL_SERVER_ERROR),
    })
}

async fn body_bytes(response: Response) -> Result<Bytes, ToolError> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|err| ToolError {
            message: format!("failed to read admin response: {err}"),
            status: Some(StatusCode::INTERNAL_SERVER_ERROR),
        })
}

/// Percent-encoding mínimo para valores de query (RFC 3986 unreserved passa).
fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
