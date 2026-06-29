//! Deno/V8 isolate backend (`--features deno`).
//!
//! Real deno_core embedding deferred to PR 10. Enable with:
//! `cargo check -p edger-isolation --features deno`

mod bundle;
mod facade;

pub use bundle::{BundleFormat, ModuleBundle, ModuleBundler, StubBundler};
pub use facade::DenoFacade;

use async_trait::async_trait;

use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};

fn not_impl(method: &str) -> IsolationError {
    IsolationError::new(
        "NOT_IMPLEMENTED",
        format!("DenoIsolate::{method} pending deno_core boot (see spike.md)"),
    )
}

/// JS/TS isolate skeleton — all methods stubbed until V8 platform boots.
pub struct DenoIsolate {
    facade: DenoFacade,
    bundler: StubBundler,
}

impl DenoIsolate {
    pub fn new(facade: DenoFacade) -> Self {
        Self {
            facade,
            bundler: StubBundler,
        }
    }

    pub fn facade(&self) -> &DenoFacade {
        &self.facade
    }

    pub fn bundler(&self) -> &StubBundler {
        &self.bundler
    }
}

#[async_trait]
impl Isolate for DenoIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_fetch"))
    }

    async fn execute_routes(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_routes"))
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("serve_static_spa"))
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_wasm"))
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Err(not_impl("notify_idle"))
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Err(not_impl("terminate"))
    }
}
