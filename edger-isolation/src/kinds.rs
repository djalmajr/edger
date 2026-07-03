//! Dispatch helper matching ExecutionKind to Isolate methods.

use edger_core::{ExecutionKind, Isolate, SerializedRequest, SerializedResponse, WorkerConfig};

/// Central dispatch used by WorkerPool (Epic 04) and tests.
pub async fn dispatch_execution<I: Isolate + ?Sized>(
    isolate: &mut I,
    kind: ExecutionKind,
    req: SerializedRequest,
    config: &WorkerConfig,
) -> Result<SerializedResponse, edger_core::IsolationError> {
    match kind {
        ExecutionKind::FetchHandler => isolate.execute_fetch(req, config).await,
        ExecutionKind::RoutesTable => isolate.execute_routes(req, config).await,
        ExecutionKind::StaticSpa { inject_base } => {
            let base = if inject_base {
                Some(req.base_href.as_deref().unwrap_or("/"))
            } else {
                None
            };
            isolate.serve_static_spa(&req.uri, base, config).await
        }
        ExecutionKind::WasmModule { .. } => isolate.execute_wasm(req, config).await,
        ExecutionKind::Fullstack { adapter } => Ok(SerializedResponse {
            status: 501,
            headers: vec![("x-adapter".into(), adapter)],
            body: Some(bytes::Bytes::from_static(b"fullstack not implemented")),
        }),
    }
}
