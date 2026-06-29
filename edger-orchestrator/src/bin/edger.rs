//! edger main binary — HTTP listener with health/readiness (story 05.01).
//!
//! Environment:
//! - `PORT` — listen port (default `3000`)

use std::sync::Arc;

use edger_isolation::MockIsolate;
use edger_orchestrator::server::{port_from_env, serve, ServerConfig, ServerState};
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
    let state = ServerState::new_unready();

    // Stub init: empty pool marks readiness until manifest loading lands in 05.03+.
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubIsolateFactory));
    state.mark_ready(pool);

    let shutdown_state = state.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("shutdown signal received");
            shutdown_state.shutdown_pool();
        }
    });

    serve(config, state).await
}