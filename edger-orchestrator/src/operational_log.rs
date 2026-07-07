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

/// Per-execution structured event (Epic 20.09): emitted once per worker
/// dispatch with the outcome and cost so a single request can be traced end to
/// end. `outcome` is "ok" on success or the error code (timeout/cpu/memory/
/// rate-limited/...) on failure. Feeds the OTLP exporter when linked.
#[allow(clippy::too_many_arguments)]
pub fn log_dispatch_event(
    request_id: &str,
    worker: &str,
    version: &str,
    namespace: &str,
    outcome: &str,
    wall_ms: u64,
    status: u16,
) {
    tracing::info!(
        target: "edger.dispatch",
        request_id,
        worker,
        version,
        namespace,
        outcome,
        wall_ms,
        status,
        "worker execution"
    );
}
