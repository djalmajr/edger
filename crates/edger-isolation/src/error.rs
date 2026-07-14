//! Isolation-layer errors (maps from edger-core wire/domain errors).

use edger_core::IsolationError as CoreIsolationError;

/// Backend-specific isolation errors for mock and future embedders.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum IsolationBackendError {
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("memory exceeded: {0}")]
    MemoryExceeded(String),
    #[error("module load: {0}")]
    ModuleLoad(String),
    #[error("wire: {0}")]
    Wire(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl IsolationBackendError {
    pub fn into_core(self) -> CoreIsolationError {
        let (code, message) = match self {
            Self::Timeout(m) => ("TIMEOUT", m),
            Self::MemoryExceeded(m) => ("MEMORY_EXCEEDED", m),
            Self::ModuleLoad(m) => ("MODULE_LOAD", m),
            Self::Wire(m) => ("WIRE", m),
            Self::Internal(m) => ("INTERNAL", m),
        };
        CoreIsolationError::new(code, message)
    }
}

impl From<edger_core::CoreError> for IsolationBackendError {
    fn from(err: edger_core::CoreError) -> Self {
        Self::Wire(err.message)
    }
}
