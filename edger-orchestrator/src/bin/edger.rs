//! edger main binary — HTTP listener with health/readiness + pipeline (story 05.01–06.02).
//!
//! Environment:
//! - `PORT` — listen port (default `3000`)
//! - `RUNTIME_WORKER_DIRS` — `:` separated worker roots (default `workers`)
//! - `ROOT_API_KEY` — synthetic root principal (optional; handled by edger-ext-auth)
//! - `EDGER_AUTH_DB` — SQLite path for API keys (default in-memory if unset)
//! - `EDGER_DURABLE_SQL_PROVIDER` — `local` (default), `turso-remote`, or `turso-sync`
//! - `EDGER_STATE_DIR` — directory for local SQL/KV/queue state (default in-memory if unset)
//! - `EDGER_EXTENSION_STATUS_FILE` — JSON overlay for runtime extension enable/disable status
//! - `EDGER_TURSO_*` — remote/sync Turso provider settings when selected

use std::path::PathBuf;
use std::sync::Arc;

use edger_core::{DurableSqlProvider, ExecutionKind, ExtensionContext};
use edger_ext_auth::AuthExtension;
use edger_ext_gateway::GatewayExtension;
use edger_ext_keyval::SqlKeyValueProvider;
use edger_ext_turso::LocalSqliteProvider;
use edger_ext_turso_remote::RemoteTursoProvider;
use edger_isolation::{DenoFacade, DenoIsolate, WasiConfig, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, collect_extensions, load_manifests_from_dirs, parse_runtime_worker_dirs,
    port_from_env, run_on_init, run_on_server_start, run_on_shutdown, serve, AuthGate,
    AuthGateConfig, ExtensionRegistry, OrchestratorState, ServerConfig, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tracing_subscriber::EnvFilter;

struct RuntimeIsolateFactory;

impl IsolateFactory for RuntimeIsolateFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => Box::new(WasmIsolate::new(
                WasiConfig::from_worker_config(&worker_ref.config),
            )),
            _ => Box::new(DenoIsolate::new(DenoFacade::new())),
        }
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
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(RuntimeIsolateFactory));
    server.mark_ready(pool.clone());
    let worker_dirs = worker_dirs_from_env();
    let index = load_manifests_from_dirs(&worker_dirs)?;

    let auth_ext = AuthExtension::from_env()?.into_arc();
    let mut registry = collect_extensions(vec![GatewayExtension::middleware()])?;
    registry.register_auth_provider(auth_ext.clone())?;
    let sql_provider = durable_sql_provider_from_env()?;
    let keyval_provider = Arc::new(SqlKeyValueProvider::new(sql_provider.clone()));
    registry.register_durable_sql_provider(sql_provider)?;
    registry.register_key_value_provider(keyval_provider.clone())?;
    registry.register_queue_provider(keyval_provider)?;
    load_extension_status_store_from_env(&registry)?;
    run_on_init(&registry, &mut ExtensionContext::default())?;
    run_on_server_start(&registry, &edger_core::ServerHandle::default());

    let state = OrchestratorState {
        server: server.clone(),
        pool,
        index,
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

fn worker_dirs_from_env() -> Vec<PathBuf> {
    std::env::var("RUNTIME_WORKER_DIRS")
        .ok()
        .map(|raw| parse_runtime_worker_dirs(&raw))
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| vec![PathBuf::from("workers")])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DurableSqlProviderKind {
    Local,
    TursoRemote,
}

fn durable_sql_provider_from_env() -> anyhow::Result<Arc<dyn DurableSqlProvider>> {
    match durable_sql_provider_kind(std::env::var("EDGER_DURABLE_SQL_PROVIDER").ok().as_deref())? {
        DurableSqlProviderKind::Local => local_sql_provider_from_env(),
        DurableSqlProviderKind::TursoRemote => {
            Ok(Arc::new(RemoteTursoProvider::from_env()?) as Arc<dyn DurableSqlProvider>)
        }
    }
}

fn durable_sql_provider_kind(raw: Option<&str>) -> anyhow::Result<DurableSqlProviderKind> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("local") | Some("sqlite") | Some("local-sqlite") => {
            Ok(DurableSqlProviderKind::Local)
        }
        Some("turso") | Some("turso-remote") | Some("remote") | Some("sync")
        | Some("turso-sync") => Ok(DurableSqlProviderKind::TursoRemote),
        Some(value) => anyhow::bail!(
            "EDGER_DURABLE_SQL_PROVIDER must be local, turso-remote or turso-sync; got {value}"
        ),
    }
}

fn local_sql_provider_from_env() -> anyhow::Result<Arc<dyn DurableSqlProvider>> {
    let provider = match std::env::var("EDGER_STATE_DIR") {
        Ok(path) if !path.trim().is_empty() => LocalSqliteProvider::open_dir(path)?,
        _ => LocalSqliteProvider::in_memory(),
    };
    Ok(Arc::new(provider) as Arc<dyn DurableSqlProvider>)
}

fn load_extension_status_store_from_env(registry: &ExtensionRegistry) -> anyhow::Result<()> {
    if let Some(path) = extension_status_file_from_env() {
        registry.load_extension_status_store(path)?;
    }
    Ok(())
}

fn extension_status_file_from_env() -> Option<PathBuf> {
    std::env::var("EDGER_EXTENSION_STATUS_FILE")
        .ok()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("EDGER_STATE_DIR")
                .ok()
                .map(|path| path.trim().to_string())
                .filter(|path| !path.is_empty())
                .map(|path| PathBuf::from(path).join("extension-status.json"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn durable_sql_provider_kind_defaults_to_local() {
        assert_eq!(
            durable_sql_provider_kind(None).unwrap(),
            DurableSqlProviderKind::Local
        );
    }

    #[test]
    fn durable_sql_provider_kind_accepts_remote_aliases() {
        for value in ["turso", "turso-remote", "remote", "sync", "turso-sync"] {
            assert_eq!(
                durable_sql_provider_kind(Some(value)).unwrap(),
                DurableSqlProviderKind::TursoRemote
            );
        }
    }

    #[test]
    fn durable_sql_provider_kind_rejects_unknown_values() {
        let err = durable_sql_provider_kind(Some("postgres")).unwrap_err();

        assert!(err.to_string().contains("EDGER_DURABLE_SQL_PROVIDER"));
    }

    #[test]
    fn durable_sql_provider_from_env_selects_remote_without_connecting() {
        let _guard = env_lock().lock().unwrap();
        std::env::set_var("EDGER_DURABLE_SQL_PROVIDER", "turso-remote");
        std::env::set_var("EDGER_TURSO_NAMESPACE", "@acme");
        std::env::set_var("EDGER_TURSO_URL", "libsql://example.turso.io");
        std::env::set_var("EDGER_TURSO_AUTH_TOKEN", "secret-token");
        std::env::remove_var("EDGER_TURSO_LOCAL_PATH");

        let provider = durable_sql_provider_from_env().unwrap();

        assert_eq!(provider.name(), "turso-remote");
        std::env::remove_var("EDGER_DURABLE_SQL_PROVIDER");
        std::env::remove_var("EDGER_TURSO_NAMESPACE");
        std::env::remove_var("EDGER_TURSO_URL");
        std::env::remove_var("EDGER_TURSO_AUTH_TOKEN");
    }

    #[test]
    fn extension_status_file_from_env_prefers_explicit_file() {
        let _guard = env_lock().lock().unwrap();
        std::env::set_var("EDGER_EXTENSION_STATUS_FILE", "/tmp/edger/extensions.json");
        std::env::set_var("EDGER_STATE_DIR", "/tmp/edger/state");

        assert_eq!(
            extension_status_file_from_env().unwrap(),
            PathBuf::from("/tmp/edger/extensions.json")
        );

        std::env::remove_var("EDGER_EXTENSION_STATUS_FILE");
        std::env::remove_var("EDGER_STATE_DIR");
    }

    #[test]
    fn extension_status_file_from_env_defaults_to_state_dir() {
        let _guard = env_lock().lock().unwrap();
        std::env::remove_var("EDGER_EXTENSION_STATUS_FILE");
        std::env::set_var("EDGER_STATE_DIR", "/tmp/edger/state");

        assert_eq!(
            extension_status_file_from_env().unwrap(),
            PathBuf::from("/tmp/edger/state/extension-status.json")
        );

        std::env::remove_var("EDGER_STATE_DIR");
    }
}
