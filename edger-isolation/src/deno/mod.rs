//! Deno isolate backend (`--features deno`).
//!
//! The current backend uses the Deno CLI as a process-isolated execution bridge.
//! It keeps the Rust orchestrator and worker pool on the production path while
//! the embedded `deno_core` facade is still pending.

mod cli;
mod facade;

pub use crate::deno_bundle::{
    entry_needs_bundle, BundleFormat, DenoCliBundler, ModuleBundle, ModuleBundler,
};
pub use cli::DenoCliRunner;
pub use facade::DenoFacade;

use async_trait::async_trait;

use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};

use crate::static_spa::serve_static_spa;

fn not_impl(method: &str) -> IsolationError {
    IsolationError::new(
        "NOT_IMPLEMENTED",
        format!("DenoIsolate::{method} pending implementation"),
    )
}

/// JS/TS isolate backed by the Deno CLI bridge.
pub struct DenoIsolate {
    facade: DenoFacade,
    bundler: DenoCliBundler,
    runner: DenoCliRunner,
}

impl DenoIsolate {
    pub fn new(facade: DenoFacade) -> Self {
        Self {
            facade,
            bundler: DenoCliBundler::default(),
            runner: DenoCliRunner::default(),
        }
    }

    pub fn facade(&self) -> &DenoFacade {
        &self.facade
    }

    pub fn bundler(&self) -> &DenoCliBundler {
        &self.bundler
    }

    pub fn runner(&self) -> &DenoCliRunner {
        &self.runner
    }
}

#[async_trait]
impl Isolate for DenoIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.runner.execute_fetch(req, config)
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.runner.execute_fetch(req, config)
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        serve_static_spa(path, base_href, config)
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_wasm"))
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}
