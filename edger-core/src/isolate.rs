//! Isolate execution trait (implemented by edger-isolation backends).

use async_trait::async_trait;

use crate::config::WorkerConfig;
use crate::error::IsolationError;
use crate::wire::{SerializedRequest, SerializedResponse, WorkerResponse};

/// Core trait implemented by concrete isolate backends.
#[async_trait]
pub trait Isolate: Send + Sync {
    async fn prepare(&mut self, _config: &WorkerConfig) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError>;

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError>;

    async fn serve_static_spa(
        &mut self,
        path: &str,
        base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError>;

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError>;

    /// Streaming variants (story 16.D): backends that can stream the response
    /// body incrementally override these; the default buffers via the regular
    /// methods so existing isolates are untouched.
    async fn execute_fetch_stream(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.execute_fetch(req, config)
            .await
            .map(WorkerResponse::Buffered)
    }

    async fn execute_routes_stream(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.execute_routes(req, config)
            .await
            .map(WorkerResponse::Buffered)
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}
