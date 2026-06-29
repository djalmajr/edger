//! edger main binary — HTTP listener with health/readiness + pipeline (story 05.01–05.05).
//!
//! Environment:
//! - `PORT` — listen port (default `3000`)
//! - `ROOT_API_KEY` — synthetic root principal (optional)
//! - `EDGER_AUTH_DB` — SQLite path for API keys (default in-memory if unset)

use std::sync::Arc;

use edger_core::ExtensionContext;
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, collect_extensions, port_from_env, run_on_init, run_on_server_start,
    run_on_shutdown, serve, AuthGate, ManifestIndex, OrchestratorState, ServerConfig, ServerState,
    SqliteApiKeyStore,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tracing_subscriber::EnvFilter;

struct StubIsolateFactory;

impl IsolateFactory for StubIsolateFactory {
    fn create_isolate(&self) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn open_auth_store() -> anyhow::Result<Arc<SqliteApiKeyStore>> {
    if let Ok(path) = std::env::var("EDGER_AUTH_DB") {
        if !path.is_empty() {
            return Ok(Arc::new(SqliteApiKeyStore::open(path)?));
        }
    }
    Ok(Arc::new(SqliteApiKeyStore::in_memory()?))
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

    // Explicit extension registration (story 06.01) — add edger-ext-* exports here.
    let registry = collect_extensions(vec![])?;

    let auth_store = open_auth_store()?;
    run_on_init(&registry, &mut ExtensionContext::default())?;
    run_on_server_start(&registry, &edger_core::ServerHandle::default());

    let state = OrchestratorState {
        server: server.clone(),
        pool,
        index: ManifestIndex::new(),
        registry,
        auth: AuthGate::from_env(auth_store),
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
