//! Deno CLI execution bridge.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bytes::Bytes;
use edger_core::{
    is_sensitive_env_key, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig,
};
use serde::{Deserialize, Serialize};

use crate::deno_bundle::{
    default_deno_executable, entry_needs_bundle, DenoCliBundler, ModuleBundler,
};

const RESPONSE_PREFIX: &str = "__EDGER_RESPONSE__";

#[derive(Debug, Clone)]
pub struct DenoCliRunner {
    executable: String,
}

#[derive(Debug, Serialize)]
struct BridgeRequest {
    body: Option<Vec<u8>>,
    headers: Vec<(String, String)>,
    method: String,
    uri: String,
}

#[derive(Debug, Deserialize)]
struct BridgeResponse {
    body: Option<Vec<u8>>,
    headers: Vec<(String, String)>,
    status: u16,
}

impl Default for DenoCliRunner {
    fn default() -> Self {
        Self {
            executable: default_deno_executable(),
        }
    }
}

impl DenoCliRunner {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn execute_fetch(
        &self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let worker_dir = config.worker_dir.as_ref().ok_or_else(|| {
            IsolationError::new("DENO_WORKER_DIR_MISSING", "worker_dir is required for Deno")
        })?;
        let worker_dir = worker_dir.canonicalize().map_err(|err| {
            IsolationError::new(
                "DENO_WORKER_DIR_INVALID",
                format!("invalid worker_dir: {err}"),
            )
        })?;
        let entrypoint = resolve_entrypoint(&worker_dir, config.entrypoint.as_deref())?;
        let (entry_url, bundle_dir) = if entry_needs_bundle(&worker_dir, &entrypoint)? {
            let bundle_dir = create_bundle_dir(&worker_dir)?;
            let bundler = DenoCliBundler::new(self.executable.clone());
            let bundle = bundler.bundle_entrypoint(&worker_dir, &entrypoint, bundle_dir.path())?;
            (path_to_file_url(Path::new(&bundle.path))?, Some(bundle_dir))
        } else {
            (path_to_file_url(&entrypoint)?, None)
        };
        let input = BridgeRequest {
            body: req.body.map(|body| body.to_vec()),
            headers: req.headers,
            method: req.method,
            uri: req.uri,
        };
        let input_json = serde_json::to_vec(&input).map_err(|err| {
            IsolationError::new(
                "DENO_BRIDGE_SERIALIZE",
                format!("request serialize failed: {err}"),
            )
        })?;
        let output = self.run_bridge(
            &worker_dir,
            bundle_dir.as_ref().map(|dir| dir.path()),
            &entry_url,
            &input_json,
            Duration::from_millis(config.timeout_ms),
            &config.env,
        )?;
        let bridge = parse_bridge_response(&output)?;
        Ok(SerializedResponse {
            status: bridge.status,
            headers: bridge.headers,
            body: bridge.body.filter(|body| !body.is_empty()).map(Bytes::from),
        })
    }

    fn run_bridge(
        &self,
        worker_dir: &Path,
        bundle_dir: Option<&Path>,
        entry_url: &str,
        input_json: &[u8],
        timeout: Duration,
        manifest_env: &std::collections::HashMap<String, String>,
    ) -> Result<String, IsolationError> {
        let script_file = write_bridge_script(worker_dir, entry_url)?;
        let mut command = Command::new(&self.executable);
        command
            .arg("run")
            .arg("--no-check")
            .arg("--no-prompt")
            .env_clear();
        apply_permission_flags(&mut command, worker_dir, bundle_dir);
        inject_runtime_env(&mut command);
        inject_manifest_env(&mut command, manifest_env);
        if let Some(config_path) = deno_config_path(worker_dir) {
            command.arg("--config").arg(config_path);
        }
        let mut child = command
            .arg(script_file.path())
            .current_dir(worker_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| {
                IsolationError::new(
                    "DENO_SPAWN_FAILED",
                    format!("failed to spawn {}: {err}", self.executable),
                )
            })?;

        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(input_json).map_err(|err| {
                IsolationError::new("DENO_STDIN_FAILED", format!("stdin write failed: {err}"))
            })?;
        }
        drop(child.stdin.take());

        let started = Instant::now();
        loop {
            if child
                .try_wait()
                .map_err(|err| {
                    IsolationError::new("DENO_WAIT_FAILED", format!("deno wait failed: {err}"))
                })?
                .is_some()
            {
                break;
            }
            if started.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(IsolationError::new(
                    "DENO_TIMEOUT",
                    format!("deno execution exceeded {}ms", timeout.as_millis()),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let output = child.wait_with_output().map_err(|err| {
            IsolationError::new("DENO_WAIT_FAILED", format!("deno wait failed: {err}"))
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            if stdout.lines().any(|line| line.starts_with(RESPONSE_PREFIX)) {
                return Ok(stdout);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(IsolationError::new(
                "DENO_EXEC_FAILED",
                format!(
                    "deno exited with status {:?}; stderr={}; stdout={}",
                    output.status.code(),
                    stderr.trim(),
                    stdout.trim()
                ),
            ));
        }

        Ok(stdout)
    }
}

/// Persist the bridge script so `deno run` can execute it with an explicit
/// permission set (`deno eval` always grants full permissions).
///
/// The script lives inside the worker dir: Deno resolves `package.json`
/// (CommonJS detection, `type: commonjs`) from the main module location, so a
/// script under the system temp dir would silently disable the Node compat
/// path. Falls back to the system temp dir for read-only worker dirs.
fn write_bridge_script(
    worker_dir: &Path,
    entry_url: &str,
) -> Result<tempfile::NamedTempFile, IsolationError> {
    let script = bridge_script(entry_url);
    let mut builder = tempfile::Builder::new();
    builder.prefix(".edger-bridge-").suffix(".mjs");
    let mut file = builder
        .tempfile_in(worker_dir)
        .or_else(|_| builder.tempfile())
        .map_err(|err| {
            IsolationError::new(
                "DENO_BRIDGE_SCRIPT",
                format!("failed to create bridge script: {err}"),
            )
        })?;
    file.write_all(script.as_bytes()).map_err(|err| {
        IsolationError::new(
            "DENO_BRIDGE_SCRIPT",
            format!("failed to write bridge script: {err}"),
        )
    })?;
    Ok(file)
}

fn create_bundle_dir(worker_dir: &Path) -> Result<tempfile::TempDir, IsolationError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(".edger-bundle-");
    builder
        .tempdir_in(worker_dir)
        .or_else(|_| {
            let mut fallback = tempfile::Builder::new();
            fallback.prefix("edger-bundle-");
            fallback.tempdir()
        })
        .map_err(|err| {
            IsolationError::new(
                "DENO_BUNDLE_TMP",
                format!("failed to create bundle tempdir: {err}"),
            )
        })
}

/// Sandbox policy for worker execution: read access to the worker dir only
/// and network per `EDGER_DENO_ALLOW_NET` (default: allowed). Env access is
/// unrestricted because the child environment is cleared and rebuilt from the
/// filtered manifest keys — Buntime semantics expect a filtered variable to
/// read as `undefined`, not to throw. Write, run, ffi and sys stay denied.
fn apply_permission_flags(command: &mut Command, worker_dir: &Path, bundle_dir: Option<&Path>) {
    let mut read_paths = vec![worker_dir.display().to_string()];
    if let Some(bundle_dir) = bundle_dir.filter(|path| !path.starts_with(worker_dir)) {
        read_paths.push(bundle_dir.display().to_string());
    }
    command.arg(format!("--allow-read={}", read_paths.join(",")));
    command.arg("--allow-env");

    match std::env::var("EDGER_DENO_ALLOW_NET")
        .map(|value| value.trim().to_string())
        .as_deref()
    {
        Ok("false") | Ok("0") | Ok("none") => {}
        Ok("") | Ok("true") | Ok("1") | Err(_) => {
            command.arg("--allow-net");
        }
        Ok(hosts) => {
            command.arg(format!("--allow-net={hosts}"));
        }
    }
}

fn inject_runtime_env(command: &mut Command) {
    for key in ["PATH", "DENO_DIR", "TMPDIR", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
}

fn inject_manifest_env(
    command: &mut Command,
    manifest_env: &std::collections::HashMap<String, String>,
) {
    for (key, value) in manifest_env {
        if !is_sensitive_env_key(key) {
            command.env(key, value);
        }
    }
}

fn parse_bridge_response(stdout: &str) -> Result<BridgeResponse, IsolationError> {
    let line = stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(RESPONSE_PREFIX))
        .ok_or_else(|| {
            IsolationError::new(
                "DENO_BRIDGE_RESPONSE_MISSING",
                format!(
                    "missing {RESPONSE_PREFIX} marker in stdout: {}",
                    stdout.trim()
                ),
            )
        })?;
    serde_json::from_str(line).map_err(|err| {
        IsolationError::new("DENO_BRIDGE_PARSE", format!("response parse failed: {err}"))
    })
}

fn resolve_entrypoint(
    worker_dir: &Path,
    configured: Option<&str>,
) -> Result<PathBuf, IsolationError> {
    let base = worker_dir.canonicalize().map_err(|err| {
        IsolationError::new(
            "DENO_WORKER_DIR_INVALID",
            format!("invalid worker_dir: {err}"),
        )
    })?;
    let candidates = if let Some(entry) = configured {
        vec![entry.to_string()]
    } else {
        vec!["index.ts".into(), "index.js".into(), "index.mjs".into()]
    };

    for candidate in candidates {
        if candidate.contains("..") {
            return Err(IsolationError::new(
                "DENO_ENTRYPOINT_DENIED",
                "entrypoint must stay inside worker_dir",
            ));
        }
        let path = base.join(candidate);
        if path.exists() {
            let canonical = path.canonicalize().map_err(|err| {
                IsolationError::new(
                    "DENO_ENTRYPOINT_INVALID",
                    format!("invalid entrypoint: {err}"),
                )
            })?;
            if !canonical.starts_with(&base) {
                return Err(IsolationError::new(
                    "DENO_ENTRYPOINT_DENIED",
                    "entrypoint must stay inside worker_dir",
                ));
            }
            return Ok(canonical);
        }
    }

    Err(IsolationError::new(
        "DENO_ENTRYPOINT_MISSING",
        "no index.{ts,js,mjs} entrypoint found",
    ))
}

fn deno_config_path(worker_dir: &Path) -> Option<PathBuf> {
    ["deno.json", "deno.jsonc"]
        .iter()
        .map(|name| worker_dir.join(name))
        .find(|path| path.is_file())
}

fn path_to_file_url(path: &Path) -> Result<String, IsolationError> {
    let path = path.canonicalize().map_err(|err| {
        IsolationError::new(
            "DENO_ENTRYPOINT_INVALID",
            format!("invalid entrypoint: {err}"),
        )
    })?;
    Ok(format!("file://{}", path.to_string_lossy()))
}

fn bridge_script(entry_url: &str) -> String {
    let entry_json = serde_json::to_string(entry_url).expect("entry URL serializes");
    format!(
        r#"
const RESPONSE_PREFIX = {response_prefix:?};
const entryUrl = {entry_json};

async function readStdin() {{
  const chunks = [];
  for await (const chunk of Deno.stdin.readable) {{
    chunks.push(chunk);
  }}
  const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {{
    bytes.set(chunk, offset);
    offset += chunk.length;
  }}
  return JSON.parse(new TextDecoder().decode(bytes));
}}

let capturedHandler = null;
let capturedNodeHandler = null;
const originalServe = Deno.serve;
Deno.serve = (arg) => {{
  if (typeof arg === "function") {{
    capturedHandler = arg;
  }} else if (arg && typeof arg.fetch === "function") {{
    capturedHandler = arg.fetch.bind(arg);
  }} else if (arg && typeof arg.handler === "function") {{
    capturedHandler = arg.handler.bind(arg);
  }}
  return {{
    finished: Promise.resolve(),
    ref() {{}},
    shutdown() {{}},
    unref() {{}},
  }};
}};

try {{
  const nodeHttp = await import("node:http");
  const http = nodeHttp.default ?? nodeHttp;
  http.createServer = (...args) => {{
    const listener = args.find((arg) => typeof arg === "function");
    if (listener) {{
      capturedNodeHandler = listener;
    }}
    return {{
      address() {{
        return {{ address: "127.0.0.1", family: "IPv4", port: 0 }};
      }},
      close(callback) {{
        if (typeof callback === "function") callback();
        return this;
      }},
      listen(...listenArgs) {{
        const callback = listenArgs.find((arg) => typeof arg === "function");
        if (callback) callback();
        return this;
      }},
      on() {{ return this; }},
      once() {{ return this; }},
      addListener() {{ return this; }},
      removeListener() {{ return this; }},
      ref() {{ return this; }},
      unref() {{ return this; }},
    }};
  }};
}} catch (_) {{
  // Non-Node workers do not need the CommonJS server-listen adapter.
}}

let mod;
try {{
  mod = await import(entryUrl + "?edger=" + crypto.randomUUID());
}} finally {{
  Deno.serve = originalServe;
}}

let handler = capturedHandler;
if (!handler && mod) {{
  if (typeof mod.default === "function") {{
    handler = mod.default;
  }} else if (mod.default && typeof mod.default.fetch === "function") {{
    handler = mod.default.fetch.bind(mod.default);
  }} else if (typeof mod.fetch === "function") {{
    handler = mod.fetch;
  }}
}}
if (!handler && typeof capturedNodeHandler === "function") {{
  handler = (request) => dispatchNodeHandler(capturedNodeHandler, request);
}}

const routesTable = (mod && ((mod.default && mod.default.routes) || mod.routes)) || null;
if (routesTable) {{
  handler = makeRoutesHandler(routesTable, handler);
}}
if (typeof handler !== "function") {{
  throw new Error("edger: no fetch handler or routes table found in module");
}}

function matchRoutePattern(pattern, pathname) {{
  const patternParts = pattern.split("/");
  const pathParts = pathname.split("/");
  const params = {{}};
  for (let index = 0; index < patternParts.length; index++) {{
    const part = patternParts[index];
    if (part === "*") {{
      return index === patternParts.length - 1 ? params : null;
    }}
    if (part.startsWith(":")) {{
      const value = pathParts[index];
      if (value === undefined || value === "") {{
        return null;
      }}
      params[part.slice(1)] = decodeURIComponent(value);
      continue;
    }}
    if (part !== pathParts[index]) {{
      return null;
    }}
  }}
  return patternParts.length === pathParts.length ? params : null;
}}

function makeRoutesHandler(routes, fallback) {{
  const entries = Object.entries(routes);
  return async (request) => {{
    const pathname = new URL(request.url).pathname;
    let target = null;
    let params = null;
    for (const [pattern, value] of entries) {{
      if (pattern === pathname) {{
        target = value;
        params = {{}};
        break;
      }}
    }}
    if (target === null) {{
      for (const [pattern, value] of entries) {{
        if (!pattern.includes(":") && !pattern.includes("*")) {{
          continue;
        }}
        const matched = matchRoutePattern(pattern, pathname);
        if (matched) {{
          target = value;
          params = matched;
          break;
        }}
      }}
    }}
    if (target === null) {{
      if (typeof fallback === "function") {{
        return fallback(request);
      }}
      return new Response("route not found", {{ status: 404 }});
    }}
    if (target && typeof target === "object" && !(target instanceof Response)) {{
      target = target[request.method.toUpperCase()];
      if (target === undefined) {{
        return new Response("method not allowed", {{ status: 405 }});
      }}
    }}
    if (target instanceof Response) {{
      return target.clone();
    }}
    if (typeof target !== "function") {{
      throw new Error("edger: invalid routes table entry for " + pathname);
    }}
    Object.defineProperty(request, "params", {{
      configurable: true,
      value: params,
    }});
    return target(request);
  }};
}}

function rawHeadersFrom(headers) {{
  const rawHeaders = [];
  for (const [name, value] of headers.entries()) {{
    rawHeaders.push(name, value);
  }}
  return rawHeaders;
}}

async function createNodeRequest(request) {{
  const {{ Readable }} = await import("node:stream");
  const url = new URL(request.url);
  const body = request.body ? Readable.fromWeb(request.body) : Readable.from([]);
  body.method = request.method;
  body.url = url.pathname + url.search;
  body.headers = Object.fromEntries(request.headers.entries());
  body.rawHeaders = rawHeadersFrom(request.headers);
  body.socket = {{
    encrypted: url.protocol === "https:",
    remoteAddress: "127.0.0.1",
  }};
  body.connection = body.socket;
  return body;
}}

function applyNodeHeaders(target, headers) {{
  if (!headers) return;
  if (headers instanceof Headers) {{
    for (const [name, value] of headers.entries()) {{
      target.set(name, value);
    }}
    return;
  }}
  if (Array.isArray(headers)) {{
    for (let index = 0; index < headers.length; index += 2) {{
      target.set(String(headers[index]), String(headers[index + 1] ?? ""));
    }}
    return;
  }}
  for (const [name, value] of Object.entries(headers)) {{
    if (Array.isArray(value)) {{
      target.set(name, value.map(String).join(", "));
    }} else if (value !== undefined) {{
      target.set(name, String(value));
    }}
  }}
}}

function concatChunks(chunks, total) {{
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {{
    bytes.set(chunk, offset);
    offset += chunk.length;
  }}
  return bytes;
}}

function createNodeResponse(resolve) {{
  const headers = new Headers();
  const chunks = [];
  let total = 0;
  const response = {{
    statusCode: 200,
    statusMessage: "OK",
    headersSent: false,
    writableEnded: false,
    setHeader(name, value) {{
      applyNodeHeaders(headers, {{ [name]: value }});
      return this;
    }},
    getHeader(name) {{
      return headers.get(String(name));
    }},
    hasHeader(name) {{
      return headers.has(String(name));
    }},
    removeHeader(name) {{
      headers.delete(String(name));
      return this;
    }},
    writeHead(statusCode, reasonOrHeaders, maybeHeaders) {{
      this.statusCode = statusCode;
      if (typeof reasonOrHeaders === "string") {{
        this.statusMessage = reasonOrHeaders;
        applyNodeHeaders(headers, maybeHeaders);
      }} else {{
        applyNodeHeaders(headers, reasonOrHeaders);
      }}
      this.headersSent = true;
      return this;
    }},
    write(chunk) {{
      const bytes = asUint8Array(chunk);
      chunks.push(bytes);
      total += bytes.length;
      return true;
    }},
    end(chunk) {{
      if (chunk !== undefined) {{
        this.write(chunk);
      }}
      this.writableEnded = true;
      this.headersSent = true;
      resolve(new Response(concatChunks(chunks, total), {{
        headers,
        status: this.statusCode,
      }}));
      return this;
    }},
    flushHeaders() {{
      this.headersSent = true;
    }},
    cork() {{}},
    uncork() {{}},
    on() {{ return this; }},
    once() {{ return this; }},
    addListener() {{ return this; }},
    removeListener() {{ return this; }},
    emit() {{ return false; }},
  }};
  return response;
}}

async function dispatchNodeHandler(nodeHandler, request) {{
  const nodeRequest = await createNodeRequest(request);
  return await new Promise((resolve, reject) => {{
    const nodeResponse = createNodeResponse(resolve);
    try {{
      const result = nodeHandler(nodeRequest, nodeResponse);
      if (result && typeof result.then === "function") {{
        result.catch(reject);
      }}
    }} catch (err) {{
      reject(err);
    }}
  }});
}}

const raw = await readStdin();
const headers = new Headers(raw.headers ?? []);
const init = {{ method: raw.method ?? "GET", headers }};
if (raw.body && !["GET", "HEAD"].includes(init.method.toUpperCase())) {{
  init.body = new Uint8Array(raw.body);
}}
const requestUrl = /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(raw.uri)
  ? raw.uri
  : "http://edger.local" + (raw.uri?.startsWith("/") ? raw.uri : "/" + raw.uri);
const response = await handler(new Request(requestUrl, init));

function asUint8Array(chunk) {{
  if (chunk instanceof Uint8Array) {{
    return chunk;
  }}
  if (chunk instanceof ArrayBuffer) {{
    return new Uint8Array(chunk);
  }}
  return new TextEncoder().encode(String(chunk));
}}

async function readWithIdleTimeout(reader, idleMs) {{
  let timerId;
  const timeout = new Promise((resolve) => {{
    timerId = setTimeout(() => resolve({{ idle: true }}), idleMs);
  }});
  const read = reader.read().then((value) => ({{ idle: false, value }}));
  const result = await Promise.race([read, timeout]);
  clearTimeout(timerId);
  return result;
}}

async function collectBody(response) {{
  if (!response.body) {{
    return new Uint8Array();
  }}
  const contentType = response.headers.get("content-type") ?? "";
  const boundedStream =
    contentType.includes("text/event-stream") ||
    contentType.includes("text/plain");
  const reader = response.body.getReader();
  const chunks = [];
  let total = 0;
  let seenChunk = false;

  while (true) {{
    const result = boundedStream && seenChunk
      ? await readWithIdleTimeout(reader, 25)
      : {{ idle: false, value: await reader.read() }};
    if (result.idle) {{
      await reader.cancel("edger bounded stream capture");
      break;
    }}
    if (result.value.done) {{
      break;
    }}
    const chunk = asUint8Array(result.value.value);
    chunks.push(chunk);
    total += chunk.length;
    seenChunk = true;
  }}

  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {{
    bytes.set(chunk, offset);
    offset += chunk.length;
  }}
  return bytes;
}}

const bytes = await collectBody(response);
const out = {{
  body: Array.from(bytes),
  headers: Array.from(response.headers.entries()),
  status: response.status,
}};
console.log(RESPONSE_PREFIX + JSON.stringify(out));
"#,
        response_prefix = RESPONSE_PREFIX,
        entry_json = entry_json,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_parser_uses_last_marker_line() {
        let response = parse_bridge_response(
            "worker log\n__EDGER_RESPONSE__{\"body\":[111,107],\"headers\":[],\"status\":200}\n",
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, Some(b"ok".to_vec()));
    }
}
