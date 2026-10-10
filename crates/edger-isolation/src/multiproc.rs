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
use edger_core::wire::StreamProductionState;
use edger_core::{
    AbandonedStream, CompletionSignal, DenoCacheMode, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, StreamCompletion, StreamedResponse, TerminationOutcome, TerminationReport,
    WorkerConfig, WorkerResponse,
};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixListener;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant as TokioInstant;

use crate::deno_bundle::{
    default_deno_executable, entry_needs_bundle, DenoCliBundler, ModuleBundler,
};
use crate::deno_sandbox_policy::{
    deno_network_permission_args_with_uds, read_allowlist, select_deno_dir,
};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NodeHttpMode {
    Capture,
    Proxy,
}

impl NodeHttpMode {
    fn as_arg(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Proxy => "proxy",
        }
    }
}

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

/// Control frame telling the harness to cancel the in-flight response
/// (EDG-9 slice 2): the client abandoned the stream and the orchestrator is
/// draining it, so the harness aborts the request's `AbortSignal`, cancels
/// the body reader and answers with an end frame carrying
/// `{"cancelled":true}`. No id: the socket order guarantees a stale cancel
/// (its response already ended) arrives before the next request frame.
#[derive(Serialize)]
struct WireCancel {
    #[serde(rename = "__control")]
    control: &'static str,
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

/// The classified outcome of a graceful-shutdown handshake (EDG-9):
/// the termination report must distinguish "no handshake was possible"
/// (socket not reclaimed — nothing was sent) from "the handshake was
/// attempted and the ack never arrived in time".
#[derive(Debug)]
enum ShutdownHandshake {
    /// The shutdown control frame was accepted and the worker acked.
    Acked(ProcessDrainReport),
    /// The read half could not be reclaimed (poisoned/lost) or the control
    /// frame could not be written: NO shutdown handshake took place.
    SocketPoisoned,
    /// The control frame was written but the ack never arrived within the
    /// grace budget (+ margin).
    AckTimedOut,
}

impl ShutdownHandshake {
    fn into_acked(self) -> Option<ProcessDrainReport> {
        match self {
            ShutdownHandshake::Acked(report) => Some(report),
            _ => None,
        }
    }
}

#[derive(Deserialize, Default)]
struct WireEndFrame {
    #[serde(default)]
    error: Option<String>,
    /// Set by the harness when the response ended because of a cancel
    /// control frame (EDG-9 slice 2): a CLEAN end inside the abandon drain
    /// (the socket is restored and the process reused), an unexpected
    /// protocol error outside of it.
    #[serde(default, rename = "cancelled")]
    cancelled: bool,
}

/// Response frame tags (must match the harness).
const TAG_HEADER: u8 = b'H';
const TAG_CHUNK: u8 = b'C';
const TAG_END: u8 = b'E';

/// Abandon-drain policy (EDG-9): when the consumer disappears before the
/// end frame, the reader keeps reading frames and discarding them — no
/// budget reservation, no enqueue — until the clean `TAG_END`, so the
/// process can be reused instead of poisoned and respawned. `0` in either
/// limit disables the drain (the socket is abandoned and the process
/// recycled, the pre-EDG-9 behavior).
#[derive(Clone, Copy, Debug)]
pub struct AbandonDrain {
    pub max_bytes: u64,
    pub max_ms: u64,
}

impl Default for AbandonDrain {
    fn default() -> Self {
        Self {
            max_bytes: edger_core::STREAM_ABANDON_DRAIN_MAX_BYTES_DEFAULT,
            max_ms: edger_core::STREAM_ABANDON_DRAIN_MAX_MS_DEFAULT,
        }
    }
}

impl AbandonDrain {
    /// The drain runs only when BOTH limits are positive.
    pub fn enabled(self) -> bool {
        self.max_bytes > 0 && self.max_ms > 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DrainOrigin {
    ClientGone,
    MaxDuration,
}

/// Snapshot of the stream-detach counters (byte-semaphore budget accounting).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamDetachStats {
    /// Responses whose production completed while the detach pipeline was
    /// active (the slot was released at production end, not at drain end).
    pub detached_total: u64,
    /// Chunks that had to wait (backpressure) because the per-response
    /// buffer cap was already full.
    pub fallback_cap_total: u64,
    /// Chunks that had to wait (backpressure) because the process-wide
    /// budget was exhausted.
    pub fallback_budget_total: u64,
    /// Abandon drains that reached a clean `TAG_END` within the limits: the
    /// process was reused (EDG-9).
    pub abandoned_drained_total: u64,
    /// Abandon drains that stopped past the byte limit (process recycled).
    pub abandoned_drain_bytes_limit_total: u64,
    /// Abandon drains that stopped past the time limit (process recycled).
    pub abandoned_drain_time_limit_total: u64,
    /// Abandon drains that stopped on a read error, an end frame with an
    /// error or a protocol error (process recycled).
    pub abandoned_drain_stream_error_total: u64,
    /// Abandoned sockets with the drain disabled (a limit of `0`): the
    /// socket was left mid-response and the process is poisoned (recycled).
    pub abandoned_socket_poisoned_total: u64,
    /// Abandon drains that stopped on the harness's CANCEL end frame
    /// (`{"cancelled":true}`, EDG-9 slice 2): the socket was restored and
    /// the process was reused.
    pub abandoned_cancelled_total: u64,
    /// Max-duration cuts by EDG-9 drain outcome. Kept separate from client
    /// abandonment so this family means the configured duration limit fired.
    pub max_duration_drained_total: u64,
    pub max_duration_drain_bytes_limit_total: u64,
    pub max_duration_drain_time_limit_total: u64,
    pub max_duration_drain_stream_error_total: u64,
    pub max_duration_socket_poisoned_total: u64,
    pub max_duration_cancelled_total: u64,
}

/// Process-wide budget for the stream-detach pipelines, shared by every
/// isolate of the `edger` process. It is a byte semaphore: every chunk the
/// reader task has READ but the forwarder has not yet delivered to the body
/// channel holds permits; permits are released the moment a chunk is
/// delivered (or discarded), so the budget never leaks across responses. The
/// 16-slot body channel is deliberately outside the budget.
#[derive(Debug)]
pub struct StreamDetachBudget {
    total: u64,
    permits: tokio::sync::Semaphore,
    stats: StreamDetachStatsInner,
}

#[derive(Debug, Default)]
struct StreamDetachStatsInner {
    detached_total: AtomicU64,
    fallback_cap_total: AtomicU64,
    fallback_budget_total: AtomicU64,
    abandoned_drained_total: AtomicU64,
    abandoned_drain_bytes_limit_total: AtomicU64,
    abandoned_drain_time_limit_total: AtomicU64,
    abandoned_drain_stream_error_total: AtomicU64,
    abandoned_socket_poisoned_total: AtomicU64,
    abandoned_cancelled_total: AtomicU64,
    max_duration_drained_total: AtomicU64,
    max_duration_drain_bytes_limit_total: AtomicU64,
    max_duration_drain_time_limit_total: AtomicU64,
    max_duration_drain_stream_error_total: AtomicU64,
    max_duration_socket_poisoned_total: AtomicU64,
    max_duration_cancelled_total: AtomicU64,
}

impl StreamDetachBudget {
    pub fn new(total: u64) -> Self {
        Self {
            total,
            permits: tokio::sync::Semaphore::new(total.min(usize::MAX as u64) as usize),
            stats: StreamDetachStatsInner::default(),
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.total
    }

    /// Bytes currently held by read-but-undelivered chunks (saturated to
    /// `usize` when `total` exceeds the pointer width).
    pub fn reserved_bytes(&self) -> u64 {
        (self.total.min(usize::MAX as u64) as usize - self.permits.available_permits()) as u64
    }

    pub fn stats(&self) -> StreamDetachStats {
        StreamDetachStats {
            detached_total: self.stats.detached_total.load(Ordering::Acquire),
            fallback_cap_total: self.stats.fallback_cap_total.load(Ordering::Acquire),
            fallback_budget_total: self.stats.fallback_budget_total.load(Ordering::Acquire),
            abandoned_drained_total: self.stats.abandoned_drained_total.load(Ordering::Acquire),
            abandoned_drain_bytes_limit_total: self
                .stats
                .abandoned_drain_bytes_limit_total
                .load(Ordering::Acquire),
            abandoned_drain_time_limit_total: self
                .stats
                .abandoned_drain_time_limit_total
                .load(Ordering::Acquire),
            abandoned_drain_stream_error_total: self
                .stats
                .abandoned_drain_stream_error_total
                .load(Ordering::Acquire),
            abandoned_socket_poisoned_total: self
                .stats
                .abandoned_socket_poisoned_total
                .load(Ordering::Acquire),
            abandoned_cancelled_total: self.stats.abandoned_cancelled_total.load(Ordering::Acquire),
            max_duration_drained_total: self
                .stats
                .max_duration_drained_total
                .load(Ordering::Acquire),
            max_duration_drain_bytes_limit_total: self
                .stats
                .max_duration_drain_bytes_limit_total
                .load(Ordering::Acquire),
            max_duration_drain_time_limit_total: self
                .stats
                .max_duration_drain_time_limit_total
                .load(Ordering::Acquire),
            max_duration_drain_stream_error_total: self
                .stats
                .max_duration_drain_stream_error_total
                .load(Ordering::Acquire),
            max_duration_socket_poisoned_total: self
                .stats
                .max_duration_socket_poisoned_total
                .load(Ordering::Acquire),
            max_duration_cancelled_total: self
                .stats
                .max_duration_cancelled_total
                .load(Ordering::Acquire),
        }
    }

    /// The byte semaphore (used by the reader/forwarder of the detach
    /// pipeline, which live in this module).
    fn permits(&self) -> &tokio::sync::Semaphore {
        &self.permits
    }

    fn total_usize(&self) -> usize {
        self.total.min(usize::MAX as u64) as usize
    }

    fn record_detached(&self) {
        self.stats.detached_total.fetch_add(1, Ordering::AcqRel);
    }

    fn record_fallback_cap(&self) {
        self.stats.fallback_cap_total.fetch_add(1, Ordering::AcqRel);
    }

    fn record_fallback_budget(&self) {
        self.stats
            .fallback_budget_total
            .fetch_add(1, Ordering::AcqRel);
    }

    /// Abandon-drain outcome counters (EDG-9): the reader records WHICH
    /// outcome a consumer-abandoned response ended in; the budget is the
    /// shared surface the pool/tests can read (the core wire types carry no
    /// outcome beyond the completion signal).
    fn record_abandoned_drained(&self) {
        self.stats
            .abandoned_drained_total
            .fetch_add(1, Ordering::AcqRel);
    }

    fn record_abandoned_drain_bytes_limit(&self) {
        self.stats
            .abandoned_drain_bytes_limit_total
            .fetch_add(1, Ordering::AcqRel);
    }

    fn record_abandoned_drain_time_limit(&self) {
        self.stats
            .abandoned_drain_time_limit_total
            .fetch_add(1, Ordering::AcqRel);
    }

    fn record_abandoned_drain_stream_error(&self) {
        self.stats
            .abandoned_drain_stream_error_total
            .fetch_add(1, Ordering::AcqRel);
    }

    fn record_abandoned_socket_poisoned(&self) {
        self.stats
            .abandoned_socket_poisoned_total
            .fetch_add(1, Ordering::AcqRel);
    }

    fn record_abandoned_cancelled(&self) {
        self.stats
            .abandoned_cancelled_total
            .fetch_add(1, Ordering::AcqRel);
    }

    fn record_max_duration(&self, outcome: AbandonedStream) {
        let counter = match outcome {
            AbandonedStream::Drained => &self.stats.max_duration_drained_total,
            AbandonedStream::BytesLimit => &self.stats.max_duration_drain_bytes_limit_total,
            AbandonedStream::TimeLimit => &self.stats.max_duration_drain_time_limit_total,
            AbandonedStream::StreamError => &self.stats.max_duration_drain_stream_error_total,
            AbandonedStream::SocketPoisoned => &self.stats.max_duration_socket_poisoned_total,
            AbandonedStream::Cancelled => &self.stats.max_duration_cancelled_total,
            AbandonedStream::RelayTimeout => return,
        };
        counter.fetch_add(1, Ordering::AcqRel);
    }

    fn record_drain_outcome(&self, origin: DrainOrigin, outcome: AbandonedStream) {
        match origin {
            DrainOrigin::ClientGone => match outcome {
                AbandonedStream::Drained => self.record_abandoned_drained(),
                AbandonedStream::BytesLimit => self.record_abandoned_drain_bytes_limit(),
                AbandonedStream::TimeLimit => self.record_abandoned_drain_time_limit(),
                AbandonedStream::StreamError => self.record_abandoned_drain_stream_error(),
                AbandonedStream::SocketPoisoned => self.record_abandoned_socket_poisoned(),
                AbandonedStream::Cancelled => self.record_abandoned_cancelled(),
                AbandonedStream::RelayTimeout => {}
            },
            DrainOrigin::MaxDuration => self.record_max_duration(outcome),
        }
    }
}

/// Detach-pipeline policy for one persistent-process isolate: the per-response
/// buffered cap (`0` disables the pipeline entirely, keeping the legacy
/// blocking behavior) and the process-wide budget shared by all isolates, plus
/// the abandon-drain policy (EDG-9) applied when the consumer disappears
/// before the end frame.
#[derive(Clone, Debug)]
pub struct StreamDetach {
    pub max_bytes: u64,
    pub budget: Arc<StreamDetachBudget>,
    pub drain: AbandonDrain,
    /// Optional total duration from response-header arrival.
    pub max_duration: Option<Duration>,
}

/// One item of the detach pipeline's internal FIFO queue. The single queue
/// guarantees order: a newer chunk can never bypass an older one, and the
/// end/error marker is delivered strictly after the last chunk.
enum QueueItem {
    /// A produced chunk awaiting delivery to the body channel. The RAII
    /// `reservations` return the chunk's per-response and global permits
    /// when the item is dropped — on delivery, on discard, or on any early
    /// exit — so no code path can leak a reservation.
    Chunk {
        chunk: Bytes,
        reservations: ChunkReservations,
    },
    /// Terminal marker, enqueued by the reader after the last chunk and
    /// passed through in order by the forwarder.
    End(Result<(), IsolationError>),
}

/// RAII guard for one chunk's detach reservations: holds the per-response
/// and the global permits taken by `reserve_chunk` and returns them on
/// drop. The guard is dropped exactly when the chunk's fate is decided —
/// after successful delivery, when discarded (consumer gone), or on early
/// exit (enqueue failure, task unwind) — so every path returns the permits
/// automatically.
struct ChunkReservations {
    per_response: Arc<tokio::sync::Semaphore>,
    per_response_permits: u32,
    budget: Arc<StreamDetachBudget>,
    budget_permits: u32,
}

impl Drop for ChunkReservations {
    fn drop(&mut self) {
        if self.per_response_permits > 0 {
            self.per_response
                .add_permits(self.per_response_permits as usize);
        }
        if self.budget_permits > 0 {
            self.budget
                .permits()
                .add_permits(self.budget_permits as usize);
        }
    }
}

/// Keep a semaphore reservation HELD past the end of this scope. The
/// returned `SemaphorePermit` is deliberately forgotten: dropping it would
/// release the permits immediately, but a detach-pipeline reservation must
/// stay held until the forwarder releases it via `add_permits` — after the
/// chunk is delivered (or discarded). Safe: the permit's `Drop` is a pure
/// counter increment on the semaphore (no allocation, no other side
/// effect), so forgetting it leaks nothing.
fn hold_permit(permit: tokio::sync::SemaphorePermit<'_>) {
    std::mem::forget(permit);
}

/// Reserve `chunk_len` bytes on the per-response semaphore and on the
/// shared global budget, waiting when no permits are free (that wait IS the
/// backpressure: the worker slot stays held, exactly like the legacy path).
/// Returns an RAII guard that returns both reservations on drop. A chunk
/// larger than a semaphore's FULL capacity reserves that full capacity —
/// not its own size — so a single oversized chunk can never deadlock the
/// pipeline. (Permit counts are `u32`, the tokio semaphore width; a budget
/// above 4 GiB simply saturates the reservation.)
///
/// `None` when the consumer-gone cancellation fires while waiting. Semaphore
/// acquisition is cancel-safe, and the guard exists from the FIRST
/// acquisition, so a cancellation before the global one still returns the
/// per-response permits.
async fn reserve_chunk(pipeline: &DetachPipeline, chunk_len: usize) -> Option<ChunkReservations> {
    let per_response_permits = permits_for(chunk_len, pipeline.per_response_cap);
    let budget_permits = permits_for(chunk_len, pipeline.budget.total_usize());
    // `cancelled()` needs `&mut`; a cloned receiver observes the same channel.
    let mut cancel = pipeline.cancel.clone();
    let per_response = match pipeline.per_response.try_acquire_many(per_response_permits) {
        Ok(permit) => {
            hold_permit(permit);
            Arc::clone(&pipeline.per_response)
        }
        Err(_) => {
            // The per-response cap is already buffered: wait for the
            // forwarder to free capacity (backpressure) — or for the
            // consumer to be lost (cancellation).
            pipeline.budget.record_fallback_cap();
            let acquired = tokio::select! {
                acquired = pipeline.per_response.acquire_many(per_response_permits) => {
                    Some(acquired.expect("only fails on task cancellation"))
                }
                _ = cancel.wait_for(|v| *v) => None,
            };
            let Some(permit) = acquired else {
                // Consumer gone: the reader finishes without enqueueing.
                return None;
            };
            hold_permit(permit);
            Arc::clone(&pipeline.per_response)
        }
    };
    // Guard from the FIRST acquisition: if the global acquisition is
    // cancelled, dropping it still returns the per-response permits.
    let mut guard = ChunkReservations {
        per_response,
        per_response_permits,
        budget: Arc::clone(&pipeline.budget),
        budget_permits: 0,
    };
    match pipeline.budget.permits().try_acquire_many(budget_permits) {
        Ok(permit) => hold_permit(permit),
        Err(_) => {
            // The process-wide budget is already buffered: wait for ANY
            // forwarder to free capacity (backpressure) — or for the
            // consumer to be lost (cancellation).
            pipeline.budget.record_fallback_budget();
            let acquired = tokio::select! {
                acquired = pipeline.budget.permits().acquire_many(budget_permits) => {
                    Some(acquired.expect("only fails on task cancellation"))
                }
                _ = cancel.wait_for(|v| *v) => None,
            };
            let Some(permit) = acquired else {
                // Cancelled: the guard drops here, returning the
                // per-response permits (budget permits: 0).
                return None;
            };
            hold_permit(permit);
        }
    }
    guard.budget_permits = budget_permits;
    Some(guard)
}

/// Permit count for a `len`-byte chunk under a `cap`-byte capacity: the
/// chunk size, the full capacity when the chunk is oversized, saturated to
/// the `u32` semaphore width.
fn permits_for(len: usize, cap: usize) -> u32 {
    len.min(cap).min(u32::MAX as usize) as u32
}

/// One response's detach-pipeline context, shared by the reader and the
/// forwarder tasks: the per-response semaphore (and its capacity), the
/// shared process-wide budget, the consumer-gone cancellation that the
/// reader's reservation waits select on (so a lost consumer can never leave
/// the reader parked on a semaphore), and the abandon-drain policy (EDG-9)
/// that decides what the reader does once the consumer is gone.
#[derive(Clone)]
struct DetachPipeline {
    per_response: Arc<tokio::sync::Semaphore>,
    per_response_cap: usize,
    budget: Arc<StreamDetachBudget>,
    cancel: watch::Receiver<bool>,
    abandon_drain: AbandonDrain,
    max_duration: Option<StreamDuration>,
}

#[derive(Clone)]
struct StreamDuration {
    started: TokioInstant,
    limit: Duration,
    elapsed_ms: Arc<AtomicU64>,
    budget: Arc<StreamDetachBudget>,
    drain: AbandonDrain,
}

enum StreamFrameRead {
    Frame(Result<Vec<u8>, StreamFrameReadError>),
    MaxDuration {
        started: TokioInstant,
        cancel_written: bool,
        frame: Result<Vec<u8>, StreamFrameReadError>,
    },
}

enum StreamFrameReadError {
    Timeout,
    Io(std::io::Error),
}

fn enabled_stream_max_duration(limit: Option<Duration>) -> Option<Duration> {
    limit.filter(|duration| !duration.is_zero())
}

/// A streamed response from the worker process: status/headers up front, body
/// chunks delivered through the channel as the worker produces them.
pub struct ProcessStreamedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub chunks: mpsc::Receiver<Result<Bytes, IsolationError>>,
    /// Production-complete signal (see `StreamedResponse::completed`); `None`
    /// when neither detach nor max-duration tracking is configured.
    pub completed: Option<CompletionSignal>,
    /// Shared production-complete flag (see
    /// `StreamedResponse::production_complete`); `None` when neither detach
    /// nor max-duration tracking is configured.
    pub production_complete: Option<Arc<StreamProductionState>>,
    pub max_duration_elapsed_ms: Option<Arc<AtomicU64>>,
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
    /// Shared write half (EDG-9 slice 2): the request writer, the shutdown
    /// writer and the abandon drain (which writes the cancel control frame
    /// from the reader task) all serialize on this mutex.
    write_half: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
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
            NodeHttpMode::Capture,
            true,
            None,
            DenoCacheMode::default(),
            None,
            None,
            None,
        )
        .await
    }

    /// Spawn with a real node:http server bound to a private Unix socket.
    /// Heavy frameworks that require genuine IncomingMessage/ServerResponse
    /// semantics use this path; no TCP port or network permission is opened.
    pub async fn spawn_with_node_http_proxy(
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
            NodeHttpMode::Proxy,
            true,
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
            NodeHttpMode::Capture,
            true,
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
        node_http_mode: NodeHttpMode,
        bundle_entrypoint: bool,
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
        let (entry_url, bundle_dir) =
            if bundle_entrypoint && entry_needs_bundle(&worker_dir, &entry)? {
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
        for arg in deno_network_permission_args_with_uds(
            allow_net,
            std::env::var("EDGER_DENO_ALLOW_NET").ok().as_deref(),
            &socket_path,
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
            .arg(node_http_mode.as_arg())
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
            write_half: Arc::new(tokio::sync::Mutex::new(write_half)),
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
    /// the end frame (story 16.D). Legacy behavior: no detach buffer.
    pub async fn request_stream(
        &mut self,
        req: SerializedRequest,
    ) -> Result<ProcessStreamedResponse, IsolationError> {
        self.request_stream_with_detach(req, None).await
    }

    /// Streaming request with an optional stream-detach policy (EDG-8): chunks
    /// flow through a single FIFO pipeline — the reader reserves each chunk's
    /// bytes (per-response cap + shared process-wide budget) before enqueueing
    /// and waits when no permits are free (backpressure keeps the slot held);
    /// the forwarder delivers chunks in order and releases the permits after
    /// delivery. A policy of `0` disables the pipeline entirely (legacy
    /// blocking path, no signal). The returned `completed` signal resolves to
    /// `true` when production finished cleanly.
    pub async fn request_stream_with_detach(
        &mut self,
        req: SerializedRequest,
        detach: Option<&StreamDetach>,
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
            let mut write_half = self.write_half.lock().await;
            tokio::time::timeout(self.timeout, write_frame(&mut *write_half, &payload))
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
        let stream_started = TokioInstant::now();
        let max_duration =
            enabled_stream_max_duration(detach.and_then(|policy| policy.max_duration));
        let max_duration_elapsed_ms = max_duration.map(|_| Arc::new(AtomicU64::new(0)));
        let duration_context = max_duration.map(|limit| StreamDuration {
            started: stream_started,
            limit,
            elapsed_ms: Arc::clone(
                max_duration_elapsed_ms
                    .as_ref()
                    .expect("max duration has an elapsed-time recorder"),
            ),
            budget: Arc::clone(
                &detach
                    .expect("max duration requires its stream policy")
                    .budget,
            ),
            drain: detach
                .expect("max duration requires its stream policy")
                .drain,
        });

        // Detach pipeline (EDG-8): a policy of `0` (or none) disables it
        // entirely — no queue or semaphores. A max-duration policy still
        // gives the legacy reader a completion signal so it can cancel and
        // drain at the total-duration cutoff.
        let (completed, production_complete) = match detach.filter(|policy| policy.max_bytes > 0) {
            Some(policy) => {
                let per_response_cap = policy.max_bytes.min(usize::MAX as u64) as usize;
                let (q_tx, q_rx) = mpsc::unbounded_channel::<QueueItem>();
                // Consumer-gone cancellation: the forwarder fires it (and
                // closes the queue) when the body is dropped mid-stream, so
                // a reader parked on a reservation semaphore is woken and
                // finishes (killing the process/socket cannot wake it).
                let (cancel_tx, cancel_rx) = watch::channel(false);
                let pipeline = DetachPipeline {
                    per_response: Arc::new(tokio::sync::Semaphore::new(per_response_cap)),
                    per_response_cap,
                    budget: Arc::clone(&policy.budget),
                    cancel: cancel_rx,
                    abandon_drain: policy.drain,
                    max_duration: duration_context.clone(),
                };
                let production_complete = Arc::new(StreamProductionState::default());
                let (done_tx, done_rx) = oneshot::channel::<StreamCompletion>();

                // Reader: reads frames, reserves the chunk's bytes (waits
                // when no permits are free — the slot stays held), and
                // enqueues it on the single FIFO queue. On a consumer loss
                // it runs the abandon drain, which shares this writer (the
                // EDG-9 slice 2 cancel control frame).
                tokio::spawn(Self::detach_reader(
                    read_half,
                    Arc::clone(&self.write_half),
                    q_tx,
                    pipeline.clone(),
                    restore_tx,
                    production_complete.clone(),
                    done_tx,
                    frame_timeout,
                ));
                // Forwarder: takes chunks from the queue IN ORDER and
                // sends them on the body channel; the RAII reservations in
                // the queue items return the permits on delivery or
                // discard. On a lost consumer it cancels the reader's
                // reservation waits and closes the queue, then drains.
                tokio::spawn(Self::detach_forwarder(q_rx, tx, cancel_tx));

                (
                    Some(Box::pin(async move {
                        // The reader sends the CAUSE on every abandon-drain
                        // exit (EDG-9) and `Completed` on a clean end;
                        // the fallback covers an abnormal task exit that
                        // dropped the sender without reporting (no known
                        // cause: the pool recycles with what it has).
                        done_rx.await.unwrap_or(StreamCompletion::Incomplete)
                    }) as CompletionSignal),
                    Some(production_complete),
                )
            }
            None => {
                let production_complete = duration_context
                    .as_ref()
                    .map(|_| Arc::new(StreamProductionState::default()));
                let (done_tx, done_rx) = if duration_context.is_some() {
                    let (tx, rx) = oneshot::channel::<StreamCompletion>();
                    (Some(tx), Some(rx))
                } else {
                    (None, None)
                };
                tokio::spawn(Self::legacy_stream_pump(
                    read_half,
                    tx,
                    restore_tx,
                    frame_timeout,
                    duration_context,
                    production_complete.clone(),
                    done_tx,
                    Arc::clone(&self.write_half),
                ));
                let completed = done_rx.map(|done_rx| {
                    Box::pin(async move { done_rx.await.unwrap_or(StreamCompletion::Incomplete) })
                        as CompletionSignal
                });
                (completed, production_complete)
            }
        };

        Ok(ProcessStreamedResponse {
            status: header.status,
            headers: header.headers,
            chunks: rx,
            completed,
            production_complete,
            max_duration_elapsed_ms,
        })
    }

    async fn read_stream_frame(
        read_half: &mut OwnedReadHalf,
        write_half: &Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
        frame_timeout: Duration,
        max_duration: Option<&StreamDuration>,
    ) -> StreamFrameRead {
        let Some(max_duration) = max_duration else {
            return match tokio::time::timeout(frame_timeout, read_frame(read_half)).await {
                Ok(Ok(frame)) => StreamFrameRead::Frame(Ok(frame)),
                Ok(Err(err)) => StreamFrameRead::Frame(Err(StreamFrameReadError::Io(err))),
                Err(_) => StreamFrameRead::Frame(Err(StreamFrameReadError::Timeout)),
            };
        };

        let deadline = max_duration.started + max_duration.limit;
        let mut read = Box::pin(read_frame(read_half));
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                let started = TokioInstant::now();
                max_duration.elapsed_ms.store(
                    started.duration_since(max_duration.started).as_millis() as u64,
                    Ordering::Release,
                );
                if !max_duration.drain.enabled() {
                    return StreamFrameRead::MaxDuration {
                        started,
                        cancel_written: false,
                        frame: Err(StreamFrameReadError::Timeout),
                    };
                }

                let cancel_payload = serde_json::to_vec(&WireCancel { control: "cancel" })
                    .expect("static cancel frame serializes");
                let write_budget = Duration::from_millis(max_duration.drain.max_ms)
                    .min(frame_timeout);
                let cancel_written = matches!(
                    tokio::time::timeout(write_budget, async {
                        let mut writer = write_half.lock().await;
                        write_frame(&mut *writer, &cancel_payload).await
                    })
                    .await,
                    Ok(Ok(()))
                );
                if !cancel_written {
                    return StreamFrameRead::MaxDuration {
                        started,
                        cancel_written,
                        frame: Err(StreamFrameReadError::Timeout),
                    };
                }

                let time_remaining = Duration::from_millis(max_duration.drain.max_ms)
                    .saturating_sub(started.elapsed())
                    .min(frame_timeout);
                let frame = match tokio::time::timeout(time_remaining, &mut read).await {
                    Ok(Ok(frame)) => Ok(frame),
                    Ok(Err(err)) => Err(StreamFrameReadError::Io(err)),
                    Err(_) => Err(StreamFrameReadError::Timeout),
                };
                StreamFrameRead::MaxDuration {
                    started,
                    cancel_written,
                    frame,
                }
            }
            result = tokio::time::timeout(frame_timeout, &mut read) => {
                match result {
                    Ok(Ok(frame)) => StreamFrameRead::Frame(Ok(frame)),
                    Ok(Err(err)) => StreamFrameRead::Frame(Err(StreamFrameReadError::Io(err))),
                    Err(_) => StreamFrameRead::Frame(Err(StreamFrameReadError::Timeout)),
                }
            }
        }
    }

    fn report_drain_outcome(
        budget: &StreamDetachBudget,
        origin: DrainOrigin,
        outcome: AbandonedStream,
        done_tx: oneshot::Sender<StreamCompletion>,
    ) {
        budget.record_drain_outcome(origin, outcome);
        let completion = match origin {
            DrainOrigin::ClientGone if outcome == AbandonedStream::Drained => {
                StreamCompletion::Completed
            }
            DrainOrigin::ClientGone => StreamCompletion::Abandoned(outcome),
            DrainOrigin::MaxDuration => StreamCompletion::MaxDuration(outcome),
        };
        let _ = done_tx.send(completion);
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_max_duration_frame(
        frame: Vec<u8>,
        read_half: OwnedReadHalf,
        write_half: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
        budget: Arc<StreamDetachBudget>,
        drain: AbandonDrain,
        restore_tx: oneshot::Sender<OwnedReadHalf>,
        production_complete: Arc<StreamProductionState>,
        done_tx: oneshot::Sender<StreamCompletion>,
        frame_timeout: Duration,
        started: TokioInstant,
        pre_discarded: u64,
        detach_active: bool,
    ) {
        let Ok((tag, body)) = split_tag(&frame) else {
            Self::report_drain_outcome(
                &budget,
                DrainOrigin::MaxDuration,
                AbandonedStream::StreamError,
                done_tx,
            );
            return;
        };
        match tag {
            TAG_CHUNK => {
                Self::drain_on_abandon(
                    read_half,
                    write_half,
                    budget,
                    drain,
                    restore_tx,
                    production_complete,
                    done_tx,
                    frame_timeout,
                    pre_discarded.saturating_add(body.len() as u64),
                    DrainOrigin::MaxDuration,
                    started,
                    true,
                    detach_active,
                )
                .await;
            }
            TAG_END => {
                let end: WireEndFrame = serde_json::from_slice(body).unwrap_or_default();
                let outcome = if end.error.is_some() {
                    AbandonedStream::StreamError
                } else if end.cancelled {
                    AbandonedStream::Cancelled
                } else {
                    AbandonedStream::Drained
                };
                if matches!(
                    outcome,
                    AbandonedStream::Drained | AbandonedStream::Cancelled
                ) {
                    let _ = restore_tx.send(read_half);
                    production_complete.mark_max_duration_complete(outcome);
                    if detach_active {
                        budget.record_detached();
                    }
                }
                Self::report_drain_outcome(&budget, DrainOrigin::MaxDuration, outcome, done_tx);
            }
            _ => Self::report_drain_outcome(
                &budget,
                DrainOrigin::MaxDuration,
                AbandonedStream::StreamError,
                done_tx,
            ),
        }
    }

    /// Detach-pipeline reader task (EDG-8): reads frames from the worker
    /// socket and enqueues each chunk on the single FIFO queue AFTER
    /// reserving its bytes on the per-response and the global semaphores.
    /// When no permits are free it WAITS (that wait is the backpressure: the
    /// worker slot stays held, exactly like the legacy path) — production
    /// cannot outrun the budget. A clean end frame restores the read half,
    /// sets the shared production-complete flag, enqueues the terminal
    /// marker and only then fires the completion signal.
    ///
    /// When the consumer is lost BEFORE the end frame (EDG-9), the reader
    /// enters discard mode (`drain_on_abandon`) instead of abandoning the
    /// socket: it keeps reading and discarding frames until the clean
    /// `TAG_END` (bounded by the drain limits), so the process can be
    /// reused instead of poisoned.
    #[allow(clippy::too_many_arguments)]
    async fn detach_reader(
        mut read_half: OwnedReadHalf,
        write_half: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
        q_tx: mpsc::UnboundedSender<QueueItem>,
        pipeline: DetachPipeline,
        restore_tx: oneshot::Sender<OwnedReadHalf>,
        production_complete: Arc<StreamProductionState>,
        done_tx: oneshot::Sender<StreamCompletion>,
        frame_timeout: Duration,
    ) {
        loop {
            // Cancellation check BETWEEN frames (frame reads are NOT
            // cancel-safe, so they never enter the select): the consumer is
            // gone. (EDG-9) Do not abandon the socket mid-response — drain
            // the rest of the response in discard mode so the process can
            // be reused. No chunk has been read at the loop top: nothing
            // is pre-discarded.
            if *pipeline.cancel.borrow() {
                return Self::drain_on_abandon(
                    read_half,
                    write_half,
                    Arc::clone(&pipeline.budget),
                    pipeline.abandon_drain,
                    restore_tx,
                    production_complete,
                    done_tx,
                    frame_timeout,
                    0,
                    DrainOrigin::ClientGone,
                    TokioInstant::now(),
                    false,
                    true,
                )
                .await;
            }
            let frame = match Self::read_stream_frame(
                &mut read_half,
                &write_half,
                frame_timeout,
                pipeline.max_duration.as_ref(),
            )
            .await
            {
                StreamFrameRead::MaxDuration {
                    started,
                    cancel_written,
                    frame,
                } => {
                    if !pipeline.abandon_drain.enabled() {
                        Self::report_drain_outcome(
                            &pipeline.budget,
                            DrainOrigin::MaxDuration,
                            AbandonedStream::SocketPoisoned,
                            done_tx,
                        );
                        return;
                    }
                    if !cancel_written {
                        Self::report_drain_outcome(
                            &pipeline.budget,
                            DrainOrigin::MaxDuration,
                            AbandonedStream::SocketPoisoned,
                            done_tx,
                        );
                        return;
                    }
                    match frame {
                        Ok(frame) => {
                            Self::finish_max_duration_frame(
                                frame,
                                read_half,
                                write_half,
                                Arc::clone(&pipeline.budget),
                                pipeline.abandon_drain,
                                restore_tx,
                                Arc::clone(&production_complete),
                                done_tx,
                                frame_timeout,
                                started,
                                0,
                                true,
                            )
                            .await;
                        }
                        Err(StreamFrameReadError::Timeout) => Self::report_drain_outcome(
                            &pipeline.budget,
                            DrainOrigin::MaxDuration,
                            AbandonedStream::TimeLimit,
                            done_tx,
                        ),
                        Err(StreamFrameReadError::Io(_)) => Self::report_drain_outcome(
                            &pipeline.budget,
                            DrainOrigin::MaxDuration,
                            AbandonedStream::StreamError,
                            done_tx,
                        ),
                    }
                    return;
                }
                StreamFrameRead::Frame(Err(StreamFrameReadError::Io(err))) => {
                    // Abnormal: surface the error IN ORDER (the forwarder
                    // delivers it) and drop the read half — poisoned.
                    let _ = q_tx.send(QueueItem::End(Err(IsolationError::new(
                        "UDS_IO",
                        format!("stream read failed: {err}"),
                    ))));
                    return;
                }
                StreamFrameRead::Frame(Err(StreamFrameReadError::Timeout)) => {
                    let _ = q_tx.send(QueueItem::End(Err(IsolationError::new(
                        "UDS_TIMEOUT",
                        "stream stalled past the frame timeout",
                    ))));
                    return; // abnormal
                }
                StreamFrameRead::Frame(Ok(frame)) => frame,
            };
            let Ok((tag, body)) = split_tag(&frame) else {
                return; // abnormal: empty frame — the queue close ends the body
            };
            match tag {
                TAG_CHUNK => {
                    let chunk = Bytes::copy_from_slice(body);
                    // Reserve the chunk's bytes BEFORE enqueueing; the RAII
                    // guard rides along in the queue item and returns both
                    // reservations when the item is dropped (delivery,
                    // discard, or early exit). `None` means the consumer
                    // was lost while reserving: discard this chunk and
                    // drain the rest of the response (EDG-9). The chunk
                    // is ALREADY READ and discarded here — count it toward
                    // the drain's byte budget from the start (captured
                    // before the chunk is moved below).
                    let discarded = chunk.len();
                    let Some(reservations) = reserve_chunk(&pipeline, discarded).await else {
                        return Self::drain_on_abandon(
                            read_half,
                            write_half,
                            Arc::clone(&pipeline.budget),
                            pipeline.abandon_drain,
                            restore_tx,
                            production_complete,
                            done_tx,
                            frame_timeout,
                            discarded as u64,
                            DrainOrigin::ClientGone,
                            TokioInstant::now(),
                            false,
                            true,
                        )
                        .await;
                    };
                    if q_tx
                        .send(QueueItem::Chunk {
                            chunk,
                            reservations,
                        })
                        .is_err()
                    {
                        // Queue closed: the forwarder only closes it after
                        // firing the consumer-gone cancel (it exits on the
                        // terminal marker otherwise, and this reader is
                        // still alive). Consumer lost → drain the rest
                        // (EDG-9); anything else keeps the pre-EDG-9 exit.
                        if *pipeline.cancel.borrow() {
                            // The chunk was read but the queue send failed:
                            // it is discarded here — count it toward the
                            // drain's byte budget from the start.
                            return Self::drain_on_abandon(
                                read_half,
                                write_half,
                                Arc::clone(&pipeline.budget),
                                pipeline.abandon_drain,
                                restore_tx,
                                production_complete,
                                done_tx,
                                frame_timeout,
                                discarded as u64,
                                DrainOrigin::ClientGone,
                                TokioInstant::now(),
                                false,
                                true,
                            )
                            .await;
                        }
                        // Frames for THIS response are still in flight, so
                        // the socket cannot be reused: finish without
                        // restoring.
                        return;
                    }
                }
                TAG_END => {
                    let end: WireEndFrame = serde_json::from_slice(body).unwrap_or_default();
                    if let Some(error) = end.error {
                        // In-band production error: restore the read half (the
                        // end frame itself was clean) and pass the error
                        // marker through in order. No success flag/signal: the
                        // pool recycles as it does today.
                        let _ = restore_tx.send(read_half);
                        let _ = q_tx.send(QueueItem::End(Err(IsolationError::new(
                            "UDS_STREAM",
                            error,
                        ))));
                        return;
                    }
                    if end.cancelled {
                        // (EDG-9 slice 2) A cancel end frame OUTSIDE the
                        // abandon drain is an unexpected protocol state — the
                        // orchestrator only cancels while draining. The
                        // socket cannot be trusted: do not restore and pass
                        // the error marker through in order (recycle).
                        let _ = q_tx.send(QueueItem::End(Err(IsolationError::new(
                            "UDS_PROTOCOL",
                            "unexpected cancel end frame outside the abandon drain",
                        ))));
                        return;
                    }
                    // Clean end: production is DONE. (1) hand back the read
                    // half (the process is reusable), (2) mark the shared
                    // production-complete flag, (3) enqueue the terminal
                    // marker, (4) fire the completion signal. The flag is set
                    // BEFORE the signal so a body drop racing the signal
                    // observer can never recycle a socket that is in sync and
                    // reusable.
                    let _ = restore_tx.send(read_half);
                    production_complete.mark_complete();
                    pipeline.budget.record_detached();
                    let _ = q_tx.send(QueueItem::End(Ok(())));
                    let _ = done_tx.send(StreamCompletion::Completed);
                    return;
                }
                _ => return, // abnormal: unknown tag — the queue close ends the body
            }
        }
    }

    /// Abandon drain (EDG-9): the consumer disappeared before `TAG_END`.
    /// Instead of abandoning the socket mid-response (which poisons the
    /// process and forces a cold start on the next request), keep reading
    /// frames and DISCARD them — no budget reservation, no enqueue — until
    /// the clean `TAG_END`, bounded by the byte and time limits.
    ///
    /// (EDG-9 slice 2) BEFORE the discard loop, the drain writes the CANCEL
    /// control frame (`{"__control":"cancel"}`) on the shared write half
    /// (bounded by the REMAINING drain time budget): the harness aborts the
    /// request's `AbortSignal`, cancels the body and answers with a `TAG_END`
    /// carrying `{"cancelled":true}` — a clean end for the drain even when
    /// the stream is endless (SSE), so the process is reused instead of
    /// recycled at the time limit. A write that fails or stalls past the
    /// budget recycles with the `socket_poisoned` sub-cause. The cancel is
    /// sent for EVERY abandon (finite and endless responses alike) — only a
    /// DISABLED drain skips it.
    ///
    /// `pre_discarded` is the size of the chunk the reader already read and
    /// discarded BEFORE entering this drain (the chunk whose reservation or
    /// queue send failed): it counts toward `max_bytes` from the start, so
    /// the byte limit is applied before a following `TAG_END` can be
    /// accepted when the budget is already spent.
    ///
    /// A clean `TAG_END` within the limits restores the read half and sets
    /// the production-complete flag. Client-abandon drains complete as
    /// `Completed`; max-duration drains carry their own outcome. Past a
    /// limit, a read error or an end frame with an error the drain stops
    /// WITHOUT restoring (the socket is desynced) and reports the cause so
    /// the pool recycles. A `0` in any limit disables the drain and abandons
    /// the socket, reporting `SocketPoisoned`.
    #[allow(clippy::too_many_arguments)]
    async fn drain_on_abandon(
        mut read_half: OwnedReadHalf,
        write_half: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
        budget: Arc<StreamDetachBudget>,
        drain: AbandonDrain,
        restore_tx: oneshot::Sender<OwnedReadHalf>,
        production_complete: Arc<StreamProductionState>,
        done_tx: oneshot::Sender<StreamCompletion>,
        frame_timeout: Duration,
        pre_discarded: u64,
        origin: DrainOrigin,
        started: TokioInstant,
        cancel_already_sent: bool,
        detach_active: bool,
    ) {
        if !drain.enabled() {
            // A disabled drain leaves the socket mid-response and recycles
            // the process. No cancel frame is written.
            tracing::info!(
                target: "edger.stream",
                max_bytes = drain.max_bytes,
                max_ms = drain.max_ms,
                "stream drain disabled; socket poisoned, process will be recycled"
            );
            Self::report_drain_outcome(&budget, origin, AbandonedStream::SocketPoisoned, done_tx);
            return;
        }
        let time_budget = Duration::from_millis(drain.max_ms);
        // (EDG-9 slice 2) Tell the harness to cancel the in-flight response
        // NOW, before the discard loop starts: an endless stream (SSE) can
        // never reach TAG_END on its own, so without the cancel the drain
        // would always stop at the time limit and recycle. The write is
        // bounded by the REMAINING drain time budget (short): a stalled
        // socket cannot hold the writer — and the pool's relay wait — open
        // past the budget.
        if !cancel_already_sent {
            let write_budget = time_budget
                .saturating_sub(started.elapsed())
                .min(frame_timeout);
            let cancel_payload = serde_json::to_vec(&WireCancel { control: "cancel" })
                .expect("static cancel frame serializes");
            match tokio::time::timeout(write_budget, async {
                let mut write_half = write_half.lock().await;
                write_frame(&mut *write_half, &cancel_payload).await
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => {
                    Self::report_drain_outcome(
                        &budget,
                        origin,
                        AbandonedStream::SocketPoisoned,
                        done_tx,
                    );
                    return;
                }
            }
        }
        // The pre-discarded chunk counts toward the byte budget from the
        // start: the limit is checked (and can stop the drain) before the
        // next frame — a `TAG_END` included — is accepted.
        let mut discarded = pre_discarded;
        loop {
            let elapsed = started.elapsed();
            if elapsed >= time_budget {
                // Time limit: the response did not finish in budget.
                tracing::info!(
                    target: "edger.stream",
                    discarded_bytes = discarded,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "abandon drain stopped at the time limit; socket poisoned, process will be recycled"
                );
                Self::report_drain_outcome(&budget, origin, AbandonedStream::TimeLimit, done_tx);
                return;
            }
            if discarded > drain.max_bytes {
                // Byte limit: more of the response was in flight (or
                // pre-discarded) than the drain may discard.
                tracing::info!(
                    target: "edger.stream",
                    discarded_bytes = discarded,
                    max_bytes = drain.max_bytes,
                    "abandon drain stopped at the byte limit; socket poisoned, process will be recycled"
                );
                Self::report_drain_outcome(&budget, origin, AbandonedStream::BytesLimit, done_tx);
                return;
            }
            // Bound every frame read by the REMAINING drain budget as well as
            // the process frame timeout: a producer that pauses past the
            // budget must not hold the socket (and the pool's wait) open.
            let read_deadline = time_budget.saturating_sub(elapsed).min(frame_timeout);
            let frame = match tokio::time::timeout(read_deadline, read_frame(&mut read_half)).await
            {
                Ok(Ok(frame)) => frame,
                Ok(Err(_)) => {
                    // Read error: the socket is desynced — recycle.
                    tracing::info!(
                        target: "edger.stream",
                        discarded_bytes = discarded,
                        "abandon drain stopped on a stream read error; socket poisoned, process will be recycled"
                    );
                    Self::report_drain_outcome(
                        &budget,
                        origin,
                        AbandonedStream::StreamError,
                        done_tx,
                    );
                    return;
                }
                Err(_) => {
                    // The frame stalled or the drain time budget ran out
                    // while reading — either way the response did not end
                    // in time: recycle.
                    tracing::info!(
                        target: "edger.stream",
                        discarded_bytes = discarded,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "abandon drain stopped at the time limit; socket poisoned, process will be recycled"
                    );
                    Self::report_drain_outcome(
                        &budget,
                        origin,
                        AbandonedStream::TimeLimit,
                        done_tx,
                    );
                    return;
                }
            };
            let Ok((tag, body)) = split_tag(&frame) else {
                // Protocol error (empty frame): the socket is desynced.
                Self::report_drain_outcome(&budget, origin, AbandonedStream::StreamError, done_tx);
                return;
            };
            match tag {
                TAG_CHUNK => {
                    discarded = discarded.saturating_add(body.len() as u64);
                }
                TAG_END => {
                    let end: WireEndFrame = serde_json::from_slice(body).unwrap_or_default();
                    if end.error.is_some() {
                        // The response itself ended in error: recycle.
                        Self::report_drain_outcome(
                            &budget,
                            origin,
                            AbandonedStream::StreamError,
                            done_tx,
                        );
                        return;
                    }
                    if end.cancelled {
                        // (EDG-9 slice 2) The harness acknowledged the cancel
                        // control frame: the body was aborted and this end is
                        // CLEAN even though the stream never reached its
                        // natural end — restore the read half, set the
                        // production-complete flag and report the `cancelled`
                        // cause: the pool reuses the process (the dispatch
                        // completes with the `stream_abandoned_drained` reason
                        // and the `cancelled` sub-cause).
                        let _ = restore_tx.send(read_half);
                        if origin == DrainOrigin::MaxDuration {
                            production_complete
                                .mark_max_duration_complete(AbandonedStream::Cancelled);
                        } else {
                            production_complete.mark_complete();
                        }
                        if detach_active {
                            budget.record_detached();
                        }
                        tracing::info!(
                            target: "edger.stream",
                            discarded_bytes = discarded,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "abandon drain reached the cancel end within the limits; process reused"
                        );
                        Self::report_drain_outcome(
                            &budget,
                            origin,
                            AbandonedStream::Cancelled,
                            done_tx,
                        );
                        return;
                    }
                    // Clean end WITHIN the limits: the socket is in sync —
                    // reuse the process exactly as on a normal clean end.
                    let _ = restore_tx.send(read_half);
                    if origin == DrainOrigin::MaxDuration {
                        production_complete.mark_max_duration_complete(AbandonedStream::Drained);
                    } else {
                        production_complete.mark_complete();
                    }
                    if detach_active {
                        budget.record_detached();
                    }
                    tracing::info!(
                        target: "edger.stream",
                        discarded_bytes = discarded,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "abandon drain reached a clean end within the limits; process reused"
                    );
                    Self::report_drain_outcome(&budget, origin, AbandonedStream::Drained, done_tx);
                    return;
                }
                _ => {
                    // Protocol error (unknown tag): the socket is desynced.
                    Self::report_drain_outcome(
                        &budget,
                        origin,
                        AbandonedStream::StreamError,
                        done_tx,
                    );
                    return;
                }
            }
        }
    }

    /// Detach-pipeline forwarder task (EDG-8): takes chunks from the single
    /// FIFO queue IN ORDER and sends them on the body channel, releasing the
    /// chunk's permits only AFTER delivery (or discard) — via the RAII
    /// `ChunkReservations` guard, so every path returns the permits. It runs
    /// continuously, so already-read chunks reach the consumer as soon as it
    /// has capacity — without waiting for the next frame or the terminal
    /// marker.
    ///
    /// On a LOST CONSUMER it fires the shared cancellation (waking a reader
    /// parked on a reservation semaphore), closes the queue (ending any
    /// reader that still tries to send) and then drains what is left
    /// (RAII returns every reservation).
    async fn detach_forwarder(
        mut q_rx: mpsc::UnboundedReceiver<QueueItem>,
        tx: mpsc::Sender<Result<Bytes, IsolationError>>,
        cancel_tx: watch::Sender<bool>,
    ) {
        loop {
            let item = match q_rx.recv().await {
                Some(item) => item,
                None => break, // reader gone (abnormal): drop `tx`, end the body
            };
            match item {
                QueueItem::Chunk {
                    chunk,
                    reservations,
                } => {
                    if tx.send(Ok(chunk)).await.is_err() {
                        // Consumer gone (client disconnect): return THIS
                        // chunk's reservations FIRST, then cancel the
                        // reader's reservation waits and close the queue —
                        // the pool recycles the process, but killing the
                        // socket cannot wake a semaphore wait, so the
                        // cancellation is the reader's only wake-up. Then
                        // discard what is left; every dropped item returns
                        // its own reservations (RAII), and `close` makes
                        // `recv` return None once the queue is drained.
                        drop(reservations);
                        let _ = cancel_tx.send_replace(true);
                        q_rx.close();
                        while let Some(QueueItem::Chunk { .. }) = q_rx.recv().await {
                            // Dropped at the end of each iteration: its
                            // `ChunkReservations` guard returns the permits.
                        }
                        break;
                    }
                    // Delivered: the permits return (RAII drop).
                    drop(reservations);
                }
                QueueItem::End(Ok(())) => break, // drop `tx` → body ends (None)
                QueueItem::End(Err(err)) => {
                    let _ = tx.send(Err(err)).await;
                    break;
                }
            }
        }
    }

    /// Legacy stream pump (pre-EDG-8): one task reads frames and blocking
    /// sends chunks on the body channel; the worker slot stays held until the
    /// body is fully consumed or dropped.
    #[allow(clippy::too_many_arguments)]
    async fn legacy_stream_pump(
        mut read_half: OwnedReadHalf,
        tx: mpsc::Sender<Result<Bytes, IsolationError>>,
        restore_tx: oneshot::Sender<OwnedReadHalf>,
        frame_timeout: Duration,
        max_duration: Option<StreamDuration>,
        production_complete: Option<Arc<StreamProductionState>>,
        done_tx: Option<oneshot::Sender<StreamCompletion>>,
        write_half: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    ) {
        loop {
            let frame = match Self::read_stream_frame(
                &mut read_half,
                &write_half,
                frame_timeout,
                max_duration.as_ref(),
            )
            .await
            {
                StreamFrameRead::MaxDuration {
                    started,
                    cancel_written,
                    frame,
                } => {
                    let Some(max_duration) = max_duration.as_ref() else {
                        return;
                    };
                    let Some(done_tx) = done_tx else {
                        return;
                    };
                    if !max_duration.drain.enabled() || !cancel_written {
                        Self::report_drain_outcome(
                            &max_duration.budget,
                            DrainOrigin::MaxDuration,
                            AbandonedStream::SocketPoisoned,
                            done_tx,
                        );
                        return;
                    }
                    match frame {
                        Ok(frame) => {
                            Self::finish_max_duration_frame(
                                frame,
                                read_half,
                                write_half,
                                Arc::clone(&max_duration.budget),
                                max_duration.drain,
                                restore_tx,
                                production_complete
                                    .as_ref()
                                    .expect("max duration has a production-complete flag")
                                    .clone(),
                                done_tx,
                                frame_timeout,
                                started,
                                0,
                                false,
                            )
                            .await;
                        }
                        Err(StreamFrameReadError::Timeout) => Self::report_drain_outcome(
                            &max_duration.budget,
                            DrainOrigin::MaxDuration,
                            AbandonedStream::TimeLimit,
                            done_tx,
                        ),
                        Err(StreamFrameReadError::Io(_)) => Self::report_drain_outcome(
                            &max_duration.budget,
                            DrainOrigin::MaxDuration,
                            AbandonedStream::StreamError,
                            done_tx,
                        ),
                    }
                    return;
                }
                StreamFrameRead::Frame(Err(StreamFrameReadError::Io(err))) => {
                    let _ = tx
                        .send(Err(IsolationError::new(
                            "UDS_IO",
                            format!("stream read failed: {err}"),
                        )))
                        .await;
                    return; // abnormal: read half dropped, process poisoned
                }
                StreamFrameRead::Frame(Err(StreamFrameReadError::Timeout)) => {
                    let _ = tx
                        .send(Err(IsolationError::new(
                            "UDS_TIMEOUT",
                            "stream stalled past the frame timeout",
                        )))
                        .await;
                    return; // abnormal
                }
                StreamFrameRead::Frame(Ok(frame)) => frame,
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
                        let _ = restore_tx.send(read_half); // clean end: reusable
                        return;
                    }
                    if end.cancelled {
                        // (EDG-9 slice 2) A cancel end frame on the LEGACY
                        // path is an unexpected protocol state (the legacy
                        // path never cancels): surface the error and do not
                        // restore the read half (poisoned).
                        let _ = tx
                            .send(Err(IsolationError::new(
                                "UDS_PROTOCOL",
                                "unexpected cancel end frame",
                            )))
                            .await;
                        return;
                    }
                    let _ = restore_tx.send(read_half); // clean end: reusable
                    if let Some(production_complete) = production_complete {
                        production_complete.mark_complete();
                    }
                    if let Some(done_tx) = done_tx {
                        let _ = done_tx.send(StreamCompletion::Completed);
                    }
                    return;
                }
                _ => return, // abnormal: unknown tag
            }
        }
    }

    /// Buffered request: streams internally and collects the whole body. Used
    /// by tests and non-streaming callers; infinite streams are bounded by the
    /// harness byte cap and the per-frame timeout. Legacy behavior: no detach
    /// buffer.
    pub async fn request(
        &mut self,
        req: SerializedRequest,
    ) -> Result<SerializedResponse, IsolationError> {
        self.request_with_detach(req, None).await
    }

    /// Buffered request with an optional stream-detach policy.
    pub async fn request_with_detach(
        &mut self,
        req: SerializedRequest,
        detach: Option<&StreamDetach>,
    ) -> Result<SerializedResponse, IsolationError> {
        let mut streamed = self.request_stream_with_detach(req, detach).await?;
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
        self.shutdown_report_classified(reason, grace)
            .await
            .into_acked()
    }

    /// Classified shutdown handshake (EDG-9): distinguishes "no handshake was
    /// possible" (socket not reclaimed / control frame not written — nothing
    /// was sent) from "the handshake was attempted and the ack never arrived
    /// in time". The unclassified wrapper above only surfaces acked reports.
    async fn shutdown_report_classified(
        &mut self,
        reason: &str,
        grace: Duration,
    ) -> ShutdownHandshake {
        // Reclaim the read half the same way a request does — after a request it
        // rests in `restore_rx`, not in `self.read_half`. A poisoned/mid-stream
        // process can't be reclaimed: skip straight to the kill — and NOTHING
        // was sent, so this is a poisoned socket, not a timeout.
        let mut read_half = match self.reclaim_read_half().await {
            Ok(half) => half,
            Err(_) => return ShutdownHandshake::SocketPoisoned,
        };
        let payload = match serde_json::to_vec(&WireShutdown {
            control: "shutdown",
            reason: reason.to_string(),
            grace_ms: grace.as_millis() as u64,
        }) {
            Ok(payload) => payload,
            // No payload, no handshake.
            Err(_) => return ShutdownHandshake::SocketPoisoned,
        };
        // (EDG-9, amendment 3) Only a fully successful write proceeds to
        // the ACK wait. `Ok(Err(_))` (broken pipe, reset by peer) means the
        // frame did NOT make it out: the handshake is impossible — a
        // poisoned socket, not a timeout. `Err(Elapsed)` (the write itself
        // stalled past the 1 s budget) is the real write timeout.
        // (EDG-9 slice 2) The write goes through the SHARED write-half mutex
        // (the abandon drain writes the cancel frame on the same half);
        // dropping the timed-out future drops the guard, releasing the lock.
        match tokio::time::timeout(Duration::from_secs(1), async {
            let mut write_half = self.write_half.lock().await;
            write_frame(&mut *write_half, &payload).await
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return ShutdownHandshake::SocketPoisoned,
            Err(_) => return ShutdownHandshake::AckTimedOut,
        }
        // Wait for the worker's drain ack, bounded by grace + a small margin.
        let deadline = grace.saturating_add(Duration::from_millis(500));
        match tokio::time::timeout(deadline, read_frame(&mut read_half)).await {
            Ok(Ok(frame)) => match serde_json::from_slice::<WireShutdownAck>(&frame) {
                Ok(ack) => ShutdownHandshake::Acked(ProcessDrainReport {
                    drained: ack.drained,
                    timed_out: ack.timed_out,
                }),
                // The peer sent something that is not a valid ack frame:
                // the handshake is broken — a poisoned socket, not a timeout.
                Err(_) => ShutdownHandshake::SocketPoisoned,
            },
            // EOF / broken pipe: the peer went away and no ack will ever
            // come — a poisoned socket, not a timeout.
            Ok(Err(_)) => ShutdownHandshake::SocketPoisoned,
            // The deadline ran out waiting for the ack: the real timeout.
            Err(_) => ShutdownHandshake::AckTimedOut,
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
    /// Stream-detach policy (EDG-8): releases the worker slot as soon as
    /// production completes; `None` keeps the legacy blocking behavior.
    detach: Option<StreamDetach>,
    /// Process-wide default duration from the orchestrator environment.
    stream_max_duration_default_ms: Option<u64>,
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

    /// Configure the stream policy: `max_bytes` caps how much of ONE
    /// response may be buffered (read but not yet delivered) before the
    /// reader applies backpressure; `budget` is the process-wide cap shared
    /// by all isolates. A `max_bytes` of `0` disables the detach queue and
    /// semaphores. The policy remains available so a configured max duration
    /// can still cancel and drain. The abandon-drain policy (EDG-9) defaults
    /// to the shared defaults; `with_abandon_drain_limits` overrides it.
    pub fn with_stream_detach(self, max_bytes: u64, budget: Arc<StreamDetachBudget>) -> Self {
        let detach = Some(StreamDetach {
            max_bytes,
            budget,
            drain: AbandonDrain::default(),
            max_duration: None,
        });
        Self { detach, ..self }
    }

    /// Set the process-wide max-duration default. A worker manifest value,
    /// including `0`, takes precedence at dispatch time.
    pub fn with_stream_max_duration_default_ms(mut self, duration_ms: u64) -> Self {
        self.stream_max_duration_default_ms = Some(duration_ms);
        self
    }

    /// Set the abandon-drain limits (EDG-9): when the response body is
    /// dropped before the end frame, the reader keeps reading and discarding
    /// frames up to `max_bytes` bytes and `max_ms` milliseconds before the
    /// socket is abandoned and the process recycled. `0` in EITHER limit
    /// disables the drain (the pre-EDG-9 behavior). These limits also govern
    /// a configured max-duration cut when `max_bytes` disables the detach
    /// queue.
    pub fn with_abandon_drain_limits(self, max_bytes: u64, max_ms: u64) -> Self {
        let detach = self.detach.map(|detach| StreamDetach {
            drain: AbandonDrain { max_bytes, max_ms },
            ..detach
        });
        Self { detach, ..self }
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
                if config.node_http_proxy
                    || config.fullstack.as_ref().is_some_and(|fullstack| {
                        matches!(fullstack.adapter.as_str(), "nextjs" | "remix")
                    })
                {
                    NodeHttpMode::Proxy
                } else {
                    NodeHttpMode::Capture
                },
                !config.fullstack.as_ref().is_some_and(|fullstack| {
                    matches!(fullstack.adapter.as_str(), "fresh" | "sveltekit")
                }),
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
            .request_with_detach(req, self.detach.as_ref())
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
        let mut policy = self.detach.clone();
        if let Some(policy) = policy.as_mut() {
            let duration_ms = config
                .stream_max_duration_ms
                .or(self.stream_max_duration_default_ms)
                .unwrap_or(0);
            policy.max_duration = Some(Duration::from_millis(duration_ms));
        }
        let result = self
            .process
            .as_mut()
            .expect("process just set")
            .request_stream_with_detach(req, policy.as_ref())
            .await;
        match result {
            Ok(streamed) => Ok(WorkerResponse::Streamed(StreamedResponse {
                status: streamed.status,
                headers: streamed.headers,
                body: Box::pin(ReceiverBody(streamed.chunks)),
                completed: streamed.completed,
                production_complete: streamed.production_complete,
                max_duration_elapsed_ms: streamed.max_duration_elapsed_ms,
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
            let handshake = process
                .shutdown_report_classified("terminate", self.shutdown_grace)
                .await;
            // (EDG-9) The report CLASSIFIES the handshake: a socket that could
            // not be reclaimed reports `SocketPoisoned` (nothing was sent —
            // no ack was ever awaited, so this is NOT a timeout); `TimedOut`
            // means a shutdown was actually sent and its deadline ran out.
            let outcome = match &handshake {
                ShutdownHandshake::Acked(report) if !report.timed_out => {
                    TerminationOutcome::Completed
                }
                // The worker acked that the beforeunload drain hit its grace
                // budget, or the ack never arrived after the send.
                ShutdownHandshake::Acked(_) | ShutdownHandshake::AckTimedOut => {
                    TerminationOutcome::TimedOut
                }
                ShutdownHandshake::SocketPoisoned => TerminationOutcome::SocketPoisoned,
            };
            let drained_count = match &handshake {
                ShutdownHandshake::Acked(report) => Some(report.drained),
                _ => None,
            };
            self.process = None;
            return Ok(TerminationReport {
                outcome,
                process_id: Some(process_id),
                drained_count,
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

#[cfg(test)]
mod stream_detach_tests {
    use super::{
        enabled_stream_max_duration, read_frame, reserve_chunk, write_frame, AbandonDrain,
        DenoProcessIsolate, DenoWorkerProcess, DetachPipeline, DrainOrigin, OwnedReadHalf,
        QueueItem, StreamDetach, StreamDetachBudget, StreamDuration, StreamProductionState,
        TAG_CHUNK, TAG_END,
    };
    use bytes::Bytes;
    use edger_core::Isolate;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::time::Instant as TokioInstant;

    struct TestReader {
        task: tokio::task::JoinHandle<()>,
        done: tokio::sync::oneshot::Receiver<edger_core::StreamCompletion>,
        restored: tokio::sync::oneshot::Receiver<OwnedReadHalf>,
        queued: tokio::sync::mpsc::UnboundedReceiver<QueueItem>,
        budget: Arc<StreamDetachBudget>,
        elapsed_ms: Option<Arc<std::sync::atomic::AtomicU64>>,
    }

    #[cfg(unix)]
    fn start_test_reader(
        read_end: tokio::net::UnixStream,
        limit: Option<Duration>,
        drain: AbandonDrain,
    ) -> TestReader {
        let (read_half, write_half_a) = read_end.into_split();
        let write_half = Arc::new(tokio::sync::Mutex::new(write_half_a));
        let budget = Arc::new(StreamDetachBudget::new(1_000_000));
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        std::mem::forget(cancel_tx);
        let elapsed_ms = enabled_stream_max_duration(limit)
            .map(|_| Arc::new(std::sync::atomic::AtomicU64::new(0)));
        let stream_started = TokioInstant::now();
        let max_duration = enabled_stream_max_duration(limit).map(|limit| StreamDuration {
            started: stream_started,
            limit,
            elapsed_ms: Arc::clone(elapsed_ms.as_ref().expect("duration recorder")),
            budget: Arc::clone(&budget),
            drain,
        });
        let pipeline = DetachPipeline {
            per_response: Arc::new(tokio::sync::Semaphore::new(1_000_000)),
            per_response_cap: 1_000_000,
            budget: Arc::clone(&budget),
            cancel: cancel_rx,
            abandon_drain: drain,
            max_duration,
        };
        let (q_tx, queued) = tokio::sync::mpsc::unbounded_channel();
        let (restore_tx, restored) = tokio::sync::oneshot::channel();
        let production_complete = Arc::new(StreamProductionState::default());
        let (done_tx, done) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(DenoWorkerProcess::detach_reader(
            read_half,
            write_half,
            q_tx,
            pipeline,
            restore_tx,
            production_complete,
            done_tx,
            Duration::from_secs(2),
        ));
        TestReader {
            task,
            done,
            restored,
            queued,
            budget,
            elapsed_ms,
        }
    }

    #[cfg(unix)]
    fn start_heartbeat_worker(
        peer: tokio::net::UnixStream,
    ) -> (
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Sender<()>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        let (mut worker_reader, mut worker_writer) = peer.into_split();
        let (cancel_seen_tx, cancel_seen_rx) = tokio::sync::oneshot::channel();
        let cancel_received = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_cancel_received = Arc::clone(&cancel_received);
        let control_reader = tokio::spawn(async move {
            if let Ok(frame) = read_frame(&mut worker_reader).await {
                let is_cancel = frame
                    .windows(b"__control".len())
                    .any(|part| part == b"__control");
                reader_cancel_received.store(is_cancel, std::sync::atomic::Ordering::Release);
                let _ = cancel_seen_tx.send(is_cancel);
            } else {
                let _ = cancel_seen_tx.send(false);
            }
        });
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        let worker_writer_task = tokio::spawn(async move {
            let mut heartbeat = tokio::time::interval(Duration::from_millis(50));
            let mut cancel_seen_rx = cancel_seen_rx;
            loop {
                tokio::select! {
                    biased;
                    cancelled = &mut cancel_seen_rx => {
                        if cancelled.unwrap_or(false) {
                            let mut end = vec![TAG_END];
                            end.extend_from_slice(br#"{"cancelled":true}"#);
                            let _ = write_frame(&mut worker_writer, &end).await;
                        }
                        break;
                    }
                    _ = &mut stop_rx => {
                        let mut end = vec![TAG_END];
                        end.extend_from_slice(b"{}");
                        let _ = write_frame(&mut worker_writer, &end).await;
                        break;
                    }
                    _ = heartbeat.tick() => {
                        let _ = write_frame(&mut worker_writer, &[TAG_CHUNK, b'h']).await;
                    }
                }
            }
        });
        (control_reader, worker_writer_task, stop_tx, cancel_received)
    }

    fn pipeline(per_response_cap: usize, budget: Arc<StreamDetachBudget>) -> DetachPipeline {
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        // Keep the sender alive for the whole test so `cancelled()` only
        // fires on an explicit cancel (not on sender drop).
        std::mem::forget(cancel_tx);
        DetachPipeline {
            per_response: Arc::new(tokio::sync::Semaphore::new(per_response_cap)),
            per_response_cap,
            budget,
            cancel: cancel_rx,
            // Disabled: these tests exercise the EDG-8 reservation/discard
            // mechanics, not the EDG-9 abandon drain.
            abandon_drain: AbandonDrain {
                max_bytes: 0,
                max_ms: 0,
            },
            max_duration: None,
        }
    }

    // Zero disables the pipeline entirely: the policy is normalized to
    // absent (no queue, no semaphores, no signal — the legacy path).
    #[test]
    fn zero_policy_is_normalized_to_absent() {
        let budget = Arc::new(StreamDetachBudget::new(1_000));
        let off = StreamDetach {
            max_bytes: 0,
            budget: Arc::clone(&budget),
            drain: AbandonDrain::default(),
            max_duration: None,
        };
        assert!(off.max_bytes == 0 && (off.max_bytes > 0).then_some(&off).is_none());
        let on = StreamDetach {
            max_bytes: 8,
            budget,
            drain: AbandonDrain::default(),
            max_duration: None,
        };
        assert!((on.max_bytes > 0).then_some(&on).is_some());
        assert_eq!(enabled_stream_max_duration(Some(Duration::ZERO)), None);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn max_duration_cancels_and_reuses_after_the_clean_end() {
        use tokio::net::UnixStream;

        let (read_end, peer) = UnixStream::pair().unwrap();
        let (control_reader, worker, _stop, cancel_received) = start_heartbeat_worker(peer);
        let mut reader = start_test_reader(
            read_end,
            Some(Duration::from_millis(200)),
            AbandonDrain {
                max_bytes: 1_000_000,
                max_ms: 2_000,
            },
        );

        tokio::time::timeout(Duration::from_secs(3), &mut reader.task)
            .await
            .expect("the total duration cuts the heartbeat stream")
            .unwrap();
        assert!(cancel_received.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            reader.restored.await.is_ok(),
            "clean cancel restores the socket"
        );
        assert_eq!(
            reader.done.await.unwrap(),
            edger_core::StreamCompletion::MaxDuration(edger_core::AbandonedStream::Cancelled)
        );
        let stats = reader.budget.stats();
        assert_eq!(stats.max_duration_cancelled_total, 1);
        assert_eq!(stats.max_duration_drained_total, 0);
        assert_eq!(stats.max_duration_socket_poisoned_total, 0);
        assert_eq!(stats.abandoned_cancelled_total, 0);
        assert_eq!(stats.abandoned_drained_total, 0);
        assert_eq!(stats.abandoned_socket_poisoned_total, 0);
        assert_eq!(stats.detached_total, 1);
        assert!(
            reader
                .elapsed_ms
                .unwrap()
                .load(std::sync::atomic::Ordering::Acquire)
                >= 200
        );

        control_reader.abort();
        worker.await.unwrap();
        drop(reader.queued);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn zero_max_duration_leaves_the_heartbeat_stream_running() {
        use tokio::net::UnixStream;

        let (read_end, peer) = UnixStream::pair().unwrap();
        let (control_reader, worker, stop, cancel_received) = start_heartbeat_worker(peer);
        let mut reader = start_test_reader(
            read_end,
            Some(Duration::ZERO),
            AbandonDrain {
                max_bytes: 1_000_000,
                max_ms: 2_000,
            },
        );

        tokio::time::sleep(Duration::from_millis(450)).await;
        assert!(
            !reader.task.is_finished(),
            "the stream ran over twice 200 ms"
        );
        assert_eq!(reader.budget.stats().max_duration_cancelled_total, 0);
        assert!(reader.elapsed_ms.is_none());
        stop.send(()).unwrap();
        worker.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), &mut reader.task)
            .await
            .expect("the natural end completes the reader")
            .unwrap();
        assert_eq!(
            reader.done.await.unwrap(),
            edger_core::StreamCompletion::Completed
        );
        assert!(reader.restored.await.is_ok());
        assert!(!cancel_received.load(std::sync::atomic::Ordering::Acquire));
        let stats = reader.budget.stats();
        assert_eq!(stats.max_duration_cancelled_total, 0);
        assert_eq!(stats.abandoned_cancelled_total, 0);

        control_reader.abort();
        drop(reader.queued);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn max_duration_with_disabled_drain_reports_socket_poisoned() {
        use tokio::net::UnixStream;

        let (read_end, peer) = UnixStream::pair().unwrap();
        let (control_reader, worker, _stop, cancel_received) = start_heartbeat_worker(peer);
        let mut reader = start_test_reader(
            read_end,
            Some(Duration::from_millis(200)),
            AbandonDrain {
                max_bytes: 0,
                max_ms: 2_000,
            },
        );

        tokio::time::timeout(Duration::from_secs(3), &mut reader.task)
            .await
            .expect("disabled drain returns promptly at the duration limit")
            .unwrap();
        assert!(
            reader.restored.await.is_err(),
            "poisoned socket is not reused"
        );
        assert_eq!(
            reader.done.await.unwrap(),
            edger_core::StreamCompletion::MaxDuration(edger_core::AbandonedStream::SocketPoisoned)
        );
        assert!(!cancel_received.load(std::sync::atomic::Ordering::Acquire));
        let stats = reader.budget.stats();
        assert_eq!(stats.max_duration_socket_poisoned_total, 1);
        assert_eq!(stats.abandoned_socket_poisoned_total, 0);
        assert_eq!(stats.abandoned_cancelled_total, 0);
        assert!(
            reader
                .elapsed_ms
                .unwrap()
                .load(std::sync::atomic::Ordering::Acquire)
                >= 200
        );

        control_reader.abort();
        let _ = worker.await;
        drop(reader.queued);
    }

    // A `0` in EITHER abandon-drain limit disables the drain (the socket is
    // abandoned and the process recycled, the pre-EDG-9 behavior).
    #[test]
    fn abandon_drain_requires_both_limits_positive() {
        assert!(AbandonDrain::default().enabled());
        assert!(!AbandonDrain {
            max_bytes: 0,
            max_ms: 2000
        }
        .enabled());
        assert!(!AbandonDrain {
            max_bytes: 8,
            max_ms: 0
        }
        .enabled());
        assert!(!AbandonDrain {
            max_bytes: 0,
            max_ms: 0
        }
        .enabled());
    }

    // A chunk larger than a semaphore's FULL capacity reserves that full
    // capacity (not its own size), so one oversized chunk can never deadlock
    // the pipeline; the RAII guard returns everything on drop.
    #[tokio::test]
    async fn oversized_chunk_reserves_full_capacity_not_its_size() {
        // Per-response cap 100, global budget 1_000.
        let budget = Arc::new(StreamDetachBudget::new(1_000));
        let pl = pipeline(100, Arc::clone(&budget));
        let guard = reserve_chunk(&pl, 5_000).await.expect("not cancelled");
        assert_eq!(
            budget.reserved_bytes(),
            1_000,
            "the whole global budget is held"
        );
        assert_eq!(
            pl.per_response.available_permits(),
            0,
            "the whole per-response cap is held"
        );
        drop(guard);
        assert_eq!(budget.reserved_bytes(), 0);
        assert_eq!(pl.per_response.available_permits(), 100);
    }

    // Per-response cap: once it is full the next reservation must WAIT for a
    // delivery (backpressure, slot held) and a fallback-cap counter is
    // recorded; dropping one chunk's guard unblocks it.
    #[tokio::test]
    async fn per_response_cap_backpressures_and_releases_on_delivery() {
        let budget = Arc::new(StreamDetachBudget::new(10_000));
        let pl = pipeline(100, Arc::clone(&budget));
        let g1 = reserve_chunk(&pl, 60).await.expect("not cancelled");
        let g2 = reserve_chunk(&pl, 40).await.expect("not cancelled");
        // Cap exhausted: the next reservation must WAIT for a delivery.
        let mut waiter = {
            let pl = pl.clone();
            tokio::spawn(async move { reserve_chunk(&pl, 1).await })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiter)
                .await
                .is_err(),
            "the reservation must still be waiting on the full cap"
        );
        // The waiter is blocked on the per-response CAP (it takes the budget
        // permit only after the cap frees up) — the budget is unchanged.
        assert_eq!(
            budget.reserved_bytes(),
            100,
            "60 + 40; the waiter has not reached the budget yet"
        );
        // Simulate the forwarder delivering the first chunk: its RAII guard
        // is dropped and the waiter can proceed.
        drop(g1);
        waiter.await.unwrap();
        assert_eq!(
            budget.reserved_bytes(),
            40,
            "only g2 (40) is still reserved; the waiter's guard dropped with its task"
        );
        let stats = budget.stats();
        assert_eq!(stats.fallback_cap_total, 1);
        assert_eq!(stats.fallback_budget_total, 0);
        // Release the rest (a delivery or a discard always returns permits).
        drop(g2);
        assert_eq!(budget.reserved_bytes(), 0);
    }

    // Global budget: shared across "responses"; exhaustion is a distinct
    // fallback reason; permits returned by one response unblock another.
    #[tokio::test]
    async fn global_budget_is_shared_and_returns_on_delivery() {
        let budget = Arc::new(StreamDetachBudget::new(100));
        let first = pipeline(10_000, Arc::clone(&budget));
        let second = pipeline(10_000, Arc::clone(&budget));
        let g1 = reserve_chunk(&first, 60).await.expect("not cancelled");
        let g2 = reserve_chunk(&second, 40).await.expect("not cancelled");
        // Budget exhausted for a third reservation: it must wait.
        let mut waiter = {
            let second = second.clone();
            tokio::spawn(async move { reserve_chunk(&second, 10).await })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiter)
                .await
                .is_err(),
            "budget exhaustion must block"
        );
        // The first response delivers: exactly its permits come back, and
        // the waiter (needing 10) unblocks and takes them.
        drop(g1);
        waiter.await.unwrap();
        assert_eq!(
            budget.reserved_bytes(),
            40,
            "only g2 (40) is still reserved; the waiter's guard dropped with its task"
        );
        let stats = budget.stats();
        assert_eq!(stats.fallback_budget_total, 1);
        drop(g2);
        assert_eq!(budget.reserved_bytes(), 0);
    }

    // Concurrency: under contention the shared budget never admits more than
    // its total; every dropped guard makes its permits available again.
    #[tokio::test]
    async fn budget_reservations_are_bounded_under_contention() {
        let budget = Arc::new(StreamDetachBudget::new(1_000));
        let pl = pipeline(u32::MAX as usize, Arc::clone(&budget));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let pl = pl.clone();
            handles.push(tokio::spawn(async move {
                let _guard = reserve_chunk(&pl, 100).await.expect("not cancelled");
                // Hold the reservation for a bit (simulating an in-flight
                // response); the RAII drop at scope end returns it.
                tokio::time::sleep(Duration::from_millis(200)).await;
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        // All tasks finished and every permit was returned: a FULL
        // reservation fits again (nothing leaked).
        assert_eq!(budget.reserved_bytes(), 0, "all permits returned");
        let full = reserve_chunk(&pl, 1_000).await.expect("not cancelled");
        assert_eq!(
            budget.reserved_bytes(),
            1_000,
            "the full reservation fits again"
        );
        drop(full);
        assert_eq!(budget.reserved_bytes(), 0);
    }

    // Review recipe for the send-failure path: body receiver CLOSED (consumer
    // gone), per-response capacity = exactly one chunk, one chunk reserved
    // sitting in the queue, and the reader trying to reserve the next one
    // (blocked on the full cap). The forwarder's discard must finish under a
    // timeout, unblock the blocked reader, and return every reservation.
    #[tokio::test]
    async fn discarded_pipeline_returns_all_reservations_under_timeout() {
        // Per-response capacity = one chunk (100); the budget is ample.
        let budget = Arc::new(StreamDetachBudget::new(1_000));
        // The forwarder's cancel sender is wired to the pipeline's receiver
        // (exactly like the real pipeline).
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let pipeline = DetachPipeline {
            per_response: Arc::new(tokio::sync::Semaphore::new(100)),
            per_response_cap: 100,
            budget: Arc::clone(&budget),
            cancel: cancel_rx,
            // Disabled: the test exercises the EDG-8 discard mechanics.
            abandon_drain: AbandonDrain {
                max_bytes: 0,
                max_ms: 0,
            },
            max_duration: None,
        };
        let (q_tx, q_rx) = tokio::sync::mpsc::unbounded_channel::<QueueItem>();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        drop(rx); // consumer gone: the body receiver is closed

        // Chunk 1: reserved and sitting in the queue.
        let g1 = reserve_chunk(&pipeline, 100).await.expect("not cancelled");
        q_tx.send(QueueItem::Chunk {
            chunk: Bytes::from(vec![b'x'; 100]),
            reservations: g1,
        })
        .unwrap();

        // The reader tries to reserve chunk 2: it blocks on the full
        // per-response capacity (or is cancelled by the forwarder). Its
        // sender is the only one left once the test drops its own, so the
        // queue closes when the reader finishes — exactly like the real
        // reader's exit. Either way it must observe a CLOSED queue.
        let mut reader = {
            let pipeline = pipeline.clone();
            let q_tx = q_tx.clone();
            tokio::spawn(async move {
                match reserve_chunk(&pipeline, 100).await {
                    Some(g2) => {
                        // Unblocked before the cancel won the race: the
                        // forwarder still closed the queue, so the send
                        // must fail.
                        q_tx.send(QueueItem::Chunk {
                            chunk: Bytes::from(vec![b'y'; 100]),
                            reservations: g2,
                        })
                        .is_err()
                    }
                    // Cancelled while reserving: the queue must be closed.
                    None => q_tx.is_closed(),
                }
            })
        };
        drop(q_tx);

        // The forwarder must drain (its send fails on the closed body)
        // returning every reservation — under a timeout.
        let mut forwarder = tokio::spawn(super::DenoWorkerProcess::detach_forwarder(
            q_rx, tx, cancel_tx,
        ));
        assert!(
            tokio::time::timeout(Duration::from_secs(2), &mut forwarder)
                .await
                .is_ok(),
            "the discard must finish under a timeout (a leaked reservation could keep it — and the reader — blocked forever)"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(2), &mut reader)
                .await
                .is_ok(),
            "the blocked reader must unblock: the discarded reservations are returned (or the cancellation fires)"
        );
        assert_eq!(budget.reserved_bytes(), 0, "every reservation was returned");
    }

    // Review recipe (round 3): the global budget (1000) is 90% held by
    // ANOTHER response that stays alive; the discarded response has a
    // 100-byte chunk whose send failed and a reader trying to reserve 1000
    // (which can NEVER fit while the other response lives). After the body
    // is closed, the forwarder cancels the reader and closes the queue:
    // BOTH tasks must finish under a timeout, the discarded response's
    // reservations must return, the other response's 900 must stay intact,
    // and the reader must see the queue closed.
    #[tokio::test]
    async fn discarded_pipeline_cancels_reader_blocked_on_other_responses_budget() {
        // Global budget 1000; another live response holds 900 of it.
        let budget = Arc::new(StreamDetachBudget::new(1_000));
        let other = pipeline(10_000, Arc::clone(&budget));
        let other_guard = reserve_chunk(&other, 900).await.expect("not cancelled");

        // Discarded response: per-response cap = one 100-byte chunk; the
        // forwarder's cancel sender is wired to the pipeline's receiver.
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let pipeline = DetachPipeline {
            per_response: Arc::new(tokio::sync::Semaphore::new(100)),
            per_response_cap: 100,
            budget: Arc::clone(&budget),
            cancel: cancel_rx,
            // Disabled: the test exercises the EDG-8 discard mechanics.
            abandon_drain: AbandonDrain {
                max_bytes: 0,
                max_ms: 0,
            },
            max_duration: None,
        };
        let (q_tx, q_rx) = tokio::sync::mpsc::unbounded_channel::<QueueItem>();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        drop(rx); // consumer gone: the body receiver is closed

        // Chunk 1: reserved (per-response 100 + budget 100 → the budget is
        // now FULL: 900 other + 100 here) and sitting in the queue.
        let g1 = reserve_chunk(&pipeline, 100).await.expect("not cancelled");
        q_tx.send(QueueItem::Chunk {
            chunk: Bytes::from(vec![b'x'; 100]),
            reservations: g1,
        })
        .unwrap();

        // The reader tries to reserve the next chunk (1000): without the
        // cancellation it would be parked on the global semaphore forever
        // (900 are held by the other live response, and returning chunk 1's
        // 100 is far from enough).
        let mut reader = {
            let pipeline = pipeline.clone();
            let q_tx = q_tx.clone();
            tokio::spawn(async move {
                match reserve_chunk(&pipeline, 1_000).await {
                    Some(g2) => {
                        // If the permits were released before the cancel
                        // won the race, the queue is still closed: the send
                        // must fail.
                        q_tx.send(QueueItem::Chunk {
                            chunk: Bytes::from(vec![b'z'; 100]),
                            reservations: g2,
                        })
                        .is_err()
                    }
                    // Cancelled while reserving: the queue must be closed.
                    None => q_tx.is_closed(),
                }
            })
        };
        drop(q_tx);

        let mut forwarder = tokio::spawn(super::DenoWorkerProcess::detach_forwarder(
            q_rx, tx, cancel_tx,
        ));
        let forwarder_result = tokio::time::timeout(Duration::from_secs(2), &mut forwarder).await;
        assert!(
            forwarder_result.is_ok(),
            "the forwarder must finish under a timeout"
        );
        let reader_result = tokio::time::timeout(Duration::from_secs(2), &mut reader).await;
        assert!(
            reader_result.is_ok(),
            "a reader parked on the global semaphore must be woken by the cancellation (killing the process/socket cannot wake a semaphore wait)"
        );
        assert!(
            reader_result.unwrap().unwrap(),
            "the reader's send to the queue must report it closed"
        );
        assert_eq!(
            budget.reserved_bytes(),
            900,
            "every reservation of the discarded response returned; the other response's 900 stays intact"
        );
        assert_eq!(
            pipeline.per_response.available_permits(),
            100,
            "the per-response permits were returned too"
        );
        drop(other_guard);
        assert_eq!(budget.reserved_bytes(), 0);
    }

    // (EDG-9) The chunk the reader had ALREADY read and discarded when the
    // consumer loss is observed (the chunk whose reservation failed) counts
    // toward the drain's byte budget from the start: with the budget already
    // spent by that chunk, a following clean TAG_END must NOT be accepted —
    // the drain stops at the byte limit, the socket is NOT restored and the
    // completion signal carries the CAUSE, not a `Completed`.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn pre_discarded_chunk_counts_toward_the_drain_byte_limit() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        // Per-response cap 100: chunk 1 (100 B) holds the WHOLE cap; chunk
        // 2 (200 B, above the cap) can only reserve via the backpressure
        // wait — the consumer loss fires there. The drain's byte limit
        // (100) is smaller than the pre-discarded chunk (200).
        // UDS pair: the reader gets the read half of end A; frames are
        // written to end B (the loopback the harness socket is for the
        // reader). The cancel control frame (EDG-9 slice 2) goes out on the
        // A-side write half — end B's buffer simply absorbs it (this test
        // never reads it back).
        let (read_end, write_end) = UnixStream::pair().unwrap();
        let (read_half, write_half_a) = read_end.into_split();
        let write_half = Arc::new(tokio::sync::Mutex::new(write_half_a));
        let mut write_half_peer = write_end;

        let budget = Arc::new(StreamDetachBudget::new(1_000));
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let pipeline = DetachPipeline {
            per_response: Arc::new(tokio::sync::Semaphore::new(100)),
            per_response_cap: 100,
            budget: Arc::clone(&budget),
            cancel: cancel_rx,
            abandon_drain: AbandonDrain {
                max_bytes: 100,
                max_ms: 10_000,
            },
            max_duration: None,
        };
        let (q_tx, _q_rx) = tokio::sync::mpsc::unbounded_channel::<QueueItem>();
        let (restore_tx, restore_rx) = tokio::sync::oneshot::channel();
        let flag = Arc::new(StreamProductionState::default());
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        // Frame wire format: 4-byte LE length, then tag byte + payload.
        let mut frame1 = vec![0u8; 4];
        frame1[..4].copy_from_slice(&((100 + 1) as u32).to_le_bytes());
        frame1.push(TAG_CHUNK);
        frame1.extend(std::iter::repeat_n(0x78u8, 100));
        let mut frame2 = vec![0u8; 4];
        frame2[..4].copy_from_slice(&((200 + 1) as u32).to_le_bytes());
        frame2.push(TAG_CHUNK);
        frame2.extend(std::iter::repeat_n(0x79u8, 200));
        let end_payload: &[u8] = &[TAG_END, b'{', b'}'];
        let mut frame3 = vec![0u8; 4];
        frame3[..4].copy_from_slice(&(end_payload.len() as u32).to_le_bytes());
        frame3.extend_from_slice(end_payload);
        // Write ALL frames before the reader starts: they sit in the socket
        // buffer, so the reader reads them without extra I/O yields and the
        // only place it can park is chunk 2's reservation select.
        write_half_peer.write_all(&frame1).await.unwrap();
        write_half_peer.write_all(&frame2).await.unwrap();
        write_half_peer.write_all(&frame3).await.unwrap();

        let mut reader = tokio::spawn(DenoWorkerProcess::detach_reader(
            read_half,
            Arc::clone(&write_half),
            q_tx,
            pipeline.clone(),
            restore_tx,
            flag.clone(),
            done_tx,
            Duration::from_secs(5),
        ));

        // Wait until chunk 1 is reserved (100 held) — i.e. the reader is
        // parked reserving chunk 2. Cancel must NOT fire before that: a
        // cancel at the loop top would drain with nothing pre-discarded.
        for _ in 0..1_000 {
            if budget.reserved_bytes() == 100 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            budget.reserved_bytes(),
            100,
            "chunk 1 must hold the whole per-response cap"
        );
        // Consumer lost DURING the reservation of the 200-B chunk.
        cancel_tx.send_replace(true);

        let joined = tokio::time::timeout(Duration::from_secs(5), &mut reader).await;
        assert!(joined.is_ok(), "the reader must exit after the drain stops");

        // The pre-discarded 200-B chunk already exceeds the 100-B limit:
        // the drain stops at the byte limit BEFORE reading the clean
        // TAG_END.
        let stats = budget.stats();
        assert_eq!(
            stats.abandoned_drain_bytes_limit_total, 1,
            "byte limit must fire"
        );
        assert_eq!(
            stats.abandoned_drained_total, 0,
            "the TAG_END must not be accepted"
        );
        assert!(!flag.is_complete(), "no production-complete flag");
        assert!(
            restore_rx.await.is_err(),
            "the socket must NOT be restored (desynced)"
        );
        assert_eq!(
            done_rx.await.unwrap(),
            edger_core::StreamCompletion::Abandoned(edger_core::AbandonedStream::BytesLimit),
            "the completion signal must carry the drain cause, not a completed"
        );
    }

    // (EDG-9 slice 2) The abandon writes the CANCEL control frame BEFORE
    // draining: the drain writes it on the shared write half and only then
    // reads frames — so the peer observes the cancel FIRST. The harness's
    // cancel end frame (`E {"cancelled":true}`) is a CLEAN end inside the
    // drain: the read half is restored, the production-complete flag is set
    // and the completion signal carries the `cancelled` cause (the pool
    // reuses the process).
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn abandon_drain_writes_cancel_before_draining_and_reports_cancelled_end() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixStream;

        let (read_end, mut peer) = UnixStream::pair().unwrap();
        let (read_half, write_half_a) = read_end.into_split();
        let write_half = Arc::new(tokio::sync::Mutex::new(write_half_a));

        let budget = Arc::new(StreamDetachBudget::new(1_000));
        let pipeline = DetachPipeline {
            per_response: Arc::new(tokio::sync::Semaphore::new(100)),
            per_response_cap: 100,
            budget: Arc::clone(&budget),
            cancel: tokio::sync::watch::channel(false).1,
            abandon_drain: AbandonDrain {
                max_bytes: 10_000,
                max_ms: 10_000,
            },
            max_duration: None,
        };
        let (restore_tx, restore_rx) = tokio::sync::oneshot::channel();
        let flag = Arc::new(StreamProductionState::default());
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let flag_inner = Arc::clone(&flag);

        let mut drain = tokio::spawn(async move {
            DenoWorkerProcess::drain_on_abandon(
                read_half,
                write_half,
                Arc::clone(&pipeline.budget),
                pipeline.abandon_drain,
                restore_tx,
                flag_inner,
                done_tx,
                Duration::from_secs(5),
                0,
                DrainOrigin::ClientGone,
                TokioInstant::now(),
                false,
                true,
            )
            .await
        });

        // The drain must write the cancel control frame BEFORE reading any
        // frame: the peer reads it first, and it is the plain
        // `{"__control":"cancel"}` JSON frame.
        let cancel_frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut peer))
            .await
            .expect("the cancel frame is written before any drain read")
            .expect("the cancel frame is read");
        let payload = String::from_utf8(cancel_frame).unwrap();
        assert!(
            payload.contains("\"__control\":\"cancel\""),
            "the drain writes the cancel control frame: {payload}"
        );

        // The harness's answer to the cancel: an end frame carrying
        // {"cancelled":true}.
        let end_payload = br#"{"cancelled":true}"#;
        let mut end_frame = ((1 + end_payload.len()) as u32).to_le_bytes().to_vec();
        end_frame.push(TAG_END);
        end_frame.extend_from_slice(end_payload);
        peer.write_all(&end_frame).await.unwrap();

        let joined = tokio::time::timeout(Duration::from_secs(5), &mut drain).await;
        assert!(
            joined.is_ok(),
            "the drain must exit on the cancel end frame"
        );

        assert!(
            restore_rx.await.is_ok(),
            "the cancel end frame restores the read half (the process is reusable)"
        );
        assert!(
            flag.is_complete(),
            "the production-complete flag is set on a cancel end"
        );
        assert_eq!(
            done_rx.await.unwrap(),
            edger_core::StreamCompletion::Abandoned(edger_core::AbandonedStream::Cancelled),
            "the completion signal carries the cancelled cause, not a completed"
        );
        let stats = budget.stats();
        assert_eq!(
            stats.abandoned_cancelled_total, 1,
            "the cancelled counter moves"
        );
        assert_eq!(
            stats.abandoned_drained_total, 0,
            "a cancel end is distinct from a plain drained end"
        );
        assert_eq!(stats.max_duration_cancelled_total, 0);
        assert!(
            stats.detached_total >= 1,
            "the aborted production still counts as detached"
        );
    }

    // (EDG-9 slice 2) A cancel write that FAILS (the peer is gone) recycles
    // with the `socket_poisoned` sub-cause: no restore, no flag, no cancel
    // end — the socket cannot be trusted.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn abandon_drain_cancel_write_failure_reports_socket_poisoned() {
        use tokio::net::UnixStream;

        let (read_end, peer) = UnixStream::pair().unwrap();
        let (read_half, write_half_a) = read_end.into_split();
        let write_half = Arc::new(tokio::sync::Mutex::new(write_half_a));
        drop(peer); // the peer is gone: the cancel write must fail

        let budget = Arc::new(StreamDetachBudget::new(1_000));
        let pipeline = DetachPipeline {
            per_response: Arc::new(tokio::sync::Semaphore::new(100)),
            per_response_cap: 100,
            budget: Arc::clone(&budget),
            cancel: tokio::sync::watch::channel(false).1,
            abandon_drain: AbandonDrain {
                max_bytes: 10_000,
                max_ms: 10_000,
            },
            max_duration: None,
        };
        let (restore_tx, restore_rx) = tokio::sync::oneshot::channel();
        let flag = Arc::new(StreamProductionState::default());
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let flag_inner = Arc::clone(&flag);

        let mut drain = tokio::spawn(async move {
            DenoWorkerProcess::drain_on_abandon(
                read_half,
                write_half,
                Arc::clone(&pipeline.budget),
                pipeline.abandon_drain,
                restore_tx,
                flag_inner,
                done_tx,
                Duration::from_secs(5),
                0,
                DrainOrigin::ClientGone,
                TokioInstant::now(),
                false,
                true,
            )
            .await
        });

        let joined = tokio::time::timeout(Duration::from_secs(5), &mut drain).await;
        assert!(
            joined.is_ok(),
            "the drain must exit on the failed cancel write (well under the budget)"
        );
        assert!(
            restore_rx.await.is_err(),
            "a failed cancel write must NOT restore the read half"
        );
        assert!(
            !flag.is_complete(),
            "no production-complete flag on a failed cancel write"
        );
        assert_eq!(
            done_rx.await.unwrap(),
            edger_core::StreamCompletion::Abandoned(edger_core::AbandonedStream::SocketPoisoned),
            "the completion signal carries the socket_poisoned sub-cause"
        );
        let stats = budget.stats();
        assert_eq!(
            stats.abandoned_socket_poisoned_total, 1,
            "the poisoned counter moves"
        );
        assert_eq!(
            stats.abandoned_cancelled_total, 0,
            "no cancel was acknowledged"
        );
    }

    // (EDG-9 slice 2) With the drain DISABLED no cancel frame is written at
    // all (the pre-slice behavior): the drain reports `socket_poisoned` up
    // front and the socket stays silent.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn disabled_abandon_drain_writes_no_cancel() {
        use tokio::io::AsyncReadExt;
        use tokio::net::UnixStream;

        let (read_end, mut peer) = UnixStream::pair().unwrap();
        let (read_half, write_half_a) = read_end.into_split();
        let write_half = Arc::new(tokio::sync::Mutex::new(write_half_a));

        let budget = Arc::new(StreamDetachBudget::new(1_000));
        let pipeline = DetachPipeline {
            per_response: Arc::new(tokio::sync::Semaphore::new(100)),
            per_response_cap: 100,
            budget: Arc::clone(&budget),
            cancel: tokio::sync::watch::channel(false).1,
            // Disabled: `0` in either limit.
            abandon_drain: AbandonDrain {
                max_bytes: 0,
                max_ms: 0,
            },
            max_duration: None,
        };
        let (restore_tx, restore_rx) = tokio::sync::oneshot::channel();
        let flag = Arc::new(StreamProductionState::default());
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        let mut drain = tokio::spawn(async move {
            DenoWorkerProcess::drain_on_abandon(
                read_half,
                write_half,
                Arc::clone(&pipeline.budget),
                pipeline.abandon_drain,
                restore_tx,
                flag,
                done_tx,
                Duration::from_secs(5),
                0,
                DrainOrigin::ClientGone,
                TokioInstant::now(),
                false,
                true,
            )
            .await
        });

        let joined = tokio::time::timeout(Duration::from_secs(2), &mut drain).await;
        assert!(
            joined.is_ok(),
            "a disabled drain must return immediately (no write attempt)"
        );
        // Nothing may have been written to the socket: the drain task has
        // exited and dropped its halves (EOF) or the socket is still open
        // (timeout) — but NO frame may have been delivered.
        let mut probe = Vec::new();
        let read_result = tokio::time::timeout(Duration::from_millis(200), peer.read(&mut probe))
            .await
            .expect("the bounded probe read finishes");
        match read_result {
            // EOF: the drain side closed its halves without writing anything.
            Ok(0) => {}
            Ok(n) => panic!("a frame was written with the drain disabled: {n} bytes"),
            Err(err) => panic!("probe read error: {err}"),
        }
        assert!(
            restore_rx.await.is_err(),
            "a disabled drain must NOT restore the read half"
        );
        assert_eq!(
            done_rx.await.unwrap(),
            edger_core::StreamCompletion::Abandoned(edger_core::AbandonedStream::SocketPoisoned),
            "the completion signal carries the socket_poisoned cause, as before"
        );
        let stats = budget.stats();
        assert_eq!(
            stats.abandoned_socket_poisoned_total, 1,
            "the poisoned counter moves"
        );
        assert_eq!(stats.abandoned_cancelled_total, 0, "no cancel was written");
    }

    // (EDG-9, amendment 2) Shutdown-handshake classification: the
    // termination report must distinguish "socket not reclaimed" (nothing
    // was sent — `SocketPoisoned`) from "shutdown sent, no ack in time"
    // (`TimedOut`). Hand-built processes over a UDS pair — no Deno child
    // needed (the dummy child only exists to be killed on drop).

    /// Hand-built process for the shutdown-classification tests: a real UDS
    /// pair (the test side is the returned stream) plus a dummy child.
    #[cfg(unix)]
    fn classified_terminate_process(
        read_half: Option<OwnedReadHalf>,
        restore_rx: Option<tokio::sync::oneshot::Receiver<OwnedReadHalf>>,
    ) -> (DenoProcessIsolate, tokio::net::UnixStream) {
        let child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("dummy child spawns");
        let (stream_a, stream_b) = tokio::net::UnixStream::pair().unwrap();
        let (_read_a, write_a) = stream_a.into_split();
        let process = DenoWorkerProcess {
            child,
            write_half: Arc::new(tokio::sync::Mutex::new(write_a)),
            read_half,
            restore_rx,
            timeout: Duration::from_secs(5),
            _bundle_dir: None,
            _workdir: tempfile::tempdir().unwrap(),
            _limit_monitor: None,
            _console_tasks: Vec::new(),
            stderr_tail: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            process_id: "test-classified-terminate".into(),
        };
        let isolate = DenoProcessIsolate {
            process: Some(process),
            // 100 ms grace: the ACK deadline is 600 ms (grace + 500 margin).
            shutdown_grace: Duration::from_millis(100),
            console_sender: None,
            console_context: None,
            stream_max_duration_default_ms: None,
            detach: None,
        };
        (isolate, stream_b)
    }

    /// No read half, no pending restore: the reclaim fails immediately and
    /// NO shutdown frame may be sent — the outcome is `SocketPoisoned`,
    /// never a timeout.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn terminate_without_a_reclaimable_socket_reports_socket_poisoned() {
        let (mut isolate, mut test_end) = classified_terminate_process(None, None);
        let report = isolate
            .terminate_with_report()
            .await
            .expect("terminate reports");
        assert_eq!(
            report.outcome,
            edger_core::TerminationOutcome::SocketPoisoned,
            "no reclaimable socket, no handshake: socket_poisoned, not a timeout"
        );
        let mut sent = Vec::new();
        test_end.read_to_end(&mut sent).await.unwrap();
        assert!(
            sent.is_empty(),
            "no shutdown frame may be written when the socket cannot be reclaimed"
        );
    }

    /// The reader keeps the restore pending PAST the ACK deadline (100 ms
    /// grace + 500 ms margin = 600 ms) and abandons it at 700 ms (poisoned
    /// recovery). The aggregate elapsed time exceeds the ACK deadline — an
    /// elapsed-based heuristic would classify this as an ACK timeout; the
    /// classified report must say `SocketPoisoned` (nothing was sent).
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn slow_socket_recovery_ending_poisoned_is_socket_poisoned_not_an_ack_timeout() {
        let (_restore_tx, restore_rx) = tokio::sync::oneshot::channel::<OwnedReadHalf>();
        let (mut isolate, mut test_end) = classified_terminate_process(None, Some(restore_rx));
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(700)).await;
            drop(_restore_tx);
        });
        let started = std::time::Instant::now();
        let report = isolate
            .terminate_with_report()
            .await
            .expect("terminate reports");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(700),
            "the terminate must have waited on the (slow) recovery: {elapsed:?}"
        );
        assert_eq!(
            report.outcome,
            edger_core::TerminationOutcome::SocketPoisoned,
            "a slow recovery that ends poisoned is NOT an ACK timeout"
        );
        let mut sent = Vec::new();
        test_end.read_to_end(&mut sent).await.unwrap();
        assert!(
            sent.is_empty(),
            "no shutdown frame may be sent — the socket was never reclaimed"
        );
    }

    /// The read half is resting: the reclaim succeeds, the shutdown frame IS
    /// sent, and the ACK never arrives — the 600 ms deadline runs out. That
    /// is the ONLY case that may be classified `TimedOut`.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_sent_without_an_ack_reports_timed_out() {
        let (read_end, _write_end) = tokio::net::UnixStream::pair().unwrap();
        let (read_half, _write_a) = read_end.into_split();
        let (mut isolate, mut test_end) = classified_terminate_process(Some(read_half), None);
        let report = isolate
            .terminate_with_report()
            .await
            .expect("terminate reports");
        assert_eq!(
            report.outcome,
            edger_core::TerminationOutcome::TimedOut,
            "a sent shutdown with no ack in time is the real timeout"
        );
        // Prove the shutdown frame actually went out: length-prefixed JSON
        // control frame on the socket.
        let mut sent = Vec::new();
        test_end.read_to_end(&mut sent).await.unwrap();
        assert!(sent.len() >= 5, "the shutdown frame must have been written");
        let len = u32::from_le_bytes(sent[0..4].try_into().unwrap()) as usize;
        let payload = std::str::from_utf8(&sent[4..4 + len]).unwrap();
        assert!(
            payload.contains("\"shutdown\""),
            "the control frame is the shutdown handshake: {payload}"
        );
    }

    // (EDG-9, amendment 3) I/O errors on the shutdown handshake are NOT
    // timeouts: a write that fails (peer gone) or an ACK read that hits
    // EOF/invalid frame is a poisoned socket; only the deadlines expiring
    // classify `TimedOut`.

    /// Hand-built process whose read half RESTS (reclaim succeeds
    /// immediately) and whose PEER side is returned: the shutdown write and
    /// the ACK read go through the real UDS pair.
    #[cfg(unix)]
    fn idle_socket_process() -> (DenoProcessIsolate, tokio::net::UnixStream) {
        let child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("dummy child spawns");
        let (stream_a, stream_b) = tokio::net::UnixStream::pair().unwrap();
        let (read_a, write_a) = stream_a.into_split();
        let process = DenoWorkerProcess {
            child,
            write_half: Arc::new(tokio::sync::Mutex::new(write_a)),
            read_half: Some(read_a),
            restore_rx: None,
            timeout: Duration::from_secs(5),
            _bundle_dir: None,
            _workdir: tempfile::tempdir().unwrap(),
            _limit_monitor: None,
            _console_tasks: Vec::new(),
            stderr_tail: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            process_id: "test-idle-socket".into(),
        };
        let isolate = DenoProcessIsolate {
            process: Some(process),
            // 100 ms grace: the ACK deadline is 600 ms (grace + 500 margin).
            shutdown_grace: Duration::from_millis(100),
            console_sender: None,
            console_context: None,
            stream_max_duration_default_ms: None,
            detach: None,
        };
        (isolate, stream_b)
    }

    /// (a) The peer closes BEFORE the shutdown write: the frame cannot make
    /// it out (broken pipe — `Ok(Err(_))`, NOT the outer timeout), so the
    /// outcome is `SocketPoisoned` and the terminate finishes well UNDER the
    /// ACK deadline (no ack was ever awaited → no `DrainTimedOut`).
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn peer_closed_before_the_shutdown_write_reports_socket_poisoned() {
        let (mut isolate, peer) = idle_socket_process();
        drop(peer); // the peer goes away before the write
        let started = std::time::Instant::now();
        let report = isolate
            .terminate_with_report()
            .await
            .expect("terminate reports");
        let elapsed = started.elapsed();
        assert_eq!(
            report.outcome,
            edger_core::TerminationOutcome::SocketPoisoned,
            "a write that fails with an I/O error is a poisoned socket, not a timeout"
        );
        assert!(
            elapsed < Duration::from_millis(600),
            "no ack was awaited — the terminate must finish under the ACK deadline: {elapsed:?}"
        );
    }

    /// (b) The peer RECEIVES the shutdown frame and closes WITHOUT acking:
    /// the ACK read hits EOF (`Ok(Err(_))`) — `SocketPoisoned`, not the old
    /// `AckTimedOut` — and the terminate finishes well UNDER the ACK
    /// deadline.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn peer_closes_after_receiving_the_shutdown_reports_socket_poisoned() {
        let (mut isolate, mut peer) = idle_socket_process();
        // The peer reads the shutdown frame, then closes without acking.
        let (frame_tx, frame_rx) = tokio::sync::oneshot::channel::<std::io::Result<Vec<u8>>>();
        tokio::spawn(async move {
            let frame = read_frame(&mut peer).await;
            let _ = frame_tx.send(frame);
            drop(peer);
        });
        let started = std::time::Instant::now();
        let report = isolate
            .terminate_with_report()
            .await
            .expect("terminate reports");
        let elapsed = started.elapsed();
        // The peer must have received the real shutdown control frame.
        let frame = frame_rx
            .await
            .expect("the peer read task ran")
            .expect("the shutdown frame arrives");
        assert!(
            String::from_utf8_lossy(&frame).contains("\"shutdown\""),
            "the peer must receive the shutdown control frame: {frame:?}"
        );
        assert_eq!(
            report.outcome,
            edger_core::TerminationOutcome::SocketPoisoned,
            "an EOF while waiting for the ack is a poisoned socket, not a timeout"
        );
        assert!(
            elapsed < Duration::from_millis(600),
            "the EOF is immediate — the terminate must finish under the ACK deadline: {elapsed:?}"
        );
    }
}
