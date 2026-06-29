//! edger-isolation — execution backends (mock, deno, wasm).
//!
//! Depends only on `edger-core`. Real embedders added behind feature flags.

pub mod error;
pub mod isolate;
pub mod kinds;
pub mod limits;
pub mod mock;
pub mod transport;
pub mod wire;

pub use error::IsolationBackendError;
pub use isolate::Isolate;
pub use kinds::dispatch_execution;
pub use limits::{execute_with_limits, CpuTimer, LimitGuard, ResourceLimits};
pub use mock::MockIsolate;
pub use transport::{InProcessTransport, IsolateTransport, UdsTransport};
pub use wire::{decode_frame, encode_frame, validate_request};

// Re-export core types for convenience.
pub use edger_core::{ExecutionKind, IsolationError, SerializedRequest, SerializedResponse};
