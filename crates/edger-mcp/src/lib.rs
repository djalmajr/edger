pub mod contracts;
pub mod discovery;

use anyhow::{anyhow, Result};
use discovery::{
    inspect_worker, list_workers, prepare_commit, validate_local, write_worker_file,
    InspectWorkerArgs, McpContext, WorkerDiscoveryArgs, WriteWorkerFileArgs,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::contracts::{capability_contract, tool_descriptors, MCP_PROTOCOL_VERSION};

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

pub fn handle_line(ctx: &McpContext, line: &str) -> String {
    let response = match serde_json::from_str::<JsonRpcRequest>(line) {
        Ok(request) => handle_request(ctx, request),
        Err(err) => json_rpc_error(None, -32700, format!("parse error: {err}")),
    };
    serde_json::to_string(&response).expect("json-rpc response serializes")
}

fn handle_request(ctx: &McpContext, request: JsonRpcRequest) -> Value {
    let id = request.id.clone();
    match handle_method(ctx, &request.method, request.params) {
        Ok(result) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
        Err(err) => json_rpc_error(id, -32603, err.to_string()),
    }
}

fn handle_method(ctx: &McpContext, method: &str, params: Option<Value>) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {
                "name": "edger-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })),
        "tools/list" => Ok(json!({
            "tools": tool_descriptors()
                .into_iter()
                .map(|tool| json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                }))
                .collect::<Vec<_>>(),
        })),
        "tools/call" => handle_tool_call(ctx, params),
        "notifications/initialized" => Ok(json!({})),
        other => Err(anyhow!("method not found: {other}")),
    }
}

fn handle_tool_call(ctx: &McpContext, params: Option<Value>) -> Result<Value> {
    #[derive(Deserialize)]
    struct ToolCall {
        name: String,
        #[serde(default)]
        arguments: Value,
    }

    let params = params.ok_or_else(|| anyhow!("tools/call params are required"))?;
    let call: ToolCall = serde_json::from_value(params)?;
    let result = match call.name.as_str() {
        "edger.list_capabilities" => capability_contract(),
        "edger.list_workers" => {
            let args = parse_args::<WorkerDiscoveryArgs>(call.arguments)?;
            list_workers(ctx, args)?
        }
        "edger.inspect_worker" => {
            let args = parse_args::<InspectWorkerArgs>(call.arguments)?;
            inspect_worker(ctx, args)?
        }
        "edger.write_worker_file" => {
            let args = parse_args::<WriteWorkerFileArgs>(call.arguments)?;
            write_worker_file(ctx, args)?
        }
        "edger.validate_local" => {
            let args = parse_args::<WorkerDiscoveryArgs>(call.arguments)?;
            validate_local(ctx, args)
        }
        "edger.prepare_commit" => {
            let workspace_root = call
                .arguments
                .get("workspaceRoot")
                .and_then(Value::as_str)
                .map(str::to_string);
            prepare_commit(ctx, workspace_root)?
        }
        other => return Err(anyhow!("unknown tool: {other}")),
    };
    Ok(tool_result(result))
}

fn parse_args<T>(value: Value) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let value = if value.is_null() { json!({}) } else { value };
    serde_json::from_value(value).map_err(Into::into)
}

fn tool_result(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).expect("structured content serializes");
    json!({
        "content": [
            {
                "type": "text",
                "text": text,
            }
        ],
        "structuredContent": value,
        "isError": false,
    })
}

fn json_rpc_error(id: Option<Value>, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}
