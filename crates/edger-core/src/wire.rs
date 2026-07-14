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

/// A response whose body streams incrementally from the worker (SSE, chunked
/// SSR). Status/headers are available up front; chunks arrive as the worker
/// produces them.
pub struct StreamedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: BodyStream,
}

impl std::fmt::Debug for StreamedResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamedResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body", &"<stream>")
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
