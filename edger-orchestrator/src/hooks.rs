//! Hook chain execution — on_request short-circuit and lifecycle (story 05.05).

use anyhow::Result;
use edger_core::{
    ExtensionContext, RequestContext, SerializedRequest, SerializedResponse, ServerHandle,
};

use crate::registry::ExtensionRegistry;

/// Run `on_request` hooks in priority order; short-circuit on first `Some(response)`.
pub fn run_on_request(
    registry: &ExtensionRegistry,
    req: &mut SerializedRequest,
    ctx: &RequestContext,
) -> Result<Option<SerializedResponse>> {
    for middleware in registry.middlewares() {
        if let Some(response) = middleware.on_request(req, ctx)? {
            return Ok(Some(response));
        }
    }
    Ok(None)
}

/// Run `on_response` hooks in reverse priority order (outermost first).
pub fn run_on_response(
    registry: &ExtensionRegistry,
    res: &mut SerializedResponse,
    ctx: &RequestContext,
) {
    for middleware in registry.middlewares().iter().rev() {
        middleware.on_response(res, ctx);
    }
}

pub fn run_on_init(registry: &ExtensionRegistry, ctx: &mut ExtensionContext) -> Result<()> {
    for ext in registry.middlewares() {
        ext.on_init(ctx)?;
    }
    Ok(())
}

pub fn run_on_server_start(registry: &ExtensionRegistry, server: &ServerHandle) {
    for ext in registry.middlewares() {
        ext.on_server_start(server);
    }
}

pub fn run_on_shutdown(registry: &ExtensionRegistry) -> Result<()> {
    for ext in registry.middlewares() {
        ext.on_shutdown()?;
    }
    Ok(())
}