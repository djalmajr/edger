//! Request and extension contexts (pure data).

use std::time::Instant;

use serde_json::Value;

use crate::principal::ApiKeyPrincipal;
use crate::worker_ref::WorkerRef;

/// Opaque server handle for extensions (orchestrator provides real impl).
#[derive(Clone, Debug, Default)]
pub struct ServerHandle {
    pub listen_addr: Option<String>,
}

/// Per-extension configuration context.
#[derive(Clone, Debug, Default)]
pub struct ExtensionContext {
    pub config: Value,
    pub global_config: Value,
}

/// Per-request context passed to hooks.
#[derive(Clone, Debug)]
pub struct RequestContext {
    pub request_id: String,
    pub principal: Option<ApiKeyPrincipal>,
    pub worker: Option<WorkerRef>,
    pub start: Instant,
}

impl RequestContext {
    pub fn new(request_id: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            principal: None,
            worker: None,
            start: Instant::now(),
        }
    }
}
