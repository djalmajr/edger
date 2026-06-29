//! edger main binary — HTTP listener with health/readiness + pipeline (story 05.01–05.03).
//!
//! Environment:
//! - `PORT` — listen port (default `3000`)

use std::sync::Arc;

use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, port_from_env, serve, HookRunner, ManifestIndex, OrchestratorState,
    ServerConfig, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tracing_subscriber::EnvFilter;

struct StubIsolateFactory;

impl IsolateFactory for StubIsolateFactory {
    fn create_isolate(&self) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("edger_orchestrator=info".parse()?))
        .init();

    let port = port_from_env();
    let config = ServerConfig::from_port(port);
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubIsolateFactory));
    server.mark_ready(pool.clone());

    let state = OrchestratorState {
        server: server.clone(),
        pool,
        index: ManifestIndex::new(),
        hooks: HookRunner,
    };

    let shutdown_server = server.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("shutdown signal received");
            shutdown_server.shutdown_pool();
        }
    });

    serve(config, build_pipeline(state)).await
}