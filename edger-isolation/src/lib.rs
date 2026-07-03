//! edger-isolation — execution backends (mock, deno, wasm).
//!
//! Depends only on `edger-core`. Real embedders added behind feature flags:
//! - `cargo check -p edger-isolation --features deno` — DenoIsolate skeleton
//! - `cargo check -p edger-isolation --features wasm` — WasmIsolate skeleton

pub mod backend;
pub mod error;
pub mod fullstack;
pub mod isolate;
pub mod kinds;
pub mod limits;
pub mod mock;
pub mod static_spa;
pub mod wire;

#[cfg(any(feature = "deno", feature = "multiproc"))]
#[path = "deno/bundle.rs"]
mod deno_bundle;

#[cfg(feature = "deno")]
pub mod deno;

#[cfg(feature = "multiproc")]
pub mod multiproc;

#[cfg(feature = "wasm")]
pub mod wasm;

pub use backend::{create_isolate, IsolationBackend};
pub use error::IsolationBackendError;
pub use fullstack::{
    dispatch_fullstack_buffered, dispatch_fullstack_stream, prepare_fullstack_request,
    try_serve_fullstack_asset,
};
pub use isolate::Isolate;
pub use kinds::dispatch_execution;
pub use limits::{execute_with_limits, CpuTimer, LimitGuard, ResourceLimits};
pub use mock::MockIsolate;
pub use wire::{decode_frame, encode_frame, validate_request};

#[cfg(feature = "deno")]
pub use deno::{DenoFacade, DenoIsolate};

#[cfg(feature = "multiproc")]
pub use multiproc::{DenoProcessIsolate, DenoWorkerProcess};

#[cfg(feature = "wasm")]
pub use wasm::{WasiConfig, WasmHttpHandler, WasmIsolate};

// Re-export core types for convenience.
pub use edger_core::{ExecutionKind, IsolationError, SerializedRequest, SerializedResponse};
