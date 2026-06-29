//! Execution kinds for worker dispatch.

use serde::{Deserialize, Serialize};

/// How the orchestrator / pool tells the isolate what to run.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExecutionKind {
    FetchHandler,
    RoutesTable,
    StaticSpa {
        inject_base: bool,
    },
    WasmModule {
        entry: Option<String>,
    },
    /// Fullstack/SSR: adapter-specific (e.g. "next" or custom export "handleSsr").
    Fullstack {
        adapter: String,
    },
}

impl ExecutionKind {
    /// Parse explicit manifest `kind` string into normalized enum.
    pub fn from_manifest_kind(kind: &str) -> Option<Self> {
        match kind.to_ascii_lowercase().as_str() {
            "serverless" | "fetch" => Some(Self::FetchHandler),
            "routes" | "backend" => Some(Self::RoutesTable),
            "spa" | "static" => Some(Self::StaticSpa { inject_base: true }),
            "wasm" => Some(Self::WasmModule { entry: None }),
            "ssr" | "fullstack" => Some(Self::Fullstack {
                adapter: "default".into(),
            }),
            _ => None,
        }
    }
}
