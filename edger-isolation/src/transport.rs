//! Transport abstraction for in-process and future multi-process IPC.
//!
//! Multi-process rollout: `UdsTransport` will send length-prefixed postcard frames
//! (see `wire::encode_frame`) over Unix domain sockets; supervisor spawns child
//! isolates and enforces `ResourceLimits` at the boundary.

use edger_core::{IsolationError, SerializedRequest, SerializedResponse};

use edger_core::ExecutionKind;

use crate::dispatch_execution;
use crate::mock::MockIsolate;

/// Transport surface for isolate execution.
pub trait IsolateTransport: Send {
    fn execute(
        &mut self,
        kind: ExecutionKind,
        req: SerializedRequest,
    ) -> impl std::future::Future<Output = Result<SerializedResponse, IsolationError>> + Send;
}

/// Direct in-process call to a local `MockIsolate` (dev/tests).
pub struct InProcessTransport {
    isolate: MockIsolate,
    config: edger_core::WorkerConfig,
}

impl InProcessTransport {
    pub fn new(isolate: MockIsolate, config: edger_core::WorkerConfig) -> Self {
        Self { isolate, config }
    }
}

impl IsolateTransport for InProcessTransport {
    async fn execute(
        &mut self,
        kind: ExecutionKind,
        req: SerializedRequest,
    ) -> Result<SerializedResponse, IsolationError> {
        dispatch_execution(&mut self.isolate, kind, req, &self.config).await
    }
}

/// UDS transport stub — behind `multiproc` feature.
#[cfg(feature = "multiproc")]
pub struct UdsTransport {
    path: String,
}

#[cfg(feature = "multiproc")]
impl UdsTransport {
    pub fn connect(path: impl Into<String>) -> Result<Self, IsolationError> {
        Ok(Self { path: path.into() })
    }

    pub async fn send_frame(&self, _frame: &[u8]) -> Result<(), IsolationError> {
        Err(IsolationError::new(
            "NOT_IMPLEMENTED",
            format!("UdsTransport not implemented: {}", self.path),
        ))
    }
}

#[cfg(not(feature = "multiproc"))]
pub struct UdsTransport;

#[cfg(not(feature = "multiproc"))]
impl UdsTransport {
    pub fn connect(_path: impl Into<String>) -> Result<Self, IsolationError> {
        Err(IsolationError::new(
            "NOT_IMPLEMENTED",
            "enable feature multiproc for UdsTransport",
        ))
    }
}
