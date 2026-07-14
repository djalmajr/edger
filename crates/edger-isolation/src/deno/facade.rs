//! Deno facade configuration stub (Edge Runtime `deno_facade` alignment — PR 10).

/// Facade settings for deno_core embedding (ops registration deferred).
#[derive(Debug, Clone, Default)]
pub struct DenoFacade {
    /// When true, register stub ops for dev harness (no V8 yet).
    pub register_stub_ops: bool,
}

impl DenoFacade {
    pub fn new() -> Self {
        Self::default()
    }

    /// Placeholder for module loader registration (real impl in PR 10).
    pub fn module_loader_registered(&self) -> bool {
        false
    }
}
