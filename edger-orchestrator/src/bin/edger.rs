//! edger main binary — HTTP listener with health/readiness + pipeline (story 05.01–06.02).
//!
//! Environment:
//! - `PORT` — listen port (default `3000`)
//! - `ROOT_API_KEY` — synthetic root principal (optional; handled by edger-ext-auth)
//! - `EDGER_AUTH_DB` — SQLite path for API keys (default in-memory if unset)

use std::sync::Arc;

use edger_core::ExtensionContext;
use edger_ext_auth::AuthExtension;
use edger_ext_gateway::GatewayExtension;
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, collect_extensions, port_from_env, run_on_init, run_on_server_start,
    run_on_shutdown, serve, AuthGate, AuthGateConfig, ManifestIndex, OrchestratorState,
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
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("edger_orchestrator=info".parse()?),
        )
        .init();

    let port = port_from_env();
    let config = ServerConfig::from_port(port);
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubIsolateFactory));
    server.mark_ready(pool.clone());

    let auth_ext = AuthExtension::from_env()?.into_arc();
    let mut registry = collect_extensions(vec![GatewayExtension::middleware()])?;
    registry.register_auth_provider(auth_ext.clone())?;
    run_on_init(&registry, &mut ExtensionContext::default())?;
    run_on_server_start(&registry, &edger_core::ServerHandle::default());

    let state = OrchestratorState {
        server: server.clone(),
        pool,
        index: ManifestIndex::new(),
        registry,
        auth: AuthGate::new(AuthGateConfig::default(), auth_ext),
    };

    let shutdown_registry = state.registry.clone();
    let shutdown_server = server.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("shutdown signal received");
            let _ = run_on_shutdown(&shutdown_registry);
            shutdown_server.shutdown_pool();
        }
    });

    serve(config, build_pipeline(state)).await
}
