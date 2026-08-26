use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

fn mock_admin(expected: usize) -> (String, mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let started = Instant::now();
        let mut served = 0;
        while served < expected && started.elapsed() < Duration::from_secs(10) {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("mock admin accept failed: {error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    panic!("mock admin request ended before headers");
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(position) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    break position + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length: ")
                        .or_else(|| line.strip_prefix("Content-Length: "))
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while request.len() < header_end + content_length {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let request_text = String::from_utf8_lossy(&request).into_owned();
            let request_line = request_text.lines().next().unwrap_or_default();
            let (status, content_type, body) = if request_line.contains("/invoke") {
                ("207 Multi-Status", "text/plain", "invoked")
            } else if request_line.starts_with("GET /api/admin/workers ") {
                (
                    "200 OK",
                    "application/json",
                    r#"{"workers":[{"name":"release","visibility":"public"},{"name":"draft","visibility":"internal"}]}"#,
                )
            } else {
                ("200 OK", "application/json", r#"{"ok":true}"#)
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            sender.send(request_text).unwrap();
            served += 1;
        }
        assert_eq!(served, expected, "not all mock admin requests arrived");
    });
    (format!("http://{address}"), receiver, handle)
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
    for name in [
        "edger.install_worker",
        "edger.list_deployed_workers",
        "edger.enable_worker",
        "edger.disable_worker",
        "edger.delete_worker",
        "edger.promote_worker",
        "edger.invoke_worker",
        "edger.list_observability_events",
    ] {
        assert!(names.contains(&name), "missing control-plane tool {name}");
    }
    assert!(tools
        .iter()
        .all(|tool| tool["inputSchema"]["type"].as_str() == Some("object")));
    let install = tools
        .iter()
        .find(|tool| tool["name"] == "edger.install_worker")
        .unwrap();
    assert_eq!(
        install["inputSchema"]["properties"]["staged"]["default"],
        false
    );
}

#[test]
fn list_capabilities_returns_versioned_contract_without_secret_terms() {
    let root = workspace();
    let ctx = McpContext::new(root.path()).unwrap();

    let response = tool_call(&ctx, "edger.list_capabilities", json!({}));
    let body = content(&response);

    assert_eq!(body["schemaVersion"], "edger.ai.v1");
    assert_eq!(body["safety"]["remoteDeploy"], true);
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
    assert!(!serialized.contains("baseurl"));
    assert!(!serialized.contains("apikey"));
    assert!(!serialized.contains("rootkey"));
}

#[test]
fn control_plane_target_is_bound_and_requires_https_off_loopback() {
    let root = workspace();
    let error = McpContext::with_control_plane(root.path(), "http://example.com", "must-not-leak")
        .unwrap_err();
    assert!(error.to_string().contains("must use https"));
    let context =
        McpContext::with_control_plane(root.path(), "https://example.com", "must-not-leak")
            .unwrap();
    let debug = format!("{context:?}");
    assert!(!debug.contains("must-not-leak"));
    assert!(!debug.contains("example.com"));
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

    // Falha de TOOL é resultado isError (contrato unificado com /api/mcp),
    // não erro JSON-RPC — erro de protocolo fica para parse/método/tool.
    assert_eq!(escape["result"]["isError"], true);
    assert!(escape["result"]["content"][0]["text"]
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

#[test]
fn control_plane_tools_emit_admin_http_contracts() {
    let root = workspace();
    fs::write(root.path().join("package.zip"), b"zip-path").unwrap();
    let (base_url, requests, server) = mock_admin(12);
    let ctx = McpContext::with_control_plane(root.path(), base_url, "control-key").unwrap();
    let invalid_delete = tool_call(&ctx, "edger.delete_worker", json!({"name": "demo"}));
    assert_eq!(invalid_delete["result"]["isError"], true);
    assert!(invalid_delete["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("version or explicit allVersions"));
    // Force sem expectedRevision não é CAS: falha ANTES de abrir HTTP (o mock
    // conta requests; nenhuma pode ser consumida por esta chamada inválida).
    let invalid_force = tool_call(
        &ctx,
        "edger.install_worker",
        json!({"zipPath": "package.zip", "force": true}),
    );
    assert_eq!(invalid_force["result"]["isError"], true);
    assert!(invalid_force["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("expectedRevision"));
    let call_success = |name: &str, arguments: Value| {
        let response = tool_call(&ctx, name, arguments);
        assert!(response.get("error").is_none(), "{name} failed: {response}");
        assert_eq!(
            response["result"]["isError"], false,
            "{name} refused: {response}"
        );
        response
    };

    call_success(
        "edger.install_worker",
        json!({
            "zipPath": "package.zip",
            "packageName": "draft.zip",
            "force": true,
            "expectedRevision": "rev-a",
        }),
    );
    call_success(
        "edger.install_worker",
        json!({"zipBase64": "emlwLWJhc2U2NA==", "staged": true}),
    );
    let listed = call_success("edger.list_deployed_workers", json!({}));
    assert_eq!(content(&listed)["workers"][0]["visibility"], "public");
    assert_eq!(content(&listed)["workers"][1]["visibility"], "internal");
    call_success(
        "edger.enable_worker",
        json!({"name": "demo", "version": "1.0.0"}),
    );
    call_success(
        "edger.disable_worker",
        json!({"name": "demo", "version": "1.0.0"}),
    );
    call_success(
        "edger.delete_worker",
        json!({"name": "demo", "version": "1.0.0"}),
    );
    call_success(
        "edger.promote_worker",
        json!({"name": "demo", "version": "1.0.0"}),
    );
    let invoked = call_success(
        "edger.invoke_worker",
        json!({
            "name": "demo",
            "version": "0.0.0",
            "path": "/probe",
            "method": "POST",
            "headers": {
                "authorization": "Bearer app-token",
                "x-api-key": "app-key",
                "x-client": "studio"
            },
            "query": {"q": "7", "version": "abc"},
            "body": "payload",
        }),
    );
    assert_eq!(content(&invoked)["status"], 207);
    assert_eq!(content(&invoked)["body"], "invoked");
    call_success(
        "edger.list_observability_events",
        json!({"worker": "demo", "limit": 5}),
    );
    call_success("edger.list_api_keys", json!({}));
    call_success(
        "edger.create_api_key",
        json!({"name": "ci", "permissions": ["workers:read"], "workers": ["demo"]}),
    );
    call_success("edger.revoke_api_key", json!({"id": 7}));

    let requests = (0..12)
        .map(|_| requests.recv_timeout(Duration::from_secs(2)).unwrap())
        .collect::<Vec<_>>();
    server.join().unwrap();
    assert!(requests.iter().enumerate().all(|(index, request)| {
        index == 7
            || request
                .to_ascii_lowercase()
                .contains("authorization: bearer control-key")
    }));
    assert!(requests[0].starts_with("POST /api/admin/workers/install?force=true "));
    assert!(requests[0].contains("x-edger-package-name: draft.zip"));
    assert!(requests[0].ends_with("zip-path"));
    assert!(requests[1].starts_with("POST /api/admin/workers/install?staged=true "));
    assert!(requests[1].ends_with("zip-base64"));
    assert!(requests[2].starts_with("GET /api/admin/workers "));
    assert!(requests[3].starts_with("POST /api/admin/workers/demo/enable?version=1.0.0 "));
    assert!(requests[4].starts_with("POST /api/admin/workers/demo/disable?version=1.0.0 "));
    assert!(requests[5].starts_with("DELETE /api/admin/workers/demo?version=1.0.0 "));
    assert!(requests[6].starts_with("POST /api/admin/workers/demo/promote?version=1.0.0 "));
    assert!(requests[7].starts_with("POST /api/admin/workers/demo/invoke/probe?q=7&version=abc "));
    assert!(requests[7].contains("x-client: studio"));
    assert!(requests[7].contains("x-edger-worker-version: 0.0.0"));
    let invoke_request = requests[7].to_ascii_lowercase();
    assert!(invoke_request.contains("x-edger-control-authorization: bearer control-key"));
    assert!(invoke_request.contains("authorization: bearer app-token"));
    assert!(invoke_request.contains("x-api-key: app-key"));
    assert!(!invoke_request
        .lines()
        .any(|line| line == "authorization: bearer control-key"));
    assert!(requests[7].ends_with("payload"));
    assert!(requests[8].starts_with("GET /api/admin/observability/events?limit=5&worker=demo "));
    assert!(requests[9].starts_with("GET /api/admin/keys "));
    assert!(requests[10].starts_with("POST /api/admin/keys "));
    assert!(requests[10].contains("\"permissions\":[\"workers:read\"]"));
    assert!(requests[11].starts_with("POST /api/admin/keys/7/revoke "));
}
