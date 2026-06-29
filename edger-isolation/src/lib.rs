//! edger-isolation — execution backends (mock, deno, wasm).
//!
//! Depends only on `edger-core`. Real embedders added behind feature flags:
//! - `cargo check -p edger-isolation --features deno` — DenoIsolate skeleton
//! - `cargo check -p edger-isolation --features wasm` — WasmIsolate skeleton

pub mod backend;
pub mod error;
pub mod isolate;
pub mod kinds;
pub mod limits;
pub mod mock;
pub mod transport;
pub mod wire;

#[cfg(feature = "deno")]
pub mod deno;

#[cfg(feature = "wasm")]
pub mod wasm;

pub use backend::{create_isolate, IsolationBackend};
pub use error::IsolationBackendError;
pub use isolate::Isolate;
pub use kinds::dispatch_execution;
pub use limits::{execute_with_limits, CpuTimer, LimitGuard, ResourceLimits};
pub use mock::MockIsolate;
pub use transport::{InProcessTransport, IsolateTransport, UdsTransport};
pub use wire::{decode_frame, encode_frame, validate_request};

#[cfg(feature = "deno")]
pub use deno::DenoIsolate;

#[cfg(feature = "wasm")]
pub use wasm::{WasiConfig, WasmHttpHandler, WasmIsolate};

// Re-export core types for convenience.
pub use edger_core::{ExecutionKind, IsolationError, SerializedRequest, SerializedResponse};
