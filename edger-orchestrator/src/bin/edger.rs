//! edger main binary — HTTP listener with health/readiness + pipeline (story 05.01–06.02).
//!
//! Environment:
//! - `PORT` — listen port (default `3000`)
//! - `RUNTIME_WORKER_DIRS` — `:` separated worker roots (default `workers`)
//! - `ROOT_API_KEY` — control-plane root key (optional)
//! - `EDGER_ROOT_KEY_FILE` — file-backed control-plane root key (takes precedence over `ROOT_API_KEY`)
//! - `EDGER_OIDC_ISSUER` — opt-in control-plane OIDC issuer; unset disables OIDC
//! - `EDGER_OIDC_AUDIENCE` — required audience when `EDGER_OIDC_ISSUER` is set
//! - `EDGER_OIDC_NAMESPACES_CLAIM` — optional dotted namespace claim path (default `namespaces`)
//! - `EDGER_OIDC_ROLES_CLAIM` — optional dotted role claim path, e.g. `realm_access.roles` or `groups`
//! - `EDGER_OIDC_ADMIN_ROLE` — optional role that marks an OIDC principal as root
//! - `EDGER_OIDC_REQUIRED_ROLE` — optional role required inside `EDGER_OIDC_ROLES_CLAIM`
//! - `EDGER_CRON_ENABLED` — enable manifest `cron[]` jobs (default true)

use std::path::PathBuf;
use std::sync::Arc;

use edger_core::ExecutionKind;
use edger_isolation::{DenoFacade, DenoIsolate, DenoProcessIsolate, WasiConfig, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, collect_cron_registrations, init_tracing_from_env, load_manifests_from_dirs,
    parse_runtime_worker_dirs, port_from_env, serve, ControlAuth, CronScheduler,
    CronSchedulerConfig, OrchestratorState, ServerConfig, ServerState,
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

    let serve_result = serve(config, app, shutdown_signal()).await;
    tracing::info!("HTTP server stopped; shutting down cron scheduler and worker pool");
    cron_scheduler.shutdown().await;
    server.shutdown_pool();
    serve_result
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(signal) => signal,
        Err(error) => {
            tracing::warn!(%error, "failed to install SIGTERM handler; waiting for SIGINT only");
            wait_for_sigint().await;
            return;
        }
    };

    tokio::select! {
        _ = wait_for_sigint() => {}
        _ = terminate.recv() => {
            tracing::info!("SIGTERM received; starting graceful shutdown");
        }
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    wait_for_sigint().await;
}

async fn wait_for_sigint() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => tracing::info!("SIGINT received; starting graceful shutdown"),
        Err(error) => {
            tracing::warn!(%error, "failed to wait for SIGINT; shutdown signal disabled");
            std::future::pending::<()>().await;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn env_flag_default_true_handles_common_false_values() {
        let _guard = env_lock().lock().unwrap();

        for value in ["0", "false", "no", "off"] {
            std::env::set_var("EDGER_CRON_ENABLED", value);
            assert!(!env_flag_default_true("EDGER_CRON_ENABLED"));
        }

        std::env::remove_var("EDGER_CRON_ENABLED");
        assert!(env_flag_default_true("EDGER_CRON_ENABLED"));
    }
}
