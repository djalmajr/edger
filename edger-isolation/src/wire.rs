//! Wire validation and IPC framing (types canonical in edger-core).

use edger_core::{validate_headers, CoreError, IsolationError, SerializedRequest, WorkerConfig};

/// Validate request against core header limits and worker max body size.
pub fn validate_request(
    req: &SerializedRequest,
    config: &WorkerConfig,
) -> Result<(), IsolationError> {
    validate_headers(&req.headers).map_err(|e| IsolationError::new(&e.code, e.message))?;

    if let (Some(max), Some(body)) = (config.max_body_size_bytes, &req.body) {
        if body.len() as u64 > max {
            return Err(IsolationError::new(
                "VALIDATION_ERROR",
                format!("body exceeds max {} bytes", max),
            ));
        }
    }
    Ok(())
}

/// Length-prefixed postcard frame for future UDS/pipe transport.
pub fn encode_frame(req: &SerializedRequest) -> Result<Vec<u8>, IsolationError> {
    let payload = postcard::to_allocvec(req)
        .map_err(|e| IsolationError::new("WIRE_ENCODE", e.to_string()))?;
    let len = u32::try_from(payload.len())
        .map_err(|_| IsolationError::new("WIRE_ENCODE", "frame too large"))?;
    let mut frame = len.to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decode length-prefixed postcard frame.
pub fn decode_frame(frame: &[u8]) -> Result<SerializedRequest, IsolationError> {
    if frame.len() < 4 {
        return Err(IsolationError::new("WIRE_DECODE", "frame too short"));
    }
    let len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if frame.len() < 4 + len {
        return Err(IsolationError::new("WIRE_DECODE", "truncated frame"));
    }
    postcard::from_bytes(&frame[4..4 + len])
        .map_err(|e| IsolationError::new("WIRE_DECODE", e.to_string()))
}

#[allow(dead_code)]
fn _core_error_bridge(err: CoreError) -> IsolationError {
    IsolationError::new(&err.code, err.message)
}
