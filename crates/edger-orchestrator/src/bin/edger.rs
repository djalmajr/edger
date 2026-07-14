//! edger main binary — HTTP listener with health/readiness + pipeline (story 05.01–06.02).
//!
//! Environment:
//! - `PORT` — listen port (default `3000`)
//! - `RUNTIME_WORKER_DIRS` — `:` separated user-worker roots (default `workers/examples`)
//! - `EDGER_CORE_WORKER_DIR` — immutable bundled core workers (default `workers/core`)
//! - `EDGER_CORE_WORKER_OVERLAY_DIR` — administrator-installed core overlays
//!   (default `.edger/core-worker-overlays`)
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
use edger_isolation::{
    ConsoleLogContext, ConsoleLogSender, ConsoleStream, DenoFacade, DenoIsolate,
    DenoProcessIsolate, WasiConfig, WasmIsolate,
};
use edger_orchestrator::observability::{
    OperationalEventInput, OperationalEventLevel, OperationalEventSource, OperationalStore,
};
use edger_orchestrator::{
    build_pipeline, collect_cron_registrations, init_tracing_from_env, load_manifests_from_roots,
    parse_runtime_worker_dirs, port_from_env, prewarm_min_process_workers,
    run_pending_releases_with_events, serve, ControlAuth, CronScheduler, CronSchedulerConfig,
    OrchestratorState, ServerConfig, ServerState,
};
use edger_worker::{
    IsolateFactory, LifecycleEventSender, PoolConfig, WorkerLifecycleEvent,
    WorkerLifecycleEventKind, WorkerPool,
};

/// Selects the JS/TS backend. Default is the durable persistent-process runtime
/// (Epic 15); `EDGER_JS_RUNTIME=bridge` forces the legacy per-request CLI bridge.
struct RuntimeIsolateFactory {
    console_sender: Option<ConsoleLogSender>,
    js_uses_process: bool,
}

impl RuntimeIsolateFactory {
    fn from_env(console_sender: Option<ConsoleLogSender>) -> Self {
        let js_uses_process = std::env::var("EDGER_JS_RUNTIME")
            .map(|value| !value.trim().eq_ignore_ascii_case("bridge"))
            .unwrap_or(true);
        Self {
            console_sender,
            js_uses_process,
        }
    }
}

impl IsolateFactory for RuntimeIsolateFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => Box::new(WasmIsolate::new(
                WasiConfig::from_worker_config(&worker_ref.config),
            )),
            _ if self.js_uses_process => match self.console_sender.as_ref() {
                Some(sender) => Box::new(DenoProcessIsolate::with_console(
                    sender.clone(),
                    ConsoleLogContext {
                        namespace: worker_ref.namespace.clone(),
                        worker: worker_ref.name.clone(),
                        version: worker_ref.version.clone(),
                    },
                )),
                None => Box::new(DenoProcessIsolate::new()),
            },
            _ => Box::new(DenoIsolate::new(DenoFacade::new())),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _tracing = init_tracing_from_env()?;
    let auth = ControlAuth::from_env()?;

    let port = port_from_env();
    let config = ServerConfig::from_port(port);
    let server = ServerState::new_unready();
    let console_sender = start_console_capture(&server);
    let lifecycle_sender = start_lifecycle_capture(&server);
    let pool = WorkerPool::with_factory_and_lifecycle(
        PoolConfig::default(),
        Arc::new(RuntimeIsolateFactory::from_env(console_sender)),
        Some(lifecycle_sender),
    );
    server.mark_ready(pool.clone());
    let worker_dirs = worker_dirs_from_env();
    let core_worker_dir = core_worker_dir_from_env();
    let core_overlay_dir = core_overlay_dir_from_env();
    let index = load_manifests_from_roots(
        std::slice::from_ref(&core_worker_dir),
        Some(&core_overlay_dir),
        &worker_dirs,
    )?;
    // Run each worker's release command (migrations) once per version before serving.
    run_pending_releases_with_events(&index, &server.operational_events()).await?;
    prewarm_min_process_workers(&index, &pool).await?;

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
    // Await the graceful worker drain (beforeunload + waitUntil) before exiting so
    // platform shutdown/scale-down runs the same cleanup as a TTL/idle recycle.
    if let Some(drain) = server.shutdown_pool() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(15), drain).await;
    }
    serve_result
}

fn start_console_capture(server: &ServerState) -> Option<ConsoleLogSender> {
    if !env_flag_default_true("EDGER_CONSOLE_LOGS_ENABLED") {
        return None;
    }
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1_024);
    let events = server.operational_events();
    tokio::spawn(async move {
        while let Some(record) = receiver.recv().await {
            record_console_event(&events, record);
        }
    });
    Some(sender)
}

fn start_lifecycle_capture(server: &ServerState) -> LifecycleEventSender {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(256);
    let events = server.operational_events();
    tokio::spawn(async move {
        while let Some(record) = receiver.recv().await {
            record_lifecycle_event(&events, record);
        }
    });
    sender
}

fn record_lifecycle_event(events: &OperationalStore, record: WorkerLifecycleEvent) {
    let (kind, level, outcome) = match record.kind {
        WorkerLifecycleEventKind::DrainStarted => (
            "process.drain.started",
            OperationalEventLevel::Info,
            "started",
        ),
        WorkerLifecycleEventKind::DrainCompleted => (
            "process.drain.completed",
            OperationalEventLevel::Info,
            "completed",
        ),
        WorkerLifecycleEventKind::DrainTimedOut => (
            "process.drain.timed_out",
            OperationalEventLevel::Warn,
            "timed_out",
        ),
        WorkerLifecycleEventKind::Terminated => (
            "process.terminated",
            OperationalEventLevel::Info,
            record.reason,
        ),
    };
    events.record(OperationalEventInput {
        source: OperationalEventSource::Drain,
        kind: kind.into(),
        level,
        namespace: record.worker_ref.namespace,
        worker: Some(record.worker_ref.name),
        version: Some(record.worker_ref.version),
        process_id: record.process_id,
        request_id: None,
        trace_id: None,
        outcome: Some(outcome.into()),
        status: None,
        duration_ms: record.duration_ms,
        code: None,
        message: record
            .drained_count
            .map(|count| format!("drained waitUntil promises: {count}")),
        truncated: None,
        dropped_count: None,
    });
}

fn record_console_event(events: &OperationalStore, record: edger_isolation::ConsoleLogRecord) {
    events.record(OperationalEventInput {
        source: OperationalEventSource::Console,
        kind: "console".into(),
        level: match record.stream {
            ConsoleStream::Stdout => OperationalEventLevel::Info,
            ConsoleStream::Stderr => OperationalEventLevel::Error,
        },
        namespace: record.context.namespace,
        worker: Some(record.context.worker),
        version: Some(record.context.version),
        process_id: Some(record.process_id),
        request_id: None,
        trace_id: None,
        outcome: None,
        status: None,
        duration_ms: None,
        code: None,
        message: Some(record.message),
        truncated: record.truncated.then_some(true),
        dropped_count: (record.dropped_before > 0).then_some(record.dropped_before),
    });
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
        .unwrap_or_else(|| vec![PathBuf::from("workers/examples")])
}

fn core_worker_dir_from_env() -> PathBuf {
    non_empty_env("EDGER_CORE_WORKER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("workers/core"))
}

fn core_overlay_dir_from_env() -> PathBuf {
    non_empty_env("EDGER_CORE_WORKER_OVERLAY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".edger/core-worker-overlays"))
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
