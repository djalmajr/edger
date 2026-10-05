//! Wire formats for isolate boundary (SerializedRequest/Response).

use bytes::Bytes;
use serde::{Deserialize, Serialize};

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

/// Production-complete signal for a streamed response (no I/O, pure std):
/// resolves to `true` once the worker has fully produced the response (end of
/// production without error, all chunks already in flight), so the runtime
/// may release the worker slot before a slow client finishes downloading the
/// buffered tail. Resolves to `false` when production did not complete cleanly
/// (error, or the producer went away).
pub type CompletionSignal = std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>;

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
    /// Set to `true` once production finished cleanly (end frame without
    /// error), BEFORE `completed` resolves. A body that is dropped or errors
    /// after this flag is set must COMPLETE the dispatch instead of
    /// recycling: the producer socket is already in sync and the process is
    /// reusable. Backends without the mechanism leave this `None`.
    pub production_complete: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl std::fmt::Debug for StreamedResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamedResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body", &"<stream>")
            .field("completed", &self.completed.is_some())
            .field("production_complete", &self.production_complete.is_some())
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
