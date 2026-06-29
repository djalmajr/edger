//! edger-ext-gateway — Middleware template extension (Epic 06.03).
//!
//! Copy this crate to scaffold new `edger-ext-*` middleware. Implements **only**
//! `Middleware` (choose ONE — do not add `AuthProvider` here).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::Result;
use edger_core::{
    Extension, ExtensionContext, Middleware, RequestContext, SerializedRequest, SerializedResponse,
};
use tracing::trace;

const GATEWAY_TEST_HEADER: &str = "x-gateway-test";

/// Minimal gateway middleware — logs and pass-through (no proxy).
pub struct GatewayExtension {
    prefix: String,
    invocations: Arc<AtomicU32>,
}

impl GatewayExtension {
    pub fn new() -> Self {
        Self::with_prefix("")
    }

    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            invocations: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Factory for explicit bin registration (story 06.01 pattern).
    pub fn middleware() -> Arc<dyn Middleware> {
        Arc::new(Self::new())
    }

    pub fn invocation_count(&self) -> u32 {
        self.invocations.load(Ordering::SeqCst)
    }

    fn has_test_header(req: &SerializedRequest) -> bool {
        req.headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(GATEWAY_TEST_HEADER))
    }
}

impl Default for GatewayExtension {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for GatewayExtension {
    fn name(&self) -> &'static str {
        "gateway"
    }

    fn priority(&self) -> i32 {
        0
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        trace!(extension = self.name(), "gateway extension initialized");
        Ok(())
    }
}

impl Middleware for GatewayExtension {
    fn on_request(
        &self,
        req: &mut SerializedRequest,
        _ctx: &RequestContext,
    ) -> Result<Option<SerializedResponse>> {
        if Self::has_test_header(req) {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            trace!(
                extension = self.name(),
                uri = %req.uri,
                prefix = %self.prefix,
                "gateway on_request (test header)"
            );
        }
        Ok(None)
    }
}
