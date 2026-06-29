//! Module bundling / eszip hooks (Edge Runtime `eszip_trait` alignment — PR 10).

use edger_core::IsolationError;

/// Loaded module bundle metadata (eszip or precompiled artifact).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleBundle {
    pub path: String,
    pub format: BundleFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleFormat {
    Eszip,
    Precompiled,
}

/// Loads worker modules from eszip or precompiled paths (PR 10).
pub trait ModuleBundler: Send + Sync {
    fn load_eszip(&self, path: &str) -> Result<ModuleBundle, IsolationError>;
    fn load_precompiled(&self, path: &str) -> Result<ModuleBundle, IsolationError>;
}

/// Stub bundler — always returns NOT_IMPLEMENTED until eszip parser lands.
#[derive(Debug, Clone, Default)]
pub struct StubBundler;

impl ModuleBundler for StubBundler {
    fn load_eszip(&self, path: &str) -> Result<ModuleBundle, IsolationError> {
        Err(IsolationError::new(
            "NOT_IMPLEMENTED",
            format!("StubBundler::load_eszip({path}) pending eszip parser (PR 10)"),
        ))
    }

    fn load_precompiled(&self, path: &str) -> Result<ModuleBundle, IsolationError> {
        Err(IsolationError::new(
            "NOT_IMPLEMENTED",
            format!("StubBundler::load_precompiled({path}) pending precomp loader (PR 10)"),
        ))
    }
}
