//! Extension system traits (Open/Closed; definitions only in core).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::{ExtensionContext, RequestContext, ServerHandle};
use crate::wire::{SerializedRequest, SerializedResponse};
use crate::worker_ref::WorkerRef;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionCapability {
    ApiKeys,
    AuthProvider,
    HostRouting,
    LifecycleHook { hook: ExtensionHook },
    MenuContribution { name: String },
    Middleware,
    RequestHook,
    ResponseHook,
    WorkerHandler,
}

impl ExtensionCapability {
    pub fn auth_provider() -> Self {
        Self::AuthProvider
    }

    pub fn label(&self) -> String {
        match self {
            Self::ApiKeys => "apiKeys".into(),
            Self::AuthProvider => "authProvider".into(),
            Self::HostRouting => "hostRouting".into(),
            Self::LifecycleHook { hook } => hook.label().into(),
            Self::MenuContribution { name } => format!("menu:{name}"),
            Self::Middleware => "middleware".into(),
            Self::RequestHook => "onRequest".into(),
            Self::ResponseHook => "onResponse".into(),
            Self::WorkerHandler => "workerHandler".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDependency {
    pub capability: ExtensionCapability,
}

impl ExtensionDependency {
    pub fn capability(capability: ExtensionCapability) -> Self {
        Self { capability }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionHook {
    OnInit,
    OnServerStart,
    OnShutdown,
    OnWorkerComplete,
    OnWorkerDispatch,
    OnWorkerError,
}

impl ExtensionHook {
    pub fn label(&self) -> &'static str {
        match self {
            Self::OnInit => "onInit",
            Self::OnServerStart => "onServerStart",
            Self::OnShutdown => "onShutdown",
            Self::OnWorkerComplete => "onWorkerComplete",
            Self::OnWorkerDispatch => "onWorkerDispatch",
            Self::OnWorkerError => "onWorkerError",
        }
    }
}

/// Base extension lifecycle hooks.
pub trait Extension: Send + Sync + 'static {
    fn capabilities(&self) -> Vec<ExtensionCapability> {
        vec![ExtensionCapability::LifecycleHook {
            hook: ExtensionHook::OnInit,
        }]
    }

    fn dependencies(&self) -> Vec<ExtensionDependency> {
        vec![]
    }

    fn name(&self) -> &'static str;

    fn priority(&self) -> i32 {
        0
    }

    fn on_init(&self, ctx: &mut ExtensionContext) -> Result<()>;

    fn on_shutdown(&self) -> Result<()> {
        Ok(())
    }

    fn on_server_start(&self, _server: &ServerHandle) {}

    fn diagnostics(&self) -> Option<Value> {
        None
    }
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

    fn on_worker_dispatch(&self, ctx: &RequestContext) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    fn on_worker_complete(&self, res: &SerializedResponse, ctx: &RequestContext) {
        let _ = (res, ctx);
    }

    fn on_worker_error(&self, error: &str, ctx: &RequestContext) {
        let _ = (error, ctx);
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
