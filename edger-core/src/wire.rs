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

/// Validate header collection against core limits (pure).
pub fn validate_headers(headers: &[(String, String)]) -> Result<(), crate::error::CoreError> {
    if headers.len() > MAX_HEADERS {
        return Err(crate::error::CoreError::validation(
            "headers",
            format!("exceeds max count {}", MAX_HEADERS),
        ));
    }
    let mut total = 0usize;
    for (name, value) in headers {
        total += name.len() + value.len();
        if value.len() > MAX_HEADER_VALUE_BYTES {
            return Err(crate::error::CoreError::validation(
                "headers",
                format!("header value exceeds {} bytes", MAX_HEADER_VALUE_BYTES),
            ));
        }
    }
    if total > MAX_HEADER_BYTES {
        return Err(crate::error::CoreError::validation(
            "headers",
            format!("total header bytes exceed {}", MAX_HEADER_BYTES),
        ));
    }
    Ok(())
}
