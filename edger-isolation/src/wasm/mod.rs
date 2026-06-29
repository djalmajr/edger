//! Wasm/wasmtime isolate backend (`--features wasm`).
//!
//! Standalone Wasm path (not co-located in V8 isolate). Enable with:
//! `cargo check -p edger-isolation --features wasm`

mod handler;
mod load;
mod wasi;

pub use handler::WasmHttpHandler;
pub use load::load_wasm_from_worker_dir;
pub use wasi::WasiConfig;

use async_trait::async_trait;

use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};

fn not_impl(method: &str) -> IsolationError {
    IsolationError::new(
        "NOT_IMPLEMENTED",
        format!("WasmIsolate::{method} pending wasmtime WASI runtime (see spike.md)"),
    )
}

/// Wasm isolate — `execute_wasm` uses wasmtime; other kinds remain stubbed (07.04).
pub struct WasmIsolate {
    wasi: WasiConfig,
    handler: WasmHttpHandler,
    wasm_bytes: Option<Vec<u8>>,
}

impl WasmIsolate {
    pub fn new(wasi: WasiConfig) -> Self {
        Self {
            wasi,
            handler: WasmHttpHandler::new(),
            wasm_bytes: None,
        }
    }

    pub fn with_wasm_bytes(mut self, bytes: Vec<u8>) -> Self {
        self.wasm_bytes = Some(bytes);
        self
    }

    pub fn wasi_config(&self) -> &WasiConfig {
        &self.wasi
    }
}

#[async_trait]
impl Isolate for WasmIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_fetch"))
    }

    async fn execute_routes(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_routes"))
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("serve_static_spa"))
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let _ = &req;
        if self.wasm_bytes.is_none() {
            if let (Some(dir), Some(entry)) =
                (config.worker_dir.as_ref(), config.entrypoint.as_deref())
            {
                self.wasm_bytes = Some(load_wasm_from_worker_dir(dir, entry)?);
            }
        }
        let bytes = self.wasm_bytes.as_ref().ok_or_else(|| {
            IsolationError::new(
                "WASM_NOT_LOADED",
                "no wasm module bytes configured on WasmIsolate",
            )
        })?;
        self.handler.execute_module(bytes)
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Err(not_impl("notify_idle"))
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Err(not_impl("terminate"))
    }
}
