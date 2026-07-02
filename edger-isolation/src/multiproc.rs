//! Persistent Deno worker process over a Unix domain socket (Epic 15, story 15.A).
//!
//! The orchestrator spawns one long-lived `deno` process running a harness that
//! imports the user module ONCE and serves requests received over a UDS. This
//! replaces the v1 bridge's `deno eval` + stdout marker (spawn + re-import per
//! request). The Rust<->Deno wire is length-prefixed JSON (u32 LE + UTF-8) —
//! postcard is reserved for a future Rust-worker boundary.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    is_sensitive_env_key, Isolate, IsolationError, SerializedRequest, SerializedResponse,
    WorkerConfig,
};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};

const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRequest {
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    request_id: String,
    base_href: Option<String>,
}

#[derive(Deserialize)]
struct WireResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

#[derive(Deserialize)]
struct ReadyFrame {
    ready: bool,
    #[serde(default)]
    error: Option<String>,
}

/// A spawned, connected, module-loaded Deno worker process.
pub struct DenoWorkerProcess {
    child: Child,
    stream: UnixStream,
    timeout: Duration,
    // Keeps the socket/harness dir alive for the process lifetime.
    _workdir: TempDir,
}

impl DenoWorkerProcess {
    /// Spawn a persistent Deno worker for `worker_dir`/`entrypoint`, wait for it
    /// to connect and finish importing the module (ready handshake).
    pub async fn spawn(
        worker_dir: &Path,
        entrypoint: Option<&str>,
        timeout: Duration,
        env: &std::collections::HashMap<String, String>,
        memory_mb: Option<u32>,
    ) -> Result<Self, IsolationError> {
        let worker_dir = worker_dir.canonicalize().map_err(|err| {
            IsolationError::new("UDS_WORKER_DIR", format!("invalid worker_dir: {err}"))
        })?;
        let entry = resolve_entrypoint(&worker_dir, entrypoint)?;
        let entry_url = format!("file://{}", entry.to_string_lossy());

        let workdir = tempfile::Builder::new()
            .prefix("edger-uds-")
            .tempdir()
            .map_err(|err| IsolationError::new("UDS_TMP", format!("tempdir failed: {err}")))?;
        let socket_path = workdir.path().join("w.sock");
        let harness_path = workdir.path().join("harness.mjs");
        std::fs::write(&harness_path, harness_script()).map_err(|err| {
            IsolationError::new("UDS_HARNESS", format!("write harness failed: {err}"))
        })?;

        let listener = UnixListener::bind(&socket_path).map_err(|err| {
            IsolationError::new("UDS_BIND", format!("bind {}: {err}", socket_path.display()))
        })?;

        let executable = std::env::var("EDGER_DENO_BIN").unwrap_or_else(|_| "deno".into());
        let mut command = Command::new(&executable);
        command
            .arg("run")
            .arg("--no-check")
            .arg("--no-prompt")
            .arg(format!(
                "--allow-read={}",
                read_allowlist(&worker_dir, workdir.path())
            ))
            // Connecting a unix socket needs write on the socket dir.
            .arg(format!("--allow-write={}", workdir.path().display()))
            .arg("--allow-net")
            .arg("--allow-env")
            // node/npm frameworks (express etc.) may query os/sys info.
            .arg("--allow-sys")
            .env_clear();
        // Memory cap via the V8 heap limit — the correct, portable enforcement
        // for a V8 process (RLIMIT_AS is unusable: V8 reserves a huge virtual
        // address space and would be killed at boot). A worker that leaks past
        // the heap cap is aborted by V8 with a fatal OOM; the pool then recycles
        // it. On Linux, cgroup `memory.max` is the production-grade RSS backstop.
        if let Some(mb) = memory_mb {
            command.arg(format!("--v8-flags=--max-old-space-size={mb}"));
        }
        inject_runtime_env(&mut command);
        inject_manifest_env(&mut command, env);
        if let Some(config_path) = deno_config_path(&worker_dir) {
            command.arg("--config").arg(config_path);
        }
        let child = command
            .arg(&harness_path)
            .arg(&socket_path)
            .arg(&entry_url)
            .current_dir(&worker_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| {
                IsolationError::new("UDS_SPAWN", format!("spawn {executable}: {err}"))
            })?;

        // Accept the harness connection and read the ready handshake.
        let stream = match tokio::time::timeout(timeout, listener.accept()).await {
            Ok(Ok((stream, _))) => stream,
            Ok(Err(err)) => {
                return Err(spawn_error(child, format!("accept failed: {err}")).await);
            }
            Err(_) => {
                return Err(spawn_error(child, "worker did not connect in time".into()).await);
            }
        };

        let mut process = Self {
            child,
            stream,
            timeout,
            _workdir: workdir,
        };

        let ready_bytes = match tokio::time::timeout(timeout, read_frame(&mut process.stream)).await
        {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(err)) => return Err(process.fail(format!("ready read failed: {err}")).await),
            Err(_) => return Err(process.fail("ready handshake timed out".into()).await),
        };
        let ready: ReadyFrame = serde_json::from_slice(&ready_bytes)
            .map_err(|err| IsolationError::new("UDS_READY", format!("bad ready frame: {err}")))?;
        if !ready.ready {
            let detail = ready
                .error
                .unwrap_or_else(|| "worker failed to start".into());
            return Err(process.fail(detail).await);
        }

        Ok(process)
    }

    /// Send one request over the persistent connection and await the response.
    /// The module is already loaded, so this is just IPC + handler execution.
    pub async fn request(
        &mut self,
        req: SerializedRequest,
    ) -> Result<SerializedResponse, IsolationError> {
        let wire = WireRequest {
            method: req.method,
            uri: req.uri,
            headers: req.headers,
            body: req.body.map(|body| body.to_vec()),
            request_id: req.request_id,
            base_href: req.base_href,
        };
        let payload = serde_json::to_vec(&wire)
            .map_err(|err| IsolationError::new("UDS_ENCODE", err.to_string()))?;

        tokio::time::timeout(self.timeout, write_frame(&mut self.stream, &payload))
            .await
            .map_err(|_| IsolationError::new("UDS_TIMEOUT", "request write timed out"))?
            .map_err(|err| IsolationError::new("UDS_IO", format!("write failed: {err}")))?;

        let response_bytes = tokio::time::timeout(self.timeout, read_frame(&mut self.stream))
            .await
            .map_err(|_| IsolationError::new("UDS_TIMEOUT", "response read timed out"))?
            .map_err(|err| IsolationError::new("UDS_IO", format!("read failed: {err}")))?;

        let wire: WireResponse = serde_json::from_slice(&response_bytes)
            .map_err(|err| IsolationError::new("UDS_DECODE", err.to_string()))?;
        Ok(SerializedResponse {
            status: wire.status,
            headers: wire.headers,
            body: wire.body.filter(|body| !body.is_empty()).map(Bytes::from),
        })
    }

    /// Kill the process, surfacing any stderr as an error message.
    async fn fail(mut self, context: String) -> IsolationError {
        let _ = self.child.start_kill();
        let stderr = drain_stderr(&mut self.child).await;
        IsolationError::new("UDS_WORKER_FAILED", format!("{context}: {stderr}"))
    }
}

async fn spawn_error(mut child: Child, context: String) -> IsolationError {
    let _ = child.start_kill();
    let stderr = drain_stderr(&mut child).await;
    IsolationError::new("UDS_WORKER_FAILED", format!("{context}: {stderr}"))
}

async fn drain_stderr(child: &mut Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_millis(500), stderr.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).trim().to_string()
}

async fn write_frame(stream: &mut UnixStream, payload: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "frame too large"))?;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}

async fn read_frame(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut len_bytes = [0u8; 4];
    stream.read_exact(&mut len_bytes).await?;
    let len = u32::from_le_bytes(len_bytes);
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds max size",
        ));
    }
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

fn resolve_entrypoint(
    worker_dir: &Path,
    configured: Option<&str>,
) -> Result<PathBuf, IsolationError> {
    let candidates = if let Some(entry) = configured {
        vec![entry.to_string()]
    } else {
        vec!["index.ts".into(), "index.js".into(), "index.mjs".into()]
    };
    for candidate in candidates {
        if candidate.contains("..") {
            return Err(IsolationError::new(
                "UDS_ENTRYPOINT_DENIED",
                "entrypoint must stay inside worker_dir",
            ));
        }
        let path = worker_dir.join(&candidate);
        if path.is_file() {
            let canonical = path.canonicalize().map_err(|err| {
                IsolationError::new("UDS_ENTRYPOINT", format!("invalid entrypoint: {err}"))
            })?;
            if !canonical.starts_with(worker_dir) {
                return Err(IsolationError::new(
                    "UDS_ENTRYPOINT_DENIED",
                    "entrypoint must stay inside worker_dir",
                ));
            }
            return Ok(canonical);
        }
    }
    Err(IsolationError::new(
        "UDS_ENTRYPOINT_MISSING",
        "no index.{ts,js,mjs} entrypoint found",
    ))
}

fn deno_config_path(worker_dir: &Path) -> Option<PathBuf> {
    ["deno.json", "deno.jsonc"]
        .iter()
        .map(|name| worker_dir.join(name))
        .find(|path| path.is_file())
}

/// Read sandbox for the worker process: its own dir + the ephemeral socket dir,
/// plus the Deno module cache (`DENO_DIR` or platform default) so `npm:`/`jsr:`
/// packages resolve. The cache is read-only shared runtime data, not tenant data.
fn read_allowlist(worker_dir: &Path, workdir: &Path) -> String {
    let mut paths = vec![
        worker_dir.display().to_string(),
        workdir.display().to_string(),
    ];
    if let Ok(deno_dir) = std::env::var("DENO_DIR") {
        if !deno_dir.trim().is_empty() {
            paths.push(deno_dir);
        }
    } else if let Ok(home) = std::env::var("HOME") {
        paths.push(format!("{home}/Library/Caches/deno")); // macOS
        paths.push(format!("{home}/.cache/deno")); // Linux
        paths.push(format!("{home}/.deno"));
    }
    paths.join(",")
}

fn inject_runtime_env(command: &mut Command) {
    for key in ["PATH", "DENO_DIR", "HOME", "TMPDIR", "TEMP", "TMP"] {
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

fn harness_script() -> &'static str {
    include_str!("multiproc_harness.mjs")
}

/// `Isolate` backed by a persistent Deno worker process (the durable JS runtime).
///
/// The process is spawned lazily on the first fetch/routes call and reused
/// across requests (module loaded once). Static SPA serving stays pure-Rust — a
/// SPA-only worker never spawns a Deno process. A crashed process resets so the
/// next request respawns.
#[derive(Default)]
pub struct DenoProcessIsolate {
    process: Option<DenoWorkerProcess>,
}

impl DenoProcessIsolate {
    pub fn new() -> Self {
        Self::default()
    }

    async fn dispatch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        if self.process.is_none() {
            let worker_dir = config.worker_dir.as_ref().ok_or_else(|| {
                IsolationError::new("UDS_WORKER_DIR", "worker_dir is required for Deno process")
            })?;
            let timeout = Duration::from_millis(config.timeout_ms.max(1));
            let limits = crate::limits::ResourceLimits::from_config(config);
            let process = DenoWorkerProcess::spawn(
                worker_dir,
                config.entrypoint.as_deref(),
                timeout,
                &config.env,
                limits.memory_mb,
            )
            .await?;
            self.process = Some(process);
        }
        let result = self
            .process
            .as_mut()
            .expect("process just set")
            .request(req)
            .await;
        if result.is_err() {
            // Drop the (possibly dead) process so the next request respawns.
            self.process = None;
        }
        result
    }
}

#[async_trait]
impl Isolate for DenoProcessIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.dispatch(req, config).await
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.dispatch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        crate::static_spa::serve_static_spa(path, base_href, config)
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new(
            "NOT_IMPLEMENTED",
            "DenoProcessIsolate does not run Wasm",
        ))
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        // Dropping the process kills it (kill_on_drop).
        self.process = None;
        Ok(())
    }
}
