//! edger main binary — HTTP listener with health/readiness + pipeline (story 05.01–06.02).
//!
//! Environment:
//! - `PORT` — listen port (default `3000`)
//! - `RUNTIME_WORKER_DIRS` — `:` separated worker roots (default `workers`)
//! - `ROOT_API_KEY` — control-plane root key (optional)
//! - `EDGER_ROOT_KEY_FILE` — file-backed control-plane root key (takes precedence over `ROOT_API_KEY`)
//! - `EDGER_OIDC_ISSUER` — opt-in control-plane OIDC issuer; unset disables OIDC
//! - `EDGER_OIDC_AUDIENCE` — required audience when `EDGER_OIDC_ISSUER` is set
//! - `EDGER_OIDC_ROLES_CLAIM` — optional dotted role claim path, e.g. `realm_access.roles` or `groups`
//! - `EDGER_OIDC_REQUIRED_ROLE` — optional role required inside `EDGER_OIDC_ROLES_CLAIM`
//! - `EDGER_EXTENSION_STATUS_FILE` — JSON overlay for runtime extension enable/disable status
//! - `EDGER_CRON_ENABLED` — enable manifest `cron[]` jobs (default true)

use std::path::PathBuf;
use std::sync::Arc;

use edger_core::{ExecutionKind, ExtensionContext};
use edger_isolation::{DenoFacade, DenoIsolate, DenoProcessIsolate, WasiConfig, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, collect_cron_registrations, collect_extensions, init_tracing_from_env,
    load_manifests_from_dirs, parse_runtime_worker_dirs, port_from_env, run_on_init,
    run_on_server_start, run_on_shutdown, serve, ControlAuth, CronScheduler, CronSchedulerConfig,
    ExtensionRegistry, OrchestratorState, ServerConfig, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};

/// Selects the JS/TS backend. Default is the durable persistent-process runtime
/// (Epic 15); `EDGER_JS_RUNTIME=bridge` forces the legacy per-request CLI bridge.
struct RuntimeIsolateFactory {
    js_uses_process: bool,
}

impl RuntimeIsolateFactory {
    fn from_env() -> Self {
        let js_uses_process = std::env::var("EDGER_JS_RUNTIME")
            .map(|value| !value.trim().eq_ignore_ascii_case("bridge"))
            .unwrap_or(true);
        Self { js_uses_process }
    }
}

impl IsolateFactory for RuntimeIsolateFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => Box::new(WasmIsolate::new(
                WasiConfig::from_worker_config(&worker_ref.config),
            )),
            _ if self.js_uses_process => Box::new(DenoProcessIsolate::new()),
            _ => Box::new(DenoIsolate::new(DenoFacade::new())),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _tracing = init_tracing_from_env()?;

    let port = port_from_env();
    let config = ServerConfig::from_port(port);
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(
        PoolConfig::default(),
        Arc::new(RuntimeIsolateFactory::from_env()),
    );
    server.mark_ready(pool.clone());
    let worker_dirs = worker_dirs_from_env();
    let index = load_manifests_from_dirs(&worker_dirs)?;

    let registry = collect_extensions(vec![])?;
    load_extension_status_store_from_env(&registry)?;
    run_on_init(&registry, &mut ExtensionContext::default())?;
    run_on_server_start(&registry, &edger_core::ServerHandle::default());
    let auth = ControlAuth::from_env();
    if auth.is_open() {
        tracing::warn!(
            "control-plane auth is open because neither ROOT_API_KEY nor EDGER_ROOT_KEY_FILE is configured"
        );
    }

    let state = OrchestratorState {
        server: server.clone(),
        pool,
        index,
        registry,
        auth,
    };
    let app = build_pipeline(state.clone());
    let cron_registrations = if env_flag_default_true("EDGER_CRON_ENABLED") {
        collect_cron_registrations(&state.index)?
    } else {
        Vec::new()
    };
    let cron_scheduler = CronScheduler::start(
        CronSchedulerConfig::new(state.auth.root_key_for_internal_clients()),
        cron_registrations,
        app.clone(),
        state.server.cron_metrics(),
    )?;

    let shutdown_registry = state.registry.clone();
    let shutdown_server = server.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("shutdown signal received");
            cron_scheduler.shutdown().await;
            let _ = run_on_shutdown(&shutdown_registry);
            shutdown_server.shutdown_pool();
        }
    });

    serve(config, app).await
}

fn worker_dirs_from_env() -> Vec<PathBuf> {
    std::env::var("RUNTIME_WORKER_DIRS")
        .ok()
        .map(|raw| parse_runtime_worker_dirs(&raw))
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| vec![PathBuf::from("workers")])
}

fn env_flag_default_true(name: &str) -> bool {
    non_empty_env(name)
        .map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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
