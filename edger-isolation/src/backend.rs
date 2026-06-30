//! Backend selection factory — Mock always available; Deno/Wasm behind features.

use edger_core::{Isolate, WorkerConfig};

use crate::mock::MockIsolate;

#[cfg(feature = "deno")]
use crate::deno::{DenoFacade, DenoIsolate};

#[cfg(feature = "wasm")]
use crate::wasm::WasmIsolate;

/// Selectable isolation backend (Epic 03 dual-backend prep).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationBackend {
    Mock,
    #[cfg(feature = "deno")]
    Deno,
    #[cfg(feature = "wasm")]
    Wasm,
}

/// Creates a boxed isolate for the requested backend.
pub fn create_isolate(backend: IsolationBackend, config: &WorkerConfig) -> Box<dyn Isolate> {
    #[cfg(not(feature = "wasm"))]
    let _ = config;

    match backend {
        IsolationBackend::Mock => Box::new(MockIsolate::new()),
        #[cfg(feature = "deno")]
        IsolationBackend::Deno => Box::new(DenoIsolate::new(DenoFacade::new())),
        #[cfg(feature = "wasm")]
        IsolationBackend::Wasm => Box::new(WasmIsolate::from_worker_config(config)),
    }
}
