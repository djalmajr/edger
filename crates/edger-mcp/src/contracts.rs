use serde::Serialize;
use serde_json::{json, Value};

pub const EDGER_SCHEMA_VERSION: &str = "edger.ai.v1";
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

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

pub fn tool_descriptors() -> Vec<ToolDescriptor> {
    vec![
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
        ToolDescriptor {
            name: "edger.install_worker",
            description: "Install a worker ZIP through the EdgeR admin control plane.",
            input_schema: install_worker_schema(),
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
    ]
}

pub fn capability_contract() -> Value {
    json!({
        "schemaVersion": EDGER_SCHEMA_VERSION,
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "resourceTypes": ["worker", "capability", "validation", "commit", "event", "invocation"],
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
    let mut properties = connection_properties();
    properties.extend([
        optional_string("zipPath"),
        optional_string("zipBase64"),
        optional_string("packageName"),
        optional_string("workspaceRoot"),
        (
            "force",
            json!({
                "type": "boolean",
                "default": false,
                "description": "Replace an existing internal draft version atomically. Requires expectedRevision (compare-and-swap): a stale revision is rejected with 409."
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
    let mut schema = object_schema(properties);
    schema["oneOf"] = json!([
        {"required": ["zipPath"]},
        {"required": ["zipBase64"]}
    ]);
    schema
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
