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
    /// Fullstack/SSR: adapter-specific framework build output.
    Fullstack {
        adapter: String,
    },
}

pub const SUPPORTED_FULLSTACK_ADAPTERS: [&str; 10] = [
    "astro",
    "fresh",
    "hono",
    "lume",
    "nextjs",
    "nuxt",
    "remix",
    "solidstart",
    "sveltekit",
    "tanstack",
];

/// Fullstack adapter whose backend is a WASM module instead of a JS process
/// (EDG-53). Additional to `SUPPORTED_FULLSTACK_ADAPTERS`, which keeps the
/// ten JS/TS adapters unchanged.
pub const WASM_FULLSTACK_ADAPTER: &str = "wasm";

pub fn normalize_fullstack_adapter(adapter: &str) -> Option<&'static str> {
    match adapter.trim().to_ascii_lowercase().as_str() {
        "astro" => Some("astro"),
        "fresh" => Some("fresh"),
        "hono" => Some("hono"),
        "lume" => Some("lume"),
        "next" | "nextjs" => Some("nextjs"),
        "nuxt" | "nuxtjs" => Some("nuxt"),
        "react-router" | "reactrouter" | "remix" => Some("remix"),
        "solid" | "solidstart" => Some("solidstart"),
        "svelte" | "sveltekit" => Some("sveltekit"),
        "tanstack" | "tanstack-start" | "tanstackstart" => Some("tanstack"),
        "wasm" => Some(WASM_FULLSTACK_ADAPTER),
        _ => None,
    }
}

impl ExecutionKind {
    /// `false` only for `Fullstack { adapter: "wasm" }` (EDG-53); the ten
    /// JS/TS fullstack adapters keep a process backend.
    pub fn uses_process_backend(&self) -> bool {
        match self {
            Self::FetchHandler | Self::RoutesTable => true,
            Self::Fullstack { adapter } => adapter != WASM_FULLSTACK_ADAPTER,
            Self::StaticSpa { .. } | Self::WasmModule { .. } => false,
        }
    }

    /// Parse explicit manifest `kind` string into normalized enum.
    pub fn from_manifest_kind(kind: &str) -> Option<Self> {
        Self::from_manifest_kind_with_adapter(kind, None)
    }

    /// Parse explicit manifest `kind` string plus fullstack adapter into a normalized enum.
    pub fn from_manifest_kind_with_adapter(kind: &str, adapter: Option<&str>) -> Option<Self> {
        match kind.trim().to_ascii_lowercase().as_str() {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fullstack(adapter: &str) -> ExecutionKind {
        ExecutionKind::Fullstack {
            adapter: adapter.into(),
        }
    }

    #[test]
    fn wasm_adapter_is_recognized_in_addition_to_the_js_adapters() {
        assert_eq!(normalize_fullstack_adapter("wasm"), Some("wasm"));
        assert_eq!(normalize_fullstack_adapter(" WASM "), Some("wasm"));
        assert!(!SUPPORTED_FULLSTACK_ADAPTERS.contains(&"wasm"));
        assert_eq!(SUPPORTED_FULLSTACK_ADAPTERS.len(), 10);
        for adapter in SUPPORTED_FULLSTACK_ADAPTERS {
            assert_eq!(normalize_fullstack_adapter(adapter), Some(adapter));
        }
        assert_eq!(normalize_fullstack_adapter("wasm2"), None);
    }

    #[test]
    fn only_fullstack_wasm_skips_the_process_backend() {
        assert!(!fullstack("wasm").uses_process_backend());
        for adapter in SUPPORTED_FULLSTACK_ADAPTERS {
            assert!(fullstack(adapter).uses_process_backend(), "{adapter}");
        }
        // Control: unchanged classification of the other kinds.
        assert!(fullstack("").uses_process_backend());
        assert!(ExecutionKind::FetchHandler.uses_process_backend());
        assert!(ExecutionKind::RoutesTable.uses_process_backend());
        assert!(!ExecutionKind::StaticSpa { inject_base: true }.uses_process_backend());
        assert!(!ExecutionKind::WasmModule { entry: None }.uses_process_backend());
    }

    #[test]
    fn fullstack_kind_with_wasm_adapter_keeps_the_existing_variant() {
        assert_eq!(
            ExecutionKind::from_manifest_kind_with_adapter("fullstack", Some("wasm")),
            Some(fullstack("wasm"))
        );
        assert_eq!(
            ExecutionKind::from_manifest_kind_with_adapter("ssr", Some("wasm")),
            Some(fullstack("wasm"))
        );
        assert_eq!(
            ExecutionKind::from_manifest_kind("wasm"),
            Some(ExecutionKind::WasmModule { entry: None })
        );
    }
}
