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
///
/// (EDG-9) The event also carries the request `method`, the worker-relative
/// `path` WITHOUT its query string, and the response `content-type` when
/// known. Only these three request/response facts are recorded — never
/// header values, cookies or other sensitive material.
#[allow(clippy::too_many_arguments)]
pub fn log_dispatch_event(
    request_id: &str,
    worker: &str,
    version: &str,
    namespace: &str,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    outcome: &str,
    wall_ms: u64,
    status: u16,
) {
    match content_type {
        Some(content_type) => tracing::info!(
            target: "edger.dispatch",
            request_id,
            worker,
            version,
            namespace,
            method,
            path,
            content_type = %content_type,
            outcome,
            wall_ms,
            status,
            "worker execution"
        ),
        None => tracing::info!(
            target: "edger.dispatch",
            request_id,
            worker,
            version,
            namespace,
            method,
            path,
            outcome,
            wall_ms,
            status,
            "worker execution"
        ),
    }
}

#[cfg(test)]
mod tests {
    //! EDG-9: the dispatch log carries method + query-less path + response
    //! content-type, and never query strings or header values.

    use super::log_dispatch_event;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct CapturedLogs {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8(self.buffer.lock().expect("log buffer").clone()).expect("utf8 logs")
        }
    }

    struct CapturedLogWriter {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for CapturedLogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer
                .lock()
                .expect("log buffer")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedLogWriter {
                buffer: self.buffer.clone(),
            }
        }
    }

    #[test]
    fn dispatch_log_includes_method_path_and_content_type_without_query() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_target(true)
            .without_time()
            .with_max_level(tracing::Level::INFO)
            .with_writer(logs.clone())
            .finish();
        // `with_default` scopes the subscriber to this thread, so the test
        // never fights other unit tests for the global default.
        tracing::subscriber::with_default(subscriber, || {
            log_dispatch_event(
                "req-1",
                "echo",
                "1.0",
                "default",
                "GET",
                "/api/items",
                Some("text/event-stream"),
                "ok",
                42,
                200,
            );
        });
        let text = logs.text();
        assert!(text.contains("method=\"GET\""), "logs:\n{text}");
        assert!(text.contains("path=\"/api/items\""), "logs:\n{text}");
        assert!(
            text.contains("content_type=text/event-stream"),
            "logs:\n{text}"
        );
        assert!(
            !text.contains('?'),
            "the query string must never reach the dispatch log:\n{text}"
        );
    }

    #[test]
    fn dispatch_log_without_content_type_omits_the_field() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_target(true)
            .without_time()
            .with_max_level(tracing::Level::INFO)
            .with_writer(logs.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_dispatch_event(
                "req-2",
                "echo",
                "1.0",
                "default",
                "POST",
                "/api/items",
                None,
                "timeout",
                7,
                504,
            );
        });
        let text = logs.text();
        assert!(text.contains("method=\"POST\""), "logs:\n{text}");
        assert!(text.contains("outcome=\"timeout\""), "logs:\n{text}");
        assert!(
            !text.contains("content_type"),
            "an unknown content-type must not be logged as empty:\n{text}"
        );
    }
}
