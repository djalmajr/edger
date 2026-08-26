pub mod control_plane;
pub mod discovery;

use anyhow::{anyhow, Result};
use control_plane::{
    create_api_key, delete_worker as delete_deployed_worker, disable_worker, enable_worker,
    install_worker, invoke_worker, list_api_keys, list_deployed_workers, list_observability_events,
    promote_worker, revoke_api_key, CreateApiKeyArgs, DeleteWorkerArgs, InstallWorkerArgs,
    InvokeWorkerArgs, ListDeployedWorkersArgs, ObservabilityEventsArgs, PromoteWorkerArgs,
    RevokeApiKeyArgs, WorkerActionArgs,
};
use discovery::{
    inspect_worker, list_workers, prepare_commit, validate_local, write_worker_file,
    InspectWorkerArgs, McpContext, WorkerDiscoveryArgs, WriteWorkerFileArgs,
};
// O vocabulário (descritores, schemas, framing) mora no edger-core — o
// orchestrator serve o MESMO protocolo em /api/mcp e não pode depender deste
// crate (a dependência é inversa: discovery reusa o manifest loader de lá).
use edger_core::mcp::{
    capability_contract, initialize_result, json_rpc_error, json_rpc_ok, parse_json_rpc,
    tool_descriptors, tool_error_result, tool_result,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// Instructions do stdio: o contrato completo, tools locais incluídas.
const STDIO_INSTRUCTIONS: &str = "Call edger.list_capabilities first: it returns the \
capability contract, safety limits and every tool schema. Local authoring tools \
(list_workers, write_worker_file, validate_local) act on the configured \
workspace; control-plane tools require EDGER_URL and EDGER_ROOT_KEY.";

pub fn handle_line(ctx: &McpContext, line: &str) -> String {
    let response = match serde_json::from_str::<Value>(line) {
        Ok(value) => match parse_json_rpc(&value) {
            Ok(request) => handle_request(ctx, request),
            Err(error_reply) => error_reply,
        },
        Err(err) => json_rpc_error(None, -32700, &format!("parse error: {err}")),
    };
    serde_json::to_string(&response).expect("json-rpc response serializes")
}

fn handle_request(ctx: &McpContext, request: edger_core::mcp::JsonRpcMessage) -> Value {
    let id = request.id.clone();
    match request.method.as_str() {
        "initialize" => json_rpc_ok(
            id,
            initialize_result("edger-mcp", env!("CARGO_PKG_VERSION"), STDIO_INSTRUCTIONS),
        ),
        "tools/list" => json_rpc_ok(id, edger_core::mcp::tools_list_result(&tool_descriptors())),
        "tools/call" => match handle_tool_call(ctx, request.params) {
            Ok(result) => json_rpc_ok(id, result),
            Err(err) => json_rpc_error(id, -32602, &err.to_string()),
        },
        "notifications/initialized" => json_rpc_ok(id, json!({})),
        other => json_rpc_error(id, -32601, &format!("method not found: {other}")),
    }
}

/// `Err` aqui é defeito de PROTOCOLO (params inválidos, tool desconhecida) e
/// vira erro JSON-RPC; falha da TOOL vira result `isError` — é o que deixa o
/// cliente distinguir "a tool recusou" de "a chamada nem era válida", o mesmo
/// contrato do /api/mcp do orchestrator.
fn handle_tool_call(ctx: &McpContext, params: Option<Value>) -> Result<Value> {
    #[derive(Deserialize)]
    struct ToolCall {
        name: String,
        #[serde(default)]
        arguments: Value,
    }

    let params = params.ok_or_else(|| anyhow!("tools/call params are required"))?;
    let call: ToolCall = serde_json::from_value(params)?;
    let outcome = match call.name.as_str() {
        "edger.list_capabilities" => Ok(capability_contract()),
        "edger.list_workers" => parse_args::<WorkerDiscoveryArgs>(call.arguments)
            .and_then(|args| list_workers(ctx, args)),
        "edger.inspect_worker" => parse_args::<InspectWorkerArgs>(call.arguments)
            .and_then(|args| inspect_worker(ctx, args)),
        "edger.write_worker_file" => parse_args::<WriteWorkerFileArgs>(call.arguments)
            .and_then(|args| write_worker_file(ctx, args)),
        "edger.validate_local" => {
            parse_args::<WorkerDiscoveryArgs>(call.arguments).map(|args| validate_local(ctx, args))
        }
        "edger.prepare_commit" => {
            let workspace_root = call
                .arguments
                .get("workspaceRoot")
                .and_then(Value::as_str)
                .map(str::to_string);
            prepare_commit(ctx, workspace_root)
        }
        "edger.install_worker" => parse_args::<InstallWorkerArgs>(call.arguments)
            .and_then(|args| install_worker(ctx, args)),
        "edger.list_deployed_workers" => parse_args::<ListDeployedWorkersArgs>(call.arguments)
            .and_then(|args| list_deployed_workers(ctx, args)),
        "edger.enable_worker" => {
            parse_args::<WorkerActionArgs>(call.arguments).and_then(|args| enable_worker(ctx, args))
        }
        "edger.disable_worker" => parse_args::<WorkerActionArgs>(call.arguments)
            .and_then(|args| disable_worker(ctx, args)),
        "edger.delete_worker" => parse_args::<DeleteWorkerArgs>(call.arguments)
            .and_then(|args| delete_deployed_worker(ctx, args)),
        "edger.promote_worker" => parse_args::<PromoteWorkerArgs>(call.arguments)
            .and_then(|args| promote_worker(ctx, args)),
        "edger.invoke_worker" => {
            parse_args::<InvokeWorkerArgs>(call.arguments).and_then(|args| invoke_worker(ctx, args))
        }
        "edger.list_observability_events" => parse_args::<ObservabilityEventsArgs>(call.arguments)
            .and_then(|args| list_observability_events(ctx, args)),
        "edger.list_api_keys" => list_api_keys(ctx),
        "edger.create_api_key" => parse_args::<CreateApiKeyArgs>(call.arguments)
            .and_then(|args| create_api_key(ctx, args)),
        "edger.revoke_api_key" => parse_args::<RevokeApiKeyArgs>(call.arguments)
            .and_then(|args| revoke_api_key(ctx, args)),
        other => return Err(anyhow!("unknown tool: {other}")),
    };
    Ok(match outcome {
        Ok(value) => tool_result(value),
        Err(err) => tool_error_result(&err.to_string(), None),
    })
}

fn parse_args<T>(value: Value) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let value = if value.is_null() { json!({}) } else { value };
    serde_json::from_value(value).map_err(Into::into)
}
