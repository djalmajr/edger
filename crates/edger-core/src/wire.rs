//! Wire formats for isolate boundary (SerializedRequest/Response).

use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// EDG-9 stream-abandon drain defaults: when a streamed response body is
/// dropped before the end frame, the reader keeps reading and discarding
/// frames (no budget, no queue) up to this many bytes before giving up and
/// recycling the process. `0` disables the drain (the socket is abandoned
/// and the process recycled, the pre-EDG-9 behavior).
pub const STREAM_ABANDON_DRAIN_MAX_BYTES_DEFAULT: u64 = 8 * 1024 * 1024;
/// Wall-clock budget (ms) for the abandon drain; the pool waits at most this
/// long (plus a small grace) for the completion signal on an early body
/// drop/error before recycling. `0` disables the drain.
pub const STREAM_ABANDON_DRAIN_MAX_MS_DEFAULT: u64 = 2000;

/// Buntime HeaderLimits port: max header count.
pub const MAX_HEADERS: usize = 100;
/// Total header bytes limit.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;
/// Per-header value limit.
pub const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;

/// Request crossing the isolate boundary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SerializedRequest {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Bytes>,
    pub request_id: String,
    pub base_href: Option<String>,
}

/// Response crossing the isolate boundary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SerializedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Option<Bytes>,
}

/// Boxed chunk stream for a streamed worker response body.
pub type BodyStream = std::pin::Pin<
    Box<dyn futures_core::Stream<Item = Result<Bytes, crate::error::IsolationError>> + Send>,
>;

/// Why an abandoned stream did not complete cleanly (EDG-9). The abandon
/// drain (consumer lost before the end frame) stops for exactly one of these
/// reasons; the pool carries it into the recycle lifecycle detail and the
/// operational event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbandonedStream {
    /// The max-duration cut reached a clean natural end while discarding the
    /// in-flight response. Client-abandon drains keep using `Completed`.
    Drained,
    /// The drain is disabled (a `0` limit): the socket was abandoned
    /// mid-response and poisoned.
    SocketPoisoned,
    /// The discard drain exceeded `EDGER_STREAM_ABANDON_DRAIN_MAX_BYTES`.
    BytesLimit,
    /// The discard drain exceeded `EDGER_STREAM_ABANDON_DRAIN_MAX_MS`.
    TimeLimit,
    /// A read, protocol, or in-band error aborted the drain.
    StreamError,
    /// The harness acknowledged the CANCEL control frame (EDG-9 slice 2):
    /// the client abandoned the stream, the orchestrator told the harness
    /// to cancel it and the harness answered with the `cancelled` end
    /// frame. Like a clean drain end, the socket is restored and the
    /// process is REUSED — the pool completes the dispatch with the
    /// `stream_abandoned_drained` reason and the `cancelled` sub-cause.
    Cancelled,
    /// The POOL's bounded wait for the drain result expired before the
    /// reader reported anything (slow producer): the reader's late result,
    /// if any, is ignored without error. Synthesized by the pool — the
    /// reader never reports it.
    RelayTimeout,
}

/// The outcome of a streamed response's production (EDG-8/EDG-9/EDG-16),
/// delivered through the completion signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamCompletion {
    /// Production reached a clean end frame and the socket is in sync —
    /// the process is reusable, either directly or after a client-abandon
    /// drain finished the response within its limits.
    Completed,
    /// The consumer was lost before the end frame and the response was
    /// abandoned; the variant names the cause (EDG-9).
    Abandoned(AbandonedStream),
    /// The total stream-duration limit fired; the outcome records whether the
    /// EDG-9 cancel/drain left the process reusable or required a recycle.
    MaxDuration(AbandonedStream),
    /// Production did not complete cleanly and no cause was reported: the
    /// producer went away before reporting one (error end or an abnormal
    /// task exit). The pool recycles with the body-level detail it has.
    Incomplete,
}

/// Production-complete signal for a streamed response (no I/O, pure std):
/// resolves to `StreamCompletion::Completed` once the worker has fully
/// produced the response (end of production without error, all chunks
/// already in flight — possibly via a client-abandon drain), so the runtime
/// may release the worker slot before a slow client finishes downloading the
/// buffered tail. Resolves to `Abandoned(cause)` when the consumer was lost
/// and its drain fails, `MaxDuration(cause)` when the total duration limit
/// fires, and `Incomplete` when production did not complete cleanly without a
/// reported cause.
pub type CompletionSignal =
    std::pin::Pin<Box<dyn std::future::Future<Output = StreamCompletion> + Send>>;

/// Shared production state for a streamed response. A clean max-duration
/// cutoff records its outcome before marking production complete, allowing a
/// body that reaches EOF before the completion observer to preserve the
/// max-duration lifecycle event without waiting for the relay.
#[derive(Debug, Default)]
pub struct StreamProductionState {
    production_complete: std::sync::atomic::AtomicBool,
    max_duration_outcome: std::sync::OnceLock<AbandonedStream>,
}

impl StreamProductionState {
    /// Marks a clean production end that did not result from the duration cap.
    pub fn mark_complete(&self) {
        self.production_complete
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Marks a clean duration cutoff after recording its reusable outcome.
    pub fn mark_max_duration_complete(&self, outcome: AbandonedStream) {
        debug_assert!(matches!(
            outcome,
            AbandonedStream::Drained | AbandonedStream::Cancelled
        ));
        let _ = self.max_duration_outcome.set(outcome);
        self.mark_complete();
    }

    /// Whether production ended with the socket in a reusable state.
    pub fn is_complete(&self) -> bool {
        self.production_complete
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// The clean outcome recorded by a max-duration cutoff, if any.
    pub fn max_duration_outcome(&self) -> Option<AbandonedStream> {
        self.max_duration_outcome.get().copied()
    }
}

/// A response whose body streams incrementally from the worker (SSE, chunked
/// SSR). Status/headers are available up front; chunks arrive as the worker
/// produces them. Backends that cannot observe production completion leave
/// `completed` and `production_complete` as `None`; the end of the body
/// remains the only release trigger for the worker slot.
pub struct StreamedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: BodyStream,
    pub completed: Option<CompletionSignal>,
    /// Marked complete once production finished cleanly (end frame without
    /// error — possibly after an abandon or max-duration drain), BEFORE `completed`
    /// resolves. A clean max-duration outcome is stored in the same shared
    /// state before completion is marked. A body that is dropped or errors
    /// after this flag is set must COMPLETE the dispatch instead of
    /// recycling: the producer socket is already in sync and the process is
    /// reusable. Backends without the mechanism leave this `None`.
    pub production_complete: Option<std::sync::Arc<StreamProductionState>>,
    /// Milliseconds from response-header arrival to the max-duration cutoff.
    /// Set by the multiprocess reader before it starts the bounded drain.
    pub max_duration_elapsed_ms: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl std::fmt::Debug for StreamedResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamedResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body", &"<stream>")
            .field("completed", &self.completed.is_some())
            .field("production_complete", &self.production_complete.is_some())
            .field(
                "max_duration_elapsed_ms",
                &self.max_duration_elapsed_ms.is_some(),
            )
            .finish()
    }
}

/// Worker response: buffered (all current backends) or streamed (persistent
/// process backend). Buffered is the trait default so existing isolates are
/// untouched.
#[derive(Debug)]
pub enum WorkerResponse {
    Buffered(SerializedResponse),
    Streamed(StreamedResponse),
}

/// Validate header collection against core limits (pure).
pub fn validate_headers(headers: &[(String, String)]) -> Result<(), crate::error::CoreError> {
    if headers.len() > MAX_HEADERS {
        return Err(crate::error::CoreError::validation(
            "headers",
            format!("exceeds max count {MAX_HEADERS}"),
        ));
    }
    let mut total = 0usize;
    for (name, value) in headers {
        total += name.len() + value.len();
        if value.len() > MAX_HEADER_VALUE_BYTES {
            return Err(crate::error::CoreError::validation(
                "headers",
                format!("header value exceeds {MAX_HEADER_VALUE_BYTES} bytes"),
            ));
        }
    }
    if total > MAX_HEADER_BYTES {
        return Err(crate::error::CoreError::validation(
            "headers",
            format!("total header bytes exceed {MAX_HEADER_BYTES}"),
        ));
    }
    Ok(())
}
