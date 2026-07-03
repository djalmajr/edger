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
    /// Fullstack/SSR: adapter-specific (`hono`, `sveltekit`, `tanstack`).
    Fullstack {
        adapter: String,
    },
}

pub const SUPPORTED_FULLSTACK_ADAPTERS: [&str; 3] = ["hono", "sveltekit", "tanstack"];

pub fn normalize_fullstack_adapter(adapter: &str) -> Option<&'static str> {
    match adapter.trim().to_ascii_lowercase().as_str() {
        "hono" => Some("hono"),
        "sveltekit" => Some("sveltekit"),
        "tanstack" => Some("tanstack"),
        _ => None,
    }
}

impl ExecutionKind {
    /// Parse explicit manifest `kind` string into normalized enum.
    pub fn from_manifest_kind(kind: &str) -> Option<Self> {
        Self::from_manifest_kind_with_adapter(kind, None)
    }

    /// Parse explicit manifest `kind` string plus fullstack adapter into a normalized enum.
    pub fn from_manifest_kind_with_adapter(kind: &str, adapter: Option<&str>) -> Option<Self> {
        match kind.to_ascii_lowercase().as_str() {
            "serverless" | "fetch" => Some(Self::FetchHandler),
            "routes" | "backend" => Some(Self::RoutesTable),
            "spa" | "static" => Some(Self::StaticSpa { inject_base: true }),
            "wasm" => Some(Self::WasmModule { entry: None }),
            "ssr" | "fullstack" => {
                let adapter = adapter.and_then(normalize_fullstack_adapter).unwrap_or("");
                Some(Self::Fullstack {
                    adapter: adapter.into(),
                })
            }
            _ => None,
        }
    }
}
