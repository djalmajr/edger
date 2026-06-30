use std::fs;
use std::process::Command;

use edger_mcp::discovery::McpContext;
use serde_json::{json, Value};
use tempfile::TempDir;

fn call(ctx: &McpContext, method: &str, params: Value) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    serde_json::from_str(&edger_mcp::handle_line(ctx, &request.to_string())).unwrap()
}

fn tool_call(ctx: &McpContext, name: &str, arguments: Value) -> Value {
    call(
        ctx,
        "tools/call",
        json!({
            "name": name,
            "arguments": arguments,
        }),
    )
}

fn content(response: &Value) -> &Value {
    &response["result"]["structuredContent"]
}

fn workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("workers")).unwrap();
    dir
}

fn write_worker(root: &TempDir, name: &str, manifest: &str) {
    let worker_dir = root.path().join("workers").join(name);
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(worker_dir.join("manifest.yaml"), manifest).unwrap();
    fs::write(worker_dir.join("index.ts"), "export default { fetch() {} }").unwrap();
}

#[test]
fn initialize_and_tools_list_expose_edger_discovery_tools() {
    let root = workspace();
    let ctx = McpContext::new(root.path()).unwrap();

    let init = call(&ctx, "initialize", json!({}));
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "edger-mcp");

    let listed = call(&ctx, "tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert!(names.contains(&"edger.list_capabilities"));
    assert!(names.contains(&"edger.list_workers"));
    assert!(names.contains(&"edger.inspect_worker"));
    assert!(names.contains(&"edger.write_worker_file"));
    assert!(names.contains(&"edger.validate_local"));
    assert!(names.contains(&"edger.prepare_commit"));
    assert!(tools
        .iter()
        .all(|tool| tool["inputSchema"]["type"].as_str() == Some("object")));
}

#[test]
fn list_capabilities_returns_versioned_contract_without_secret_terms() {
    let root = workspace();
    let ctx = McpContext::new(root.path()).unwrap();

    let response = tool_call(&ctx, "edger.list_capabilities", json!({}));
    let body = content(&response);

    assert_eq!(body["schemaVersion"], "edger.ai.v1");
    assert_eq!(body["safety"]["remoteDeploy"], false);
    assert_eq!(body["safety"]["workspaceBoundedWrites"], true);
    assert!(body["resourceTypes"]
        .as_array()
        .unwrap()
        .contains(&json!("worker")));
    assert!(body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "edger.write_worker_file"));

    let serialized = serde_json::to_string(body).unwrap().to_lowercase();
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains("token"));
}

#[test]
fn list_workers_loads_real_manifests_and_redacts_worker_env() {
    let root = workspace();
    write_worker(
        &root,
        "secure-api",
        r#"
name: secure-api
version: 1.2.3
entrypoint: index.ts
kind: fetch
visibility: protected
env:
  DATABASE_URL: postgres://hidden
  PUBLIC_VALUE: visible
"#,
    );
    let ctx = McpContext::new(root.path()).unwrap();

    let response = tool_call(&ctx, "edger.list_workers", json!({}));
    let body = content(&response);
    let workers = body["workers"].as_array().unwrap();

    assert_eq!(body["count"], 1);
    assert_eq!(workers[0]["name"], "secure-api");
    assert_eq!(workers[0]["version"], "1.2.3");
    assert_eq!(workers[0]["source"], "workers/secure-api");
    assert_eq!(workers[0]["visibility"], "protected");

    let serialized = serde_json::to_string(body).unwrap();
    assert!(!serialized.contains("postgres://hidden"));
    assert!(!serialized.contains("DATABASE_URL"));
}

#[test]
fn inspect_worker_returns_selected_worker() {
    let root = workspace();
    write_worker(
        &root,
        "todos",
        r#"
name: todos
version: 1.0.0
entrypoint: index.ts
kind: fetch
"#,
    );
    let ctx = McpContext::new(root.path()).unwrap();

    let response = tool_call(
        &ctx,
        "edger.inspect_worker",
        json!({
            "name": "todos",
            "version": "1.0.0"
        }),
    );

    assert_eq!(content(&response)["worker"]["name"], "todos");
    assert_eq!(content(&response)["worker"]["source"], "workers/todos");
}

#[test]
fn write_worker_file_defaults_to_dry_run_and_blocks_path_escape() {
    let root = workspace();
    let ctx = McpContext::new(root.path()).unwrap();

    let dry_run = tool_call(
        &ctx,
        "edger.write_worker_file",
        json!({
            "path": "workers/new-worker/index.ts",
            "content": "export default { fetch() {} }"
        }),
    );

    assert_eq!(content(&dry_run)["dryRun"], true);
    assert_eq!(content(&dry_run)["changed"], false);
    assert!(!root.path().join("workers/new-worker/index.ts").exists());

    let escape = tool_call(
        &ctx,
        "edger.write_worker_file",
        json!({
            "path": "../outside.ts",
            "content": "bad",
            "dryRun": false
        }),
    );

    assert_eq!(escape["error"]["code"], -32603);
    assert!(escape["error"]["message"]
        .as_str()
        .unwrap()
        .contains("parent traversal"));
}

#[test]
fn write_worker_file_applies_when_dry_run_is_false() {
    let root = workspace();
    let ctx = McpContext::new(root.path()).unwrap();

    let response = tool_call(
        &ctx,
        "edger.write_worker_file",
        json!({
            "path": "workers/new-worker/index.ts",
            "content": "export default { fetch() {} }",
            "dryRun": false
        }),
    );

    assert_eq!(content(&response)["changed"], true);
    assert_eq!(
        fs::read_to_string(root.path().join("workers/new-worker/index.ts")).unwrap(),
        "export default { fetch() {} }"
    );
}

#[test]
fn authored_worker_file_can_be_discovered_after_write() {
    let root = workspace();
    let ctx = McpContext::new(root.path()).unwrap();

    tool_call(
        &ctx,
        "edger.write_worker_file",
        json!({
            "path": "workers/generated/manifest.yaml",
            "content": "name: generated\nversion: 1.0.0\nentrypoint: index.ts\nkind: fetch\n",
            "dryRun": false
        }),
    );
    tool_call(
        &ctx,
        "edger.write_worker_file",
        json!({
            "path": "workers/generated/index.ts",
            "content": "export default { fetch() {} }",
            "dryRun": false
        }),
    );

    let response = tool_call(&ctx, "edger.list_workers", json!({}));
    let workers = content(&response)["workers"].as_array().unwrap();

    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0]["name"], "generated");
    assert_eq!(workers[0]["source"], "workers/generated");
}

#[test]
fn validate_local_reports_manifest_discovery_status() {
    let root = workspace();
    write_worker(
        &root,
        "valid",
        r#"
name: valid
version: 1.0.0
entrypoint: index.ts
kind: fetch
"#,
    );
    let ctx = McpContext::new(root.path()).unwrap();

    let response = tool_call(&ctx, "edger.validate_local", json!({}));
    let body = content(&response);

    assert_eq!(body["status"], "passed");
    assert_eq!(body["remoteDeploy"], false);
    assert_eq!(body["checks"][0]["id"], "worker-manifest-discovery");
    assert_eq!(body["inventory"]["count"], 1);
}

#[test]
fn validate_local_reports_manifest_errors() {
    let root = workspace();
    let worker_dir = root.path().join("workers").join("bad");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(worker_dir.join("manifest.yaml"), "name: [not valid").unwrap();
    let ctx = McpContext::new(root.path()).unwrap();

    let response = tool_call(&ctx, "edger.validate_local", json!({}));
    let body = content(&response);

    assert_eq!(body["status"], "failed");
    assert_eq!(body["remoteDeploy"], false);
    assert_eq!(body["checks"][0]["id"], "worker-manifest-discovery");
    assert!(body["checks"][0]["error"]
        .as_str()
        .unwrap()
        .contains("failed to parse"));
}

#[test]
fn prepare_commit_summarizes_local_git_changes_without_committing() {
    let root = workspace();
    Command::new("git")
        .args(["init"])
        .current_dir(root.path())
        .output()
        .unwrap();
    fs::write(root.path().join("workers/readme.md"), "changed").unwrap();
    let ctx = McpContext::new(root.path()).unwrap();

    let response = tool_call(&ctx, "edger.prepare_commit", json!({}));
    let body = content(&response);

    assert_eq!(body["remoteDeploy"], false);
    assert!(body["statusShort"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains("workers/readme.md")));
    assert_eq!(
        body["suggestedCommitMessage"],
        "feat: update edger worker control plane"
    );
    assert_eq!(
        body["suggestedPrTitle"],
        "Update edger worker control plane"
    );
    assert!(body["suggestedPrBody"]
        .as_str()
        .unwrap()
        .contains("Remote deploy"));
}
