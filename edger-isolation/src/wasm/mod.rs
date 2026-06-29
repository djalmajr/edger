//! Wasm/wasmtime isolate backend (`--features wasm`).
//!
//! Standalone Wasm path (not co-located in V8 isolate). Enable with:
//! `cargo check -p edger-isolation --features wasm`

mod wasi;

pub use wasi::WasiConfig;

use async_trait::async_trait;

use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};

fn not_impl(method: &str) -> IsolationError {
    IsolationError::new(
        "NOT_IMPLEMENTED",
        format!("WasmIsolate::{method} pending wasmtime WASI runtime (see spike.md)"),
    )
}

/// Wasm isolate skeleton — all methods stubbed until wasmtime engine wires in.
pub struct WasmIsolate {
    wasi: WasiConfig,
}

impl WasmIsolate {
    pub fn new(wasi: WasiConfig) -> Self {
        Self { wasi }
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
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_wasm"))
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Err(not_impl("notify_idle"))
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Err(not_impl("terminate"))
    }
}
