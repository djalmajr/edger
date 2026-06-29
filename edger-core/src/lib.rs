//! edger-core: pure vocabulary (types, errors, traits, manifests).
//! No I/O. Leaf crate. All higher crates depend on this.

use serde::{Deserialize, Serialize};

/// Execution kind for a worker (from design).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExecutionKind {
    FetchHandler,
    RoutesTable,
    StaticSpa { inject_base: bool },
    WasmModule { entry: Option<String> },
    Fullstack { adapter: String },
}

/// Basic error type for core (pure, no I/O).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoreError {
    pub code: String,
    pub message: String,
}

impl CoreError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
        }
    }
}

/// Minimal worker manifest subset (pure data).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerManifest {
    pub name: String,
    pub entrypoint: Option<String>,
    pub ttl: u64, // 0 = ephemeral
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_kind_roundtrips() {
        let kind = ExecutionKind::FetchHandler;
        let json = serde_json::to_string(&kind).unwrap();
        let back: ExecutionKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }

    #[test]
    fn core_error_and_manifest_are_pure() {
        let err = CoreError::new("TEST", "pure error");
        assert_eq!(err.code, "TEST");

        let m = WorkerManifest {
            name: "hello".into(),
            entrypoint: Some("index.ts".into()),
            ttl: 0,
        };
        assert_eq!(m.name, "hello");
    }
}
