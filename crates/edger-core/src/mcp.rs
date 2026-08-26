//! MCP vocabulary shared by both transports.
//!
//! Movido do `edger-mcp` (contracts + framing) para cá porque a dependência
//! entre os crates é INVERSA ao que o endpoint HTTP precisa: `edger-mcp`
//! depende do orchestrator (discovery local reusa o manifest loader), então o
//! orchestrator jamais poderia importar os descritores de lá. Descritores,
//! schemas e o framing JSON-RPC são dados puros — moram no vocabulário.
//!
//! Dois transportes, um contrato: o stdio (`edger-mcp`) expõe TODAS as tools,
//! inclusive as locais de authoring (filesystem do workspace de quem roda); o
//! HTTP (`/api/mcp` no orchestrator) expõe só o subconjunto de control plane —
//! no servidor, as tools locais leriam o filesystem do RUNTIME, um trust
//! domain que não pertence ao chamador.

use serde::Serialize;
use serde_json::{json, Value};

pub const EDGER_SCHEMA_VERSION: &str = "edger.ai.v1";
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Instructions do `initialize` HTTP: curtas e apontando para a tool de
/// contrato, no lugar de despejar o catálogo inteiro no handshake.
pub const HTTP_INSTRUCTIONS: &str = "Call edger.list_capabilities first: it returns the \
capability contract, safety limits and every tool schema. Deploy flow: \
edger.install_worker (zipBase64; staged=true for a public version that only \
serves version-pinned traffic) then edger.promote_worker to make it the \
default. edger.invoke_worker exercises a deployed worker through the \
authenticated control plane.";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityDescriptor {
    pub id: &'static str,
    pub status: &'static str,
    pub owner: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

/// Tools do transporte stdio: authoring local + control plane + keys.
pub fn tool_descriptors() -> Vec<ToolDescriptor> {
    let mut tools = vec![
        ToolDescriptor {
            name: "edger.list_capabilities",
            description: "List AI-native edger capability contracts and safety limits.",
            input_schema: object_schema(vec![]),
        },
        ToolDescriptor {
            name: "edger.list_workers",
            description: "Load worker manifests from local worker dirs and return safe inventory.",
            input_schema: worker_dirs_schema(),
        },
        ToolDescriptor {
            name: "edger.inspect_worker",
            description: "Inspect one local worker by name and optional version.",
            input_schema: inspect_worker_schema(),
        },
        ToolDescriptor {
            name: "edger.write_worker_file",
            description:
                "Create or replace a worker file inside the workspace, dry-run by default.",
            input_schema: write_worker_file_schema(),
        },
        ToolDescriptor {
            name: "edger.validate_local",
            description: "Run local in-process edger validation for worker manifests.",
            input_schema: worker_dirs_schema(),
        },
        ToolDescriptor {
            name: "edger.prepare_commit",
            description: "Prepare a local git change summary and suggested commit metadata.",
            input_schema: object_schema(vec![optional_string("workspaceRoot")]),
        },
    ];
    tools.extend(control_plane_tool_descriptors(install_worker_schema()));
    tools.extend(api_key_tool_descriptors());
    tools
}

/// Tools do transporte HTTP: só control plane + keys. Sem as locais — e o
/// install aceita apenas `zipBase64` (não existe "path do meu disco" quando o
/// disco é o do servidor).
pub fn http_tool_descriptors() -> Vec<ToolDescriptor> {
    let mut tools = vec![ToolDescriptor {
        name: "edger.list_capabilities",
        description: "List AI-native edger capability contracts and safety limits.",
        input_schema: object_schema(vec![]),
    }];
    tools.extend(control_plane_tool_descriptors(install_worker_http_schema()));
    tools.extend(api_key_tool_descriptors());
    tools
}

fn control_plane_tool_descriptors(install_schema: Value) -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            name: "edger.install_worker",
            description: "Install a worker ZIP through the EdgeR admin control plane.",
            input_schema: install_schema,
        },
        ToolDescriptor {
            name: "edger.list_deployed_workers",
            description: "List workers currently indexed by the EdgeR admin control plane.",
            input_schema: control_connection_schema(),
        },
        ToolDescriptor {
            name: "edger.enable_worker",
            description: "Enable one deployed worker version through the control plane.",
            input_schema: worker_action_schema(),
        },
        ToolDescriptor {
            name: "edger.disable_worker",
            description: "Disable one deployed worker version through the control plane.",
            input_schema: worker_action_schema(),
        },
        ToolDescriptor {
            name: "edger.delete_worker",
            description: "Delete one worker version or every version plus its runtime processes.",
            input_schema: delete_worker_schema(),
        },
        ToolDescriptor {
            name: "edger.promote_worker",
            description: "Select an immutable public worker version as the durable default.",
            input_schema: promote_worker_schema(),
        },
        ToolDescriptor {
            name: "edger.invoke_worker",
            description: "Invoke a worker through the authenticated control plane.",
            input_schema: invoke_worker_schema(),
        },
        ToolDescriptor {
            name: "edger.list_observability_events",
            description: "Query operational events, optionally filtered by worker.",
            input_schema: observability_events_schema(),
        },
    ]
}

fn api_key_tool_descriptors() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            name: "edger.list_api_keys",
            description: "List control-plane API keys (previews only; requires keys:manage).",
            input_schema: object_schema(vec![]),
        },
        ToolDescriptor {
            name: "edger.create_api_key",
            description: "Create a scoped API key. The raw key is returned ONCE. A non-root \
                caller can only grant a subset of its own permissions/namespaces/workers.",
            input_schema: create_api_key_schema(),
        },
        ToolDescriptor {
            name: "edger.revoke_api_key",
            description: "Revoke an API key by id. Revocation is terminal.",
            input_schema: revoke_api_key_schema(),
        },
    ]
}

pub fn capability_descriptors() -> Vec<CapabilityDescriptor> {
    vec![
        CapabilityDescriptor {
            id: "workers.discovery",
            status: "functional",
            owner: "edger-mcp",
            description: "Discovers workers from local manifests using edger manifest loading.",
        },
        CapabilityDescriptor {
            id: "workers.authoring",
            status: "functional-local",
            owner: "edger-mcp",
            description: "Creates or replaces worker files inside the authorized workspace.",
        },
        CapabilityDescriptor {
            id: "workers.validation",
            status: "functional-local",
            owner: "edger-mcp",
            description: "Validates local worker manifests without remote deploy.",
        },
        CapabilityDescriptor {
            id: "git.commit-prep",
            status: "functional-local",
            owner: "edger-mcp",
            description: "Summarizes local git changes and prepares commit metadata.",
        },
        CapabilityDescriptor {
            id: "workers.control-plane",
            status: "functional",
            owner: "edger-mcp",
            description:
                "Installs, lists, invokes, promotes, toggles and deletes deployed workers.",
        },
        CapabilityDescriptor {
            id: "observability.events",
            status: "functional",
            owner: "edger-mcp",
            description: "Queries control-plane operational events with worker filters.",
        },
        CapabilityDescriptor {
            id: "keys.management",
            status: "functional",
            owner: "edger-orchestrator",
            description: "Lists, creates and revokes scoped control-plane API keys.",
        },
    ]
}

pub fn capability_contract() -> Value {
    json!({
        "schemaVersion": EDGER_SCHEMA_VERSION,
        "protocolVersion": MCP_PROTOCOL_VERSION,
        // "key", não "apiKey": o teste de contrato veta o literal "apikey" no
        // JSON inteiro como tripwire contra vazamento de credencial.
        "resourceTypes": ["worker", "capability", "validation", "commit", "event", "invocation", "key"],
        "safety": {
            "remoteDeploy": true,
            "dryRunDefault": true,
            "workspaceBoundedWrites": true,
            "arbitraryShell": false
        },
        "capabilities": capability_descriptors(),
        "tools": tool_descriptors()
            .into_iter()
            .map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            }))
            .collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// Framing JSON-RPC — helpers puros compartilhados pelos dois transportes.
// Sem trait de dispatcher de propósito: o stdio é sync e o HTTP é async, e um
// trait forçaria async-trait num dos lados; cada transporte escreve o próprio
// `match method` (~50 linhas) sobre estes helpers.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct JsonRpcMessage {
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
    pub is_notification: bool,
}

/// Interpreta UMA mensagem JSON-RPC. `Err` já é a resposta de erro pronta.
pub fn parse_json_rpc(value: &Value) -> Result<JsonRpcMessage, Value> {
    let obj = value
        .as_object()
        .ok_or_else(|| json_rpc_error(None, -32600, "invalid request: not an object"))?;
    let method = obj
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            json_rpc_error(obj.get("id").cloned(), -32600, "invalid request: no method")
        })?
        .to_string();
    let id = obj.get("id").cloned().filter(|id| !id.is_null());
    Ok(JsonRpcMessage {
        is_notification: id.is_none(),
        id,
        method,
        params: obj.get("params").cloned(),
    })
}

pub fn json_rpc_ok(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn json_rpc_error(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

pub fn initialize_result(server_name: &str, version: &str, instructions: &str) -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": server_name, "version": version },
        "instructions": instructions,
    })
}

pub fn tools_list_result(tools: &[ToolDescriptor]) -> Value {
    json!({
        "tools": tools
            .iter()
            .map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            }))
            .collect::<Vec<_>>(),
    })
}

/// Resultado de tool bem-sucedida: `content` textual + `structuredContent`.
pub fn tool_result(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).expect("structured content serializes");
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": false,
    })
}

/// Falha de TOOL vira resultado com `isError`, nunca erro JSON-RPC: é o que
/// permite ao cliente distinguir "a tool recusou" (com `_meta.status` HTTP)
/// de "o transporte quebrou". Erro JSON-RPC fica para parse/método/tool
/// desconhecidos.
pub fn tool_error_result(message: &str, status: Option<u16>) -> Value {
    let mut result = json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    });
    if let Some(status) = status {
        result["_meta"] = json!({ "status": status });
    }
    result
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

fn worker_dirs_schema() -> Value {
    object_schema(vec![
        optional_string("workspaceRoot"),
        (
            "workerDirs",
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Worker directories relative to workspaceRoot; defaults to workers."
            }),
        ),
    ])
}

fn inspect_worker_schema() -> Value {
    object_schema(vec![
        required_string("name"),
        optional_string("version"),
        optional_string("workspaceRoot"),
        (
            "workerDirs",
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Worker directories relative to workspaceRoot; defaults to workers."
            }),
        ),
    ])
}

fn write_worker_file_schema() -> Value {
    object_schema(vec![
        required_string("path"),
        required_string("content"),
        optional_string("workspaceRoot"),
        (
            "dryRun",
            json!({
                "type": "boolean",
                "default": true,
                "description": "When omitted, no file is written."
            }),
        ),
        (
            "overwrite",
            json!({
                "type": "boolean",
                "default": false,
                "description": "Allow replacing an existing file when dryRun is false."
            }),
        ),
    ])
}

fn control_connection_schema() -> Value {
    object_schema(connection_properties())
}

fn install_worker_schema() -> Value {
    let mut schema = object_schema(install_worker_properties(true));
    schema["oneOf"] = json!([
        {"required": ["zipPath"]},
        {"required": ["zipBase64"]}
    ]);
    schema
}

/// Variante HTTP: sem `zipPath`/`workspaceRoot` — o filesystem visível seria o
/// do servidor, não o do chamador.
fn install_worker_http_schema() -> Value {
    let mut schema = object_schema(install_worker_properties(false));
    schema["required"] = json!(["zipBase64"]);
    schema
}

fn install_worker_properties(local: bool) -> Vec<(&'static str, Value)> {
    let mut properties = connection_properties();
    if local {
        properties.push(optional_string("zipPath"));
    }
    properties.push(optional_string("zipBase64"));
    properties.push(optional_string("packageName"));
    if local {
        properties.push(optional_string("workspaceRoot"));
    }
    properties.extend([
        (
            "force",
            json!({
                "type": "boolean",
                "default": false,
                "description": "Replace an existing internal draft version atomically. Requires expectedRevision (compare-and-swap): a stale revision is rejected with 409."
            }),
        ),
        (
            "staged",
            json!({
                "type": "boolean",
                "default": false,
                "description": "Install an immutable public version for version-pinned traffic and health checks without changing the unversioned route until promote."
            }),
        ),
        (
            "expectedRevision",
            json!({
                "type": "string",
                "description": "Revision the caller last saw for this draft (returned by install/list). Mandatory with force: prevents two overlapping autosaves from publishing older code last."
            }),
        ),
    ]);
    properties
}

fn worker_action_schema() -> Value {
    let mut properties = connection_properties();
    properties.extend([required_string("name"), optional_string("version")]);
    object_schema(properties)
}

fn delete_worker_schema() -> Value {
    let mut properties = connection_properties();
    properties.extend([
        required_string("name"),
        optional_string("version"),
        (
            "allVersions",
            json!({
                "type": "boolean",
                "default": false
            }),
        ),
    ]);
    let mut schema = object_schema(properties);
    schema["oneOf"] = json!([
        {
            "required": ["version"],
            "properties": {"allVersions": {"const": false}}
        },
        {
            "required": ["allVersions"],
            "properties": {"allVersions": {"const": true}}
        }
    ]);
    schema
}

fn promote_worker_schema() -> Value {
    let mut properties = connection_properties();
    properties.extend([required_string("name"), required_string("version")]);
    object_schema(properties)
}

fn invoke_worker_schema() -> Value {
    let mut properties = connection_properties();
    properties.extend([
        required_string("name"),
        optional_string("version"),
        optional_string("path"),
        optional_string("method"),
        (
            "headers",
            json!({
                "type": "object",
                "additionalProperties": {"type": "string"}
            }),
        ),
        (
            "query",
            json!({
                "type": "object",
                "additionalProperties": {"type": "string"}
            }),
        ),
        optional_string("body"),
        optional_string("bodyBase64"),
    ]);
    object_schema(properties)
}

fn observability_events_schema() -> Value {
    let mut properties = connection_properties();
    for name in ["before", "limit", "sinceMs", "untilMs", "status", "cursor"] {
        properties.push((
            name,
            json!({
                "type": "integer",
                "minimum": 0
            }),
        ));
    }
    for name in [
        "namespace",
        "worker",
        "version",
        "processId",
        "source",
        "kind",
        "level",
        "outcome",
        "requestId",
        "traceId",
    ] {
        properties.push(optional_string(name));
    }
    object_schema(properties)
}

fn create_api_key_schema() -> Value {
    object_schema(vec![
        required_string("name"),
        (
            "permissions",
            json!({
                "type": "array",
                "items": {"type": "string"},
                "minItems": 1,
                "x-required": true,
                "description": "Subset of the permission catalog (workers:read|install|delete|promote|invoke, observability:read, keys:manage). \"*\" is not storable."
            }),
        ),
        (
            "namespaces",
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Tenant namespaces this key may touch; defaults to [\"*\"] when the creator can grant it."
            }),
        ),
        (
            "workers",
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Worker names this key may touch: exact name or suffix glob like \"p-abc*\"; defaults to [\"*\"] when the creator can grant it."
            }),
        ),
        (
            "expiresAt",
            json!({
                "type": "integer",
                "minimum": 0,
                "description": "Unix epoch seconds; omitted = never expires."
            }),
        ),
    ])
}

fn revoke_api_key_schema() -> Value {
    object_schema(vec![(
        "id",
        json!({
            "type": "integer",
            "minimum": 1,
            "x-required": true
        }),
    )])
}

fn connection_properties() -> Vec<(&'static str, Value)> {
    Vec::new()
}

fn object_schema(properties: Vec<(&'static str, Value)>) -> Value {
    let required = properties
        .iter()
        .filter_map(|(name, schema)| {
            schema
                .get("x-required")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                .then_some(*name)
        })
        .collect::<Vec<_>>();
    let properties = properties
        .into_iter()
        .map(|(name, mut schema)| {
            if let Some(obj) = schema.as_object_mut() {
                obj.remove("x-required");
            }
            (name.to_string(), schema)
        })
        .collect::<serde_json::Map<_, _>>();

    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

fn required_string(name: &'static str) -> (&'static str, Value) {
    (
        name,
        json!({
            "type": "string",
            "minLength": 1,
            "x-required": true,
        }),
    )
}

fn optional_string(name: &'static str) -> (&'static str, Value) {
    (
        name,
        json!({
            "type": "string",
            "minLength": 1,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_subset_has_no_local_tools() {
        let http: Vec<&str> = http_tool_descriptors().iter().map(|t| t.name).collect();
        for local in [
            "edger.list_workers",
            "edger.inspect_worker",
            "edger.write_worker_file",
            "edger.validate_local",
            "edger.prepare_commit",
        ] {
            assert!(
                !http.contains(&local),
                "{local} must not be exposed remotely"
            );
        }
        for remote in [
            "edger.list_capabilities",
            "edger.install_worker",
            "edger.promote_worker",
            "edger.invoke_worker",
            "edger.create_api_key",
        ] {
            assert!(http.contains(&remote), "{remote} missing from HTTP subset");
        }
    }

    #[test]
    fn http_install_only_accepts_zip_base64() {
        let http = http_tool_descriptors();
        let install = http
            .iter()
            .find(|t| t.name == "edger.install_worker")
            .expect("install tool");
        let props = install.input_schema["properties"].as_object().unwrap();
        assert!(!props.contains_key("zipPath"));
        assert!(!props.contains_key("workspaceRoot"));
        assert_eq!(install.input_schema["required"], json!(["zipBase64"]));
    }

    #[test]
    fn stdio_descriptors_include_authoring_and_keys() {
        let names: Vec<&str> = tool_descriptors().iter().map(|t| t.name).collect();
        for name in [
            "edger.write_worker_file",
            "edger.list_api_keys",
            "edger.create_api_key",
            "edger.revoke_api_key",
        ] {
            assert!(names.contains(&name), "{name} missing from stdio set");
        }
    }

    #[test]
    fn json_rpc_parse_distinguishes_notifications() {
        let call = parse_json_rpc(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).unwrap();
        assert!(!call.is_notification);
        let note =
            parse_json_rpc(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .unwrap();
        assert!(note.is_notification);
        let err = parse_json_rpc(&json!("nope")).unwrap_err();
        assert_eq!(err["error"]["code"], -32600);
    }

    #[test]
    fn tool_error_result_carries_status_meta() {
        let result = tool_error_result("denied", Some(403));
        assert_eq!(result["isError"], true);
        assert_eq!(result["_meta"]["status"], 403);
        let no_status = tool_error_result("boom", None);
        assert!(no_status.get("_meta").is_none());
    }
}
