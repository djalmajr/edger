//! Resource limits stubs (timeout real; mem/cpu placeholders).
//!
//! Future port: Edge Runtime `cpu_timer` and `base_mem_check` patterns.

use edger_core::{ExecutionKind, Isolate, SerializedRequest, SerializedResponse, WorkerConfig};

use crate::kinds::dispatch_execution;
use crate::wire::validate_request;

/// Configurable resource limits applied before isolate dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    pub memory_mb: Option<u32>,
    pub cpu_time_ms: Option<u64>,
    pub wall_timeout_ms: u64,
    pub low_memory: bool,
}

impl ResourceLimits {
    pub fn from_config(config: &WorkerConfig) -> Self {
        Self {
            memory_mb: if config.low_memory {
                Some(128)
            } else {
                Some(512)
            },
            cpu_time_ms: Some(config.timeout_ms),
            wall_timeout_ms: config.timeout_ms,
            low_memory: config.low_memory,
        }
    }
}

/// RAII guard placeholder for memory/cpu accounting.
pub struct LimitGuard {
    #[allow(dead_code)]
    limits: ResourceLimits,
}

impl LimitGuard {
    pub fn new(limits: ResourceLimits) -> Self {
        Self { limits }
    }

    /// Stub — real accounting in supervisor (Epic 04).
    pub fn check_memory(&self) -> Result<(), edger_core::IsolationError> {
        Ok(())
    }
}

/// `CpuTimer` stub — see Edge Runtime cpu_timer for future port.
pub struct CpuTimer;

impl CpuTimer {
    pub fn new() -> Self {
        Self
    }
}

/// Execute dispatch with validation + wall-clock timeout.
pub async fn execute_with_limits<I: Isolate + ?Sized>(
    isolate: &mut I,
    kind: ExecutionKind,
    req: SerializedRequest,
    config: &WorkerConfig,
    limits: &ResourceLimits,
) -> Result<SerializedResponse, edger_core::IsolationError> {
    validate_request(&req, config)?;
    let _guard = LimitGuard::new(limits.clone());
    _guard.check_memory()?;

    let timeout = std::time::Duration::from_millis(limits.wall_timeout_ms);
    match tokio::time::timeout(timeout, dispatch_execution(isolate, kind, req, config)).await {
        Ok(inner) => inner,
        Err(_) => Err(edger_core::IsolationError::new(
            "TIMEOUT",
            format!("exceeded {}ms", limits.wall_timeout_ms),
        )),
    }
}
