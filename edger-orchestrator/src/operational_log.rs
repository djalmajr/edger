//! Structured operational logs with redaction by construction.

use axum::http::StatusCode;
use edger_core::CoreError;

pub fn log_operational_error(
    surface: &str,
    request_id: Option<&str>,
    status: StatusCode,
    err: &CoreError,
) {
    let request_id = request_id.unwrap_or("unknown");
    tracing::warn!(
        target: "edger.operational",
        surface,
        request_id,
        status = status.as_u16(),
        code = %err.code,
        "operational request failed"
    );
}
