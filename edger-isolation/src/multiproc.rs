//! Persistent Deno worker process over a Unix domain socket (Epic 15, story 15.A).
//!
//! The orchestrator spawns one long-lived `deno` process running a harness that
//! imports the user module ONCE and serves requests received over a UDS. This
//! replaces the v1 bridge's `deno eval` + stdout marker (spawn + re-import per
//! request). The Rust<->Deno wire is length-prefixed JSON (u32 LE + UTF-8) —
//! postcard is reserved for a future Rust-worker boundary.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    DenoCacheMode, Isolate, IsolationError, SerializedRequest, SerializedResponse,
    StreamedResponse, TerminationOutcome, TerminationReport, WorkerConfig, WorkerResponse,
};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixListener;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use crate::deno_bundle::{
    default_deno_executable, entry_needs_bundle, DenoCliBundler, ModuleBundler,
};
use crate::deno_sandbox_policy::{deno_network_permission_args, read_allowlist, select_deno_dir};

const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;
pub const CONSOLE_LINE_MAX_BYTES: usize = 4 * 1024;
pub const CONSOLE_LINES_PER_SECOND: usize = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsoleStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug)]
pub struct ConsoleLogContext {
    pub namespace: Option<String>,
    pub worker: String,
    pub version: String,
}

#[derive(Clone, Debug)]
pub struct ConsoleLogRecord {
    pub at_ms: u128,
    pub context: ConsoleLogContext,
    pub process_id: String,
    pub stream: ConsoleStream,
    pub message: String,
    pub truncated: bool,
    pub dropped_before: u64,
}

pub type ConsoleLogSender = mpsc::Sender<ConsoleLogRecord>;

static PROCESS_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn sanitize_console_line(bytes: &[u8]) -> (String, bool) {
    let truncated = bytes.len() > CONSOLE_LINE_MAX_BYTES;
    let bytes = &bytes[..bytes.len().min(CONSOLE_LINE_MAX_BYTES)];
    let input = String::from_utf8_lossy(bytes);
    let mut clean = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for sequence in chars.by_ref() {
                if sequence.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if !ch.is_control() || ch == '\t' {
            clean.push(ch);
        }
    }
    let lower = clean.to_ascii_lowercase();
    if [
        "authorization",
        "cookie",
        "password",
        "secret",
        "token",
        "api_key",
        "api-key",
        "file://",
        "/users/",
        "/home/",
        "/var/",
        "/tmp/",
        "\\users\\",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return ("[redacted]".into(), truncated);
    }
    if truncated {
        clean.push('…');
    }
    (clean, truncated)
}

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
struct WireResponseHeader {
    status: u16,
    headers: Vec<(String, String)>,
}

/// Control frame telling the worker to run `beforeunload` and drain
/// `EdgeRuntime.waitUntil()` within a grace budget before the process is killed.
#[derive(Serialize)]
struct WireShutdown {
    #[serde(rename = "__control")]
    control: &'static str,
    reason: String,
    #[serde(rename = "graceMs")]
    grace_ms: u64,
}

/// The worker's shutdown ack (untagged JSON frame with the drained count).
#[derive(Deserialize)]
struct WireShutdownAck {
    #[serde(default)]
    drained: u64,
    #[serde(default, rename = "timedOut")]
    timed_out: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessDrainReport {
    pub drained: u64,
    pub timed_out: bool,
}

#[derive(Deserialize, Default)]
struct WireEndFrame {
    #[serde(default)]
    error: Option<String>,
}

/// Response frame tags (must match the harness).
const TAG_HEADER: u8 = b'H';
const TAG_CHUNK: u8 = b'C';
const TAG_END: u8 = b'E';

/// A streamed response from the worker process: status/headers up front, body
/// chunks delivered through the channel as the worker produces them.
pub struct ProcessStreamedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub chunks: mpsc::Receiver<Result<Bytes, IsolationError>>,
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
    write_half: OwnedWriteHalf,
    // The read half is owned by the response pump while a request streams; it
    // comes back through `restore_rx` on a CLEAN end-of-stream. An abnormal end
    // (mid-stream error, consumer dropped) never restores it — the process is
    // poisoned and the next request fails fast so the caller respawns.
    read_half: Option<OwnedReadHalf>,
    restore_rx: Option<oneshot::Receiver<OwnedReadHalf>>,
    timeout: Duration,
    // Keeps the bundled entrypoint alive for the process lifetime when bundling is required.
    _bundle_dir: Option<TempDir>,
    // Keeps the socket/harness dir alive for the process lifetime.
    _workdir: TempDir,
    // CPU/RSS limit sampler task (Linux only; no-op elsewhere). Self-terminates
    // when the process pid disappears, so no explicit abort is required.
    _limit_monitor: Option<tokio::task::JoinHandle<()>>,
    _console_tasks: Vec<tokio::task::JoinHandle<()>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    process_id: String,
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
        Self::spawn_with_policy(
            worker_dir,
            entrypoint,
            timeout,
            env,
            memory_mb,
            None,
            DenoCacheMode::default(),
            None,
            None,
            None,
        )
        .await
    }

    pub async fn spawn_with_console(
        worker_dir: &Path,
        entrypoint: Option<&str>,
        timeout: Duration,
        env: &std::collections::HashMap<String, String>,
        memory_mb: Option<u32>,
        console_sender: ConsoleLogSender,
        console_context: ConsoleLogContext,
    ) -> Result<Self, IsolationError> {
        Self::spawn_with_policy(
            worker_dir,
            entrypoint,
            timeout,
            env,
            memory_mb,
            None,
            DenoCacheMode::default(),
            None,
            Some(console_sender),
            Some(console_context),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_with_policy(
        worker_dir: &Path,
        entrypoint: Option<&str>,
        timeout: Duration,
        env: &std::collections::HashMap<String, String>,
        memory_mb: Option<u32>,
        allow_net: Option<&[String]>,
        deno_cache_mode: DenoCacheMode,
        caps: Option<crate::limits::ResourceLimits>,
        console_sender: Option<ConsoleLogSender>,
        console_context: Option<ConsoleLogContext>,
    ) -> Result<Self, IsolationError> {
        let worker_dir = worker_dir.canonicalize().map_err(|err| {
            IsolationError::new("UDS_WORKER_DIR", format!("invalid worker_dir: {err}"))
        })?;
        let entry = resolve_entrypoint(&worker_dir, entrypoint)?;

        let workdir = tempfile::Builder::new()
            .prefix("edger-uds-")
            .tempdir()
            .map_err(|err| IsolationError::new("UDS_TMP", format!("tempdir failed: {err}")))?;
        let socket_path = workdir.path().join("w.sock");
        let harness_path = workdir.path().join("harness.mjs");
        std::fs::write(&harness_path, harness_script()).map_err(|err| {
            IsolationError::new("UDS_HARNESS", format!("write harness failed: {err}"))
        })?;
        let (entry_url, bundle_dir) = if entry_needs_bundle(&worker_dir, &entry)? {
            let bundle_dir = create_bundle_dir(&worker_dir, workdir.path())?;
            let bundler = DenoCliBundler::default();
            let bundle = bundler.bundle_entrypoint(&worker_dir, &entry, bundle_dir.path())?;
            (path_to_file_url(Path::new(&bundle.path))?, Some(bundle_dir))
        } else {
            (path_to_file_url(&entry)?, None)
        };

        let listener = UnixListener::bind(&socket_path).map_err(|err| {
            IsolationError::new("UDS_BIND", format!("bind {}: {err}", socket_path.display()))
        })?;

        let deno_dir = select_deno_dir(
            &worker_dir,
            deno_cache_mode,
            std::env::var("DENO_DIR").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
            std::env::var("EDGER_DENO_CACHE_ROOT").ok().as_deref(),
        );
        if let Some(dir) = deno_dir.env_dir.as_deref() {
            std::fs::create_dir_all(dir).map_err(|err| {
                IsolationError::new(
                    "UDS_DENO_DIR",
                    format!("create DENO_DIR {}: {err}", dir.display()),
                )
            })?;
        }

        let executable = default_deno_executable();
        let mut command = Command::new(&executable);
        command
            .arg("run")
            .arg("--no-check")
            .arg("--no-prompt")
            // Enables Deno.openKv(). The app chooses the backend itself (:memory:,
            // a path it manages, or a remote KV Connect endpoint) — edger does not
            // prescribe or manage a KV location.
            .arg("--unstable-kv")
            // The harness loads the user module via dynamic `import(entryUrl)`, so
            // the worker is NEVER the process main module. Deno auto-detects a
            // `"type": "commonjs"` package as CommonJS only for the MAIN module;
            // dynamically-imported `.js` files need this flag to get `require`,
            // `module`, `exports` and `__dirname`. Without it, CommonJS workers
            // (node:http servers, @hono/node-server) fail at load with
            // `ReferenceError: require is not defined`. ESM workers are unaffected.
            .arg("--unstable-detect-cjs")
            .arg(format!(
                "--allow-read={}",
                read_allowlist(&worker_dir, workdir.path(), &deno_dir.read_dirs)
            ))
            // Connecting a unix socket needs write on the socket dir.
            .arg(format!("--allow-write={}", workdir.path().display()))
            .arg("--allow-env")
            // node/npm frameworks (express etc.) may query os/sys info.
            .arg("--allow-sys")
            .env_clear();
        for arg in deno_network_permission_args(
            allow_net,
            std::env::var("EDGER_DENO_ALLOW_NET").ok().as_deref(),
        ) {
            command.arg(arg);
        }
        // Memory cap via the V8 heap limit — the correct, portable enforcement
        // for a V8 process (RLIMIT_AS is unusable: V8 reserves a huge virtual
        // address space and would be killed at boot). A worker that leaks past
        // the heap cap is aborted by V8 with a fatal OOM; the pool then recycles
        // it. On Linux, cgroup `memory.max` is the production-grade RSS backstop.
        if let Some(mb) = memory_mb {
            command.arg(format!("--v8-flags=--max-old-space-size={mb}"));
        }
        inject_runtime_env(&mut command, deno_dir.env_dir.as_deref());
        inject_manifest_env(&mut command, env);
        if let Some(config_path) = deno_config_path(&worker_dir) {
            command.arg("--config").arg(config_path);
        }
        let mut child = command
            .arg(&harness_path)
            .arg(&socket_path)
            .arg(&entry_url)
            .current_dir(&worker_dir)
            .stdin(Stdio::null())
            .stdout(if console_sender.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| {
                IsolationError::new("UDS_SPAWN", format!("spawn {executable}: {err}"))
            })?;

        let process_id = format!(
            "{}-{}",
            child.id().unwrap_or_default(),
            PROCESS_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(20)));
        let mut console_tasks = Vec::new();
        if let Some(stderr) = child.stderr.take() {
            console_tasks.push(tokio::spawn(drain_console(
                stderr,
                ConsoleStream::Stderr,
                console_sender.clone(),
                console_context.clone(),
                process_id.clone(),
                Some(Arc::clone(&stderr_tail)),
            )));
        }
        if let Some(stdout) = child.stdout.take() {
            console_tasks.push(tokio::spawn(drain_console(
                stdout,
                ConsoleStream::Stdout,
                console_sender,
                console_context,
                process_id.clone(),
                None,
            )));
        }

        // Accept the harness connection and read the ready handshake.
        let stream = match tokio::time::timeout(timeout, listener.accept()).await {
            Ok(Ok((stream, _))) => stream,
            Ok(Err(err)) => {
                return Err(spawn_error(child, format!("accept failed: {err}"), stderr_tail).await);
            }
            Err(_) => {
                return Err(spawn_error(
                    child,
                    "worker did not connect in time".into(),
                    stderr_tail,
                )
                .await);
            }
        };

        let (read_half, write_half) = stream.into_split();
        let mut process = Self {
            child,
            write_half,
            read_half: Some(read_half),
            restore_rx: None,
            timeout,
            _bundle_dir: bundle_dir,
            _workdir: workdir,
            _limit_monitor: None,
            _console_tasks: console_tasks,
            stderr_tail,
            process_id,
        };

        let ready_bytes = match tokio::time::timeout(
            timeout,
            read_frame(process.read_half.as_mut().expect("read half present")),
        )
        .await
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

        // Start the CPU/RSS limit monitor once the process is ready. On Linux
        // it samples /proc and SIGKILLs the process on a hard breach (the pool
        // then respawns it); on other platforms the sampler yields nothing and
        // the task exits immediately.
        if let Some(caps) = caps {
            if caps.has_process_caps() {
                if let Some(pid) = process.child.id() {
                    let handle = tokio::spawn(async move {
                        crate::limits::monitor_process(
                            pid,
                            caps,
                            crate::limits::ProcFsSampler,
                            Duration::from_millis(500),
                            |breach| {
                                eprintln!(
                                    "[edger] worker pid {pid} soft resource limit reached: {breach:?}"
                                );
                            },
                            move |breach| {
                                eprintln!(
                                    "[edger] worker pid {pid} hard resource limit exceeded ({breach:?}); killing"
                                );
                                #[cfg(unix)]
                                // SAFETY: SIGKILL to a child pid we spawned.
                                unsafe {
                                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                                }
                            },
                        )
                        .await;
                    });
                    process._limit_monitor = Some(handle);
                }
            }
        }

        Ok(process)
    }

    /// Reclaim the read half: either it is resting between requests, or a prior
    /// stream is finishing and will hand it back through `restore_rx`. An
    /// abnormal previous stream never restores it — poisoned process.
    async fn reclaim_read_half(&mut self) -> Result<OwnedReadHalf, IsolationError> {
        if let Some(half) = self.read_half.take() {
            return Ok(half);
        }
        if let Some(rx) = self.restore_rx.take() {
            return match tokio::time::timeout(self.timeout, rx).await {
                Ok(Ok(half)) => Ok(half),
                Ok(Err(_)) => Err(IsolationError::new(
                    "UDS_POISONED",
                    "previous stream ended abnormally; process must be respawned",
                )),
                Err(_) => Err(IsolationError::new(
                    "UDS_TIMEOUT",
                    "previous stream still active; process must be respawned",
                )),
            };
        }
        Err(IsolationError::new(
            "UDS_POISONED",
            "read half lost; process must be respawned",
        ))
    }

    /// Send one request and stream the response: status/headers resolve as soon
    /// as the worker produced them; body chunks flow through the channel until
    /// the end frame (story 16.D).
    pub async fn request_stream(
        &mut self,
        req: SerializedRequest,
    ) -> Result<ProcessStreamedResponse, IsolationError> {
        let mut read_half = self.reclaim_read_half().await?;

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

        let write = async {
            tokio::time::timeout(self.timeout, write_frame(&mut self.write_half, &payload))
                .await
                .map_err(|_| IsolationError::new("UDS_TIMEOUT", "request write timed out"))?
                .map_err(|err| IsolationError::new("UDS_IO", format!("write failed: {err}")))
        };
        if let Err(err) = write.await {
            // Keep the half so a respawning caller sees a consistent state.
            self.read_half = Some(read_half);
            return Err(err);
        }

        let header_frame =
            match tokio::time::timeout(self.timeout, read_frame(&mut read_half)).await {
                Ok(Ok(frame)) => frame,
                Ok(Err(err)) => {
                    return Err(IsolationError::new("UDS_IO", format!("read failed: {err}")))
                }
                Err(_) => {
                    return Err(IsolationError::new(
                        "UDS_TIMEOUT",
                        "response read timed out",
                    ))
                }
            };
        let (tag, body) = split_tag(&header_frame)?;
        if tag != TAG_HEADER {
            return Err(IsolationError::new(
                "UDS_PROTOCOL",
                format!("expected header frame, got tag {tag:#x}"),
            ));
        }
        let header: WireResponseHeader = serde_json::from_slice(body)
            .map_err(|err| IsolationError::new("UDS_DECODE", err.to_string()))?;

        let (tx, rx) = mpsc::channel::<Result<Bytes, IsolationError>>(16);
        let (restore_tx, restore_rx) = oneshot::channel();
        self.restore_rx = Some(restore_rx);
        let frame_timeout = self.timeout;

        tokio::spawn(async move {
            loop {
                let frame =
                    match tokio::time::timeout(frame_timeout, read_frame(&mut read_half)).await {
                        Ok(Ok(frame)) => frame,
                        Ok(Err(err)) => {
                            let _ = tx
                                .send(Err(IsolationError::new(
                                    "UDS_IO",
                                    format!("stream read failed: {err}"),
                                )))
                                .await;
                            return; // abnormal: read half dropped, process poisoned
                        }
                        Err(_) => {
                            let _ = tx
                                .send(Err(IsolationError::new(
                                    "UDS_TIMEOUT",
                                    "stream stalled past the frame timeout",
                                )))
                                .await;
                            return; // abnormal
                        }
                    };
                let Ok((tag, body)) = split_tag(&frame) else {
                    return; // abnormal: empty frame
                };
                match tag {
                    TAG_CHUNK => {
                        if tx.send(Ok(Bytes::copy_from_slice(body))).await.is_err() {
                            // Consumer dropped mid-stream (client disconnect):
                            // frames for THIS response are still in flight, so
                            // the socket cannot be reused — do not restore.
                            return;
                        }
                    }
                    TAG_END => {
                        let end: WireEndFrame = serde_json::from_slice(body).unwrap_or_default();
                        if let Some(error) = end.error {
                            let _ = tx.send(Err(IsolationError::new("UDS_STREAM", error))).await;
                        }
                        let _ = restore_tx.send(read_half); // clean end: reusable
                        return;
                    }
                    _ => return, // abnormal: unknown tag
                }
            }
        });

        Ok(ProcessStreamedResponse {
            status: header.status,
            headers: header.headers,
            chunks: rx,
        })
    }

    /// Buffered request: streams internally and collects the whole body. Used
    /// by tests and non-streaming callers; infinite streams are bounded by the
    /// harness byte cap and the per-frame timeout.
    pub async fn request(
        &mut self,
        req: SerializedRequest,
    ) -> Result<SerializedResponse, IsolationError> {
        let mut streamed = self.request_stream(req).await?;
        let mut body = Vec::new();
        while let Some(chunk) = streamed.chunks.recv().await {
            body.extend_from_slice(&chunk?);
        }
        Ok(SerializedResponse {
            status: streamed.status,
            headers: streamed.headers,
            body: (!body.is_empty()).then(|| Bytes::from(body)),
        })
    }

    /// Graceful shutdown: send a control frame so the worker fires its
    /// `beforeunload` handlers and drains `EdgeRuntime.waitUntil()` promises
    /// within `grace`, then return so the caller can drop (kill) the process.
    ///
    /// The grace budget is SEPARATE from the request timeout: it runs after the
    /// last response, so it never counts against a request's wall-clock. Only
    /// possible when idle (read half available); mid-stream/poisoned processes
    /// skip straight to the kill. Returns the number of drained `waitUntil`
    /// promises the worker reported, when the ack arrives in time.
    pub async fn shutdown(&mut self, reason: &str, grace: Duration) -> Option<u64> {
        self.shutdown_report(reason, grace)
            .await
            .map(|report| report.drained)
    }

    pub async fn shutdown_report(
        &mut self,
        reason: &str,
        grace: Duration,
    ) -> Option<ProcessDrainReport> {
        // Reclaim the read half the same way a request does — after a request it
        // rests in `restore_rx`, not in `self.read_half`. A poisoned/mid-stream
        // process can't be reclaimed: skip straight to the kill.
        let mut read_half = self.reclaim_read_half().await.ok()?;
        let payload = serde_json::to_vec(&WireShutdown {
            control: "shutdown",
            reason: reason.to_string(),
            grace_ms: grace.as_millis() as u64,
        })
        .ok()?;
        if tokio::time::timeout(
            Duration::from_secs(1),
            write_frame(&mut self.write_half, &payload),
        )
        .await
        .is_err()
        {
            return None;
        }
        // Wait for the worker's drain ack, bounded by grace + a small margin.
        let deadline = grace.saturating_add(Duration::from_millis(500));
        match tokio::time::timeout(deadline, read_frame(&mut read_half)).await {
            Ok(Ok(frame)) => serde_json::from_slice::<WireShutdownAck>(&frame)
                .ok()
                .map(|ack| ProcessDrainReport {
                    drained: ack.drained,
                    timed_out: ack.timed_out,
                }),
            _ => None,
        }
    }

    /// Kill the process, surfacing any stderr as an error message.
    async fn fail(mut self, context: String) -> IsolationError {
        let _ = self.child.start_kill();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let stderr = stderr_tail_text(&self.stderr_tail);
        IsolationError::new("UDS_WORKER_FAILED", format!("{context}: {stderr}"))
    }
}

async fn spawn_error(
    mut child: Child,
    context: String,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
) -> IsolationError {
    let _ = child.start_kill();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let stderr = stderr_tail_text(&stderr_tail);
    IsolationError::new("UDS_WORKER_FAILED", format!("{context}: {stderr}"))
}

fn stderr_tail_text(tail: &Arc<Mutex<VecDeque<String>>>) -> String {
    tail.lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_default()
}

async fn drain_console<R>(
    mut reader: R,
    stream: ConsoleStream,
    sender: Option<ConsoleLogSender>,
    context: Option<ConsoleLogContext>,
    process_id: String,
    stderr_tail: Option<Arc<Mutex<VecDeque<String>>>>,
) where
    R: AsyncRead + Unpin,
{
    let mut chunk = [0u8; 4 * 1024];
    let mut line = Vec::with_capacity(CONSOLE_LINE_MAX_BYTES);
    let mut truncated = false;
    let mut window_started = Instant::now();
    let mut emitted = 0usize;
    let mut dropped = 0u64;
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        for byte in &chunk[..read] {
            if *byte == b'\n' {
                emit_console_line(
                    &line,
                    truncated,
                    stream,
                    sender.as_ref(),
                    context.as_ref(),
                    &process_id,
                    stderr_tail.as_ref(),
                    &mut window_started,
                    &mut emitted,
                    &mut dropped,
                );
                line.clear();
                truncated = false;
            } else if line.len() < CONSOLE_LINE_MAX_BYTES {
                line.push(*byte);
            } else {
                truncated = true;
            }
        }
    }
    if !line.is_empty() {
        emit_console_line(
            &line,
            truncated,
            stream,
            sender.as_ref(),
            context.as_ref(),
            &process_id,
            stderr_tail.as_ref(),
            &mut window_started,
            &mut emitted,
            &mut dropped,
        );
    }
    if dropped > 0 {
        if let (Some(sender), Some(context)) = (sender, context) {
            let _ = sender.try_send(ConsoleLogRecord {
                at_ms: now_ms(),
                context,
                process_id,
                stream,
                message: format!("[{dropped} console lines dropped]"),
                truncated: false,
                dropped_before: dropped,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_console_line(
    bytes: &[u8],
    already_truncated: bool,
    stream: ConsoleStream,
    sender: Option<&ConsoleLogSender>,
    context: Option<&ConsoleLogContext>,
    process_id: &str,
    stderr_tail: Option<&Arc<Mutex<VecDeque<String>>>>,
    window_started: &mut Instant,
    emitted: &mut usize,
    dropped: &mut u64,
) {
    let (message, truncated_by_sanitizer) = sanitize_console_line(bytes);
    let truncated = already_truncated || truncated_by_sanitizer;
    if let Some(tail) = stderr_tail {
        if let Ok(mut tail) = tail.lock() {
            if tail.len() == 20 {
                tail.pop_front();
            }
            tail.push_back(message.clone());
        }
    }
    if window_started.elapsed() >= Duration::from_secs(1) {
        *window_started = Instant::now();
        *emitted = 0;
    }
    let (Some(sender), Some(context)) = (sender, context) else {
        return;
    };
    if *emitted >= CONSOLE_LINES_PER_SECOND {
        *dropped = dropped.saturating_add(1);
        return;
    }
    let record = ConsoleLogRecord {
        at_ms: now_ms(),
        context: context.clone(),
        process_id: process_id.to_string(),
        stream,
        message,
        truncated,
        dropped_before: *dropped,
    };
    match sender.try_send(record) {
        Ok(()) => {
            *emitted += 1;
            *dropped = 0;
        }
        Err(_) => *dropped = dropped.saturating_add(1),
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn split_tag(frame: &[u8]) -> Result<(u8, &[u8]), IsolationError> {
    match frame.split_first() {
        Some((tag, body)) => Ok((*tag, body)),
        None => Err(IsolationError::new("UDS_PROTOCOL", "empty response frame")),
    }
}

async fn write_frame<W: AsyncWrite + Unpin>(stream: &mut W, payload: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "frame too large"))?;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}

async fn read_frame<R: AsyncRead + Unpin>(stream: &mut R) -> std::io::Result<Vec<u8>> {
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

fn create_bundle_dir(worker_dir: &Path, fallback_dir: &Path) -> Result<TempDir, IsolationError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(".edger-bundle-");
    builder
        .tempdir_in(worker_dir)
        .or_else(|_| {
            let mut fallback = tempfile::Builder::new();
            fallback.prefix("edger-bundle-");
            fallback.tempdir_in(fallback_dir)
        })
        .map_err(|err| {
            IsolationError::new(
                "UDS_BUNDLE_TMP",
                format!("failed to create bundle tempdir: {err}"),
            )
        })
}

fn path_to_file_url(path: &Path) -> Result<String, IsolationError> {
    let path = path.canonicalize().map_err(|err| {
        IsolationError::new("UDS_BUNDLE_OUTPUT", format!("invalid bundle output: {err}"))
    })?;
    Ok(format!("file://{}", path.to_string_lossy()))
}

fn inject_runtime_env(command: &mut Command, deno_dir: Option<&Path>) {
    for key in ["PATH", "HOME", "TMPDIR", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
    if let Some(deno_dir) = deno_dir {
        command.env("DENO_DIR", deno_dir);
    } else if let Ok(value) = std::env::var("DENO_DIR") {
        command.env("DENO_DIR", value);
    }
}

fn inject_manifest_env(
    command: &mut Command,
    manifest_env: &std::collections::HashMap<String, String>,
) {
    // Server workers are a trusted server-side context: inject ALL operator-declared
    // manifest env (DATABASE_URL, API keys, ...). Secrets never reach the browser —
    // that path is gated separately by the publicEnv allowlist (static_spa.rs).
    // DENO_DIR is reserved for the runtime cache dir and set by inject_runtime_env.
    for (key, value) in manifest_env {
        if !key.eq_ignore_ascii_case("DENO_DIR") {
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
    /// Grace budget for the beforeunload drain on graceful termination.
    shutdown_grace: Duration,
    console_sender: Option<ConsoleLogSender>,
    console_context: Option<ConsoleLogContext>,
}

impl DenoProcessIsolate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_console(sender: ConsoleLogSender, context: ConsoleLogContext) -> Self {
        Self {
            console_sender: Some(sender),
            console_context: Some(context),
            ..Self::default()
        }
    }

    async fn ensure_process(&mut self, config: &WorkerConfig) -> Result<(), IsolationError> {
        self.shutdown_grace = Duration::from_millis(config.shutdown_grace_ms);
        if self.process.is_none() {
            let worker_dir = config.worker_dir.as_ref().ok_or_else(|| {
                IsolationError::new("UDS_WORKER_DIR", "worker_dir is required for Deno process")
            })?;
            let timeout = Duration::from_millis(config.timeout_ms.max(1));
            let limits = crate::limits::ResourceLimits::from_config(config);
            let process = DenoWorkerProcess::spawn_with_policy(
                worker_dir,
                config.entrypoint.as_deref(),
                timeout,
                &config.env,
                limits.memory_mb,
                config.allow_net.as_deref(),
                config.deno_cache_mode,
                Some(limits.clone()),
                self.console_sender.clone(),
                self.console_context.clone(),
            )
            .await?;
            self.process = Some(process);
        }
        Ok(())
    }

    async fn dispatch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.ensure_process(config).await?;
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

    async fn dispatch_stream(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.ensure_process(config).await?;
        let result = self
            .process
            .as_mut()
            .expect("process just set")
            .request_stream(req)
            .await;
        match result {
            Ok(streamed) => Ok(WorkerResponse::Streamed(StreamedResponse {
                status: streamed.status,
                headers: streamed.headers,
                body: Box::pin(ReceiverBody(streamed.chunks)),
            })),
            Err(err) => {
                // Drop the (possibly dead/poisoned) process so the next request
                // respawns. Mid-STREAM failures are handled by the pool, which
                // recycles the whole instance.
                self.process = None;
                Err(err)
            }
        }
    }
}

/// Adapts the pump channel into the core `BodyStream` contract.
struct ReceiverBody(mpsc::Receiver<Result<Bytes, IsolationError>>);

impl futures_core::Stream for ReceiverBody {
    type Item = Result<Bytes, IsolationError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

#[async_trait]
impl Isolate for DenoProcessIsolate {
    async fn prepare(&mut self, config: &WorkerConfig) -> Result<(), IsolationError> {
        self.ensure_process(config).await
    }

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

    async fn execute_fetch_stream(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.dispatch_stream(req, config).await
    }

    async fn execute_routes_stream(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.dispatch_stream(req, config).await
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        self.terminate_with_report().await.map(|_| ())
    }

    async fn terminate_with_report(&mut self) -> Result<TerminationReport, IsolationError> {
        if let Some(process) = self.process.as_mut() {
            // Best-effort graceful drain (beforeunload + waitUntil) before the
            // process is dropped (killed). Bounded by the shutdown grace budget.
            let process_id = process.process_id.clone();
            let drain = process
                .shutdown_report("terminate", self.shutdown_grace)
                .await;
            let outcome = match drain {
                Some(report) if !report.timed_out => TerminationOutcome::Completed,
                _ => TerminationOutcome::TimedOut,
            };
            self.process = None;
            return Ok(TerminationReport {
                outcome,
                process_id: Some(process_id),
                drained_count: drain.map(|report| report.drained),
            });
        }
        // Dropping the process kills it (kill_on_drop).
        self.process = None;
        Ok(TerminationReport {
            outcome: TerminationOutcome::NotRunning,
            process_id: None,
            drained_count: None,
        })
    }
}

#[cfg(test)]
mod console_tests {
    use super::{sanitize_console_line, CONSOLE_LINE_MAX_BYTES};

    #[test]
    fn console_line_is_bounded_sanitized_and_redacted() {
        let long = vec![b'x'; CONSOLE_LINE_MAX_BYTES + 128];
        let (line, truncated) = sanitize_console_line(&long);
        assert!(truncated);
        assert!(line.len() <= CONSOLE_LINE_MAX_BYTES + 3);

        let (line, truncated) = sanitize_console_line(b"\x1b[31mboom\x1b[0m\0\xff");
        assert!(!truncated);
        assert_eq!(line, "boom�");

        let (line, _) = sanitize_console_line(b"authorization=Bearer secret-value");
        assert_eq!(line, "[redacted]");

        let (line, _) = sanitize_console_line(b"at file:///Users/operator/workers/app.ts:1");
        assert_eq!(line, "[redacted]");
    }
}
