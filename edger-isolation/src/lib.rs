//! edger-isolation — execution backends (mock, deno, wasm).
//!
//! Depends only on `edger-core`. Real embedders added behind feature flags.

pub mod error;
pub mod isolate;
pub mod kinds;
pub mod mock;

pub use error::IsolationBackendError;
pub use isolate::Isolate;
pub use kinds::dispatch_execution;
pub use mock::MockIsolate;

// Re-export core wire types for convenience.
pub use edger_core::{IsolationError, SerializedRequest, SerializedResponse};
