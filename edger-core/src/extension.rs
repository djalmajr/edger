//! Extension system traits (Open/Closed; definitions only in core).

use anyhow::Result;

use crate::context::{ExtensionContext, RequestContext, ServerHandle};
use crate::wire::{SerializedRequest, SerializedResponse};
use crate::worker_ref::WorkerRef;

/// Base extension lifecycle hooks.
pub trait Extension: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn priority(&self) -> i32 {
        0
    }
    fn on_init(&self, ctx: &mut ExtensionContext) -> Result<()>;
    fn on_shutdown(&self) -> Result<()> {
        Ok(())
    }
    fn on_server_start(&self, _server: &ServerHandle) {}
}

/// Middleware hook trait (Buntime onRequest/onResponse).
pub trait Middleware: Extension {
    fn on_request(
        &self,
        req: &mut SerializedRequest,
        ctx: &RequestContext,
    ) -> Result<Option<SerializedResponse>>;

    fn on_response(&self, res: &mut SerializedResponse, ctx: &RequestContext) {
        let _ = (res, ctx);
    }
}

/// Serverless worker handler dispatched by the pool.
#[async_trait::async_trait]
pub trait WorkerHandler: Send + Sync {
    async fn handle(
        &self,
        req: SerializedRequest,
        worker: &WorkerRef,
    ) -> Result<SerializedResponse>;
}
