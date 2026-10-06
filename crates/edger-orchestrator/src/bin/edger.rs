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
//! - `EDGER_API_KEYS_DB` — SQLite das api-keys persistentes (default `.edger/api-keys.db`; só inicializa com auth configurada; o console por senha usa o MESMO arquivo)
//! - `EDGER_ROOT_PASSWORD_FILE` — arquivo com a senha inicial do root da console (semeia o root SOMENTE quando o usuário `root` não existe — operadores pré-existentes não bloqueiam a semente; arquivo configurado e inválido/vazio/fora da política de força falha o boot; nunca usa o root token como senha nem cria senha default)
//! - `EDGER_OIDC_ISSUER` — opt-in control-plane OIDC issuer; unset disables OIDC
//! - `EDGER_OIDC_AUDIENCE` — required audience when `EDGER_OIDC_ISSUER` is set
//! - `EDGER_OIDC_NAMESPACES_CLAIM` — optional dotted namespace claim path (default `namespaces`)
//! - `EDGER_OIDC_ROLES_CLAIM` — optional dotted role claim path, e.g. `realm_access.roles` or `groups`
//! - `EDGER_OIDC_ADMIN_ROLE` — optional role that marks an OIDC principal as root
//! - `EDGER_OIDC_REQUIRED_ROLE` — optional role required inside `EDGER_OIDC_ROLES_CLAIM`
//! - `EDGER_CRON_ENABLED` — enable manifest `cron[]` jobs (default true)
//! - `EDGER_TENANT_ROUTING_ENABLED` — opt in to tenant allowlists (default false)
//! - `EDGER_WEIGHTED_ROUTING_ENABLED` — opt in to weighted version selection (default false)
//! - `EDGER_TENANCIT_IDENTIFY_URL` — exact Tenancit Consumer API `/v1/identify` URL
//! - `EDGER_TENANCIT_TOKEN_FILE` — file containing the `tenant:identify` API client token
//! - `EDGER_COMPRESSION` — `on` (default) | `off`: data-plane compression
//!   layer (brotli + gzip); `off` mounts no layer (no 406, no Vary from it)
//! - `EDGER_COMPRESSION_MIN_BYTES` — minimum compressible body size (default 1024)
//! - `EDGER_COMPRESSION_LEVEL` — `default` (default) | `fastest` | `best` | integer
//!   (`best` is brotli quality 11 — expensive for dynamic/streaming bodies)
//!
//! Invalid values log a warning and fall back to the default.

use anyhow::Context;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use edger_core::ExecutionKind;
use edger_isolation::{
    ConsoleLogContext, ConsoleLogSender, ConsoleStream, DenoFacade, DenoIsolate,
    DenoProcessIsolate, StreamDetachBudget, WasiConfig, WasmIsolate,
};
use edger_orchestrator::compression::{CompressionConfig, MIN_COMPRESSIBLE_BYTES};
use edger_orchestrator::observability::{
    OperationalEventInput, OperationalEventLevel, OperationalEventSource, OperationalStore,
};
use edger_orchestrator::tenant_identity::TenantIdentityClient;
use edger_orchestrator::{
    bind_ip_from_env, build_pipeline, collect_cron_registrations, init_tracing_from_env,
    load_manifests_from_roots, parse_runtime_worker_dirs, port_from_env,
    prewarm_min_process_workers, run_pending_releases_with_events, serve, ControlAuth,
    CronScheduler, CronSchedulerConfig, OrchestratorState, ServerConfig, ServerState,
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
    stream_detach_max_bytes: u64,
    stream_detach_budget: Arc<StreamDetachBudget>,
    /// EDG-9 abandon-drain limits (per isolate process, mirrored by the
    /// pool's completion wait): `0` in either one disables the drain.
    abandon_drain_max_bytes: u64,
    abandon_drain_max_ms: u64,
}

/// `EDGER_STREAM_DETACH_MAX_BYTES` default: 8 MiB per-response buffered tail.
const DEFAULT_STREAM_DETACH_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// `EDGER_STREAM_DETACH_TOTAL_BYTES` default: 64 MiB process-wide budget.
const DEFAULT_STREAM_DETACH_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Parse a detach-bytes env var: invalid values (including empty or negative
/// text) fall back to the default with a warning; `0` is a valid disable.
fn stream_detach_env(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(parsed) => parsed,
            Err(_) => {
                tracing::warn!(var = name, default, "invalid {name}; using the default");
                default
            }
        },
        Err(_) => default,
    }
}

impl RuntimeIsolateFactory {
    fn from_env(console_sender: Option<ConsoleLogSender>) -> Self {
        let js_uses_process = std::env::var("EDGER_JS_RUNTIME")
            .map(|value| !value.trim().eq_ignore_ascii_case("bridge"))
            .unwrap_or(true);
        let stream_detach_max_bytes = stream_detach_env(
            "EDGER_STREAM_DETACH_MAX_BYTES",
            DEFAULT_STREAM_DETACH_MAX_BYTES,
        );
        let stream_detach_total_bytes = stream_detach_env(
            "EDGER_STREAM_DETACH_TOTAL_BYTES",
            DEFAULT_STREAM_DETACH_TOTAL_BYTES,
        );
        // EDG-9: the reader keeps discarding an abandoned response to a
        // clean end frame, bounded by both limits; `0` disables the drain
        // (the pre-EDG-9 socket_poisoned recycle).
        let abandon_drain_max_bytes = stream_detach_env(
            "EDGER_STREAM_ABANDON_DRAIN_MAX_BYTES",
            edger_core::STREAM_ABANDON_DRAIN_MAX_BYTES_DEFAULT,
        );
        let abandon_drain_max_ms = stream_detach_env(
            "EDGER_STREAM_ABANDON_DRAIN_MAX_MS",
            edger_core::STREAM_ABANDON_DRAIN_MAX_MS_DEFAULT,
        );
        Self {
            console_sender,
            js_uses_process,
            stream_detach_max_bytes,
            stream_detach_budget: Arc::new(StreamDetachBudget::new(stream_detach_total_bytes)),
            abandon_drain_max_bytes,
            abandon_drain_max_ms,
        }
    }
}

impl IsolateFactory for RuntimeIsolateFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => Box::new(WasmIsolate::new(
                WasiConfig::from_worker_config(&worker_ref.config),
            )),
            _ if self.js_uses_process => {
                let isolate = match self.console_sender.as_ref() {
                    Some(sender) => DenoProcessIsolate::with_console(
                        sender.clone(),
                        ConsoleLogContext {
                            namespace: worker_ref.namespace.clone(),
                            worker: worker_ref.name.clone(),
                            version: worker_ref.version.clone(),
                        },
                    ),
                    None => DenoProcessIsolate::new(),
                };
                Box::new(
                    isolate
                        .with_stream_detach(
                            self.stream_detach_max_bytes,
                            Arc::clone(&self.stream_detach_budget),
                        )
                        // EDG-9: on consumer loss, drain the abandoned response
                        // (discard-mode to a clean end frame) instead of
                        // poisoning the socket and recycling the process.
                        .with_abandon_drain_limits(
                            self.abandon_drain_max_bytes,
                            self.abandon_drain_max_ms,
                        ),
                )
            }
            _ => Box::new(DenoIsolate::new(DenoFacade::new())),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _tracing = init_tracing_from_env()?;
    let auth = ControlAuth::from_env()?;

    let port = port_from_env();
    let config = ServerConfig::from_bind(bind_ip_from_env().map_err(anyhow::Error::msg)?, port);
    let server = ServerState::new_unready();
    // EDG-6: data-plane compression settings are read once at boot and
    // mounted by `build_pipeline` (the first `set_compression_config` wins).
    server.set_compression_config(compression_config_from_env());
    if opt_in_flag("EDGER_TENANT_ROUTING_ENABLED")? {
        configure_tenant_identity(&server)?;
        server.enable_tenant_routing();
    }
    if opt_in_flag("EDGER_WEIGHTED_ROUTING_ENABLED")? {
        server.enable_weighted_routing();
    }
    let console_sender = start_console_capture(&server);
    let lifecycle_sender = start_lifecycle_capture(&server);
    // EDG-9: the pool's completion wait must mirror the isolates'
    // `EDGER_STREAM_ABANDON_DRAIN_MAX_MS` (the drain's own deadline is
    // measured from the consumer loss, which happens before the body's drop
    // observes it; the pool adds its small grace on top). `0` in either
    // limit disables the drain AND the pool's relay wait (immediate
    // recycle, `socket_poisoned` sub-cause).
    let isolate_factory = RuntimeIsolateFactory::from_env(console_sender);
    let abandon_drain = edger_worker::AbandonDrainLimits {
        max_bytes: isolate_factory.abandon_drain_max_bytes,
        max_ms: isolate_factory.abandon_drain_max_ms,
    };
    let pool = WorkerPool::with_factory_and_lifecycle_abandon_drain(
        PoolConfig::default(),
        Arc::new(isolate_factory),
        Some(lifecycle_sender),
        abandon_drain,
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

    // Console por senha (root user + sessões `ses-` persistentes): mesmo
    // arquivo de banco das api-keys. A semente vem de EDGER_ROOT_PASSWORD_FILE
    // (somente quando o usuário `root` não existe — operadores pré-existentes
    // não bloqueiam a semente); arquivo configurado e inválido/vazio/fora da
    // política de força falha o boot — nunca senha default nem root
    // token como senha.
    let console_seed = match non_empty_env("EDGER_ROOT_PASSWORD_FILE").map(PathBuf::from) {
        Some(path) => match edger_orchestrator::console_auth::load_seed_password(&path) {
            Ok(seed) => seed,
            Err(err) => anyhow::bail!("EDGER_ROOT_PASSWORD_FILE: {err}"),
        },
        None => None,
    };
    let db_path = std::env::var("EDGER_API_KEYS_DB")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".edger/api-keys.db"));
    if let Some(parent) = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(path = %parent.display(), error = %err, "api keys dir not creatable");
        }
    }

    // Keys persistentes (egk_) e console por senha: só fazem sentido com
    // credencial configurada (root key/OIDC ou senha semeada) — em open mode
    // fresco tudo já é root e os stores nem inicializam. Falha de open vira
    // warn, não crash: uma instância sem o PVC continua servindo com root.
    let auth = if auth.is_open_without_console() {
        // Open mode (sem root key/OIDC): a senha é credencial — o wiring do
        // console decide seed/adoção/falha (nunca degrada para open).
        wire_console_open_mode(auth, &db_path, console_seed.as_deref())?
    } else {
        let auth = match edger_orchestrator::api_keys::ApiKeyService::open(&db_path) {
            Ok(service) => {
                tracing::info!(path = %db_path.display(), "api key store ready");
                auth.with_key_service(std::sync::Arc::new(service))
            }
            Err(err) => {
                tracing::warn!(path = %db_path.display(), code = %err.code, "api key store unavailable: {}", err.message);
                auth
            }
        };
        match edger_orchestrator::ConsoleAuthService::open(&db_path) {
            Ok(service) => {
                match service.seed_root_if_empty(console_seed.as_deref()) {
                    Ok(true) => {
                        tracing::info!(path = %db_path.display(), "console root user seeded from EDGER_ROOT_PASSWORD_FILE");
                    }
                    Ok(false) => {
                        if console_seed.is_some() {
                            tracing::warn!(
                                "root user already present; EDGER_ROOT_PASSWORD_FILE ignored"
                            );
                        }
                    }
                    Err(err) => anyhow::bail!("cannot seed console root user: {err}"),
                }
                auth.with_console_service(std::sync::Arc::new(service))
            }
            Err(err) => {
                if console_seed.is_some() {
                    // Senha explicitamente configurada: não cai para open
                    // mode silenciosamente — falha fechada no boot.
                    anyhow::bail!("cannot open console store at {}: {err}", db_path.display());
                }
                tracing::warn!(path = %db_path.display(), code = %err.code, "console store unavailable: {}", err.message);
                auth
            }
        }
    };

    if auth.is_open() {
        tracing::warn!(
            "control-plane auth is open because neither ROOT_API_KEY, EDGER_ROOT_KEY_FILE, OIDC, nor a seeded console root is configured"
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

fn configure_tenant_identity(server: &ServerState) -> anyhow::Result<()> {
    let endpoint = std::env::var("EDGER_TENANCIT_IDENTIFY_URL")
        .context("EDGER_TENANCIT_IDENTIFY_URL is required when tenant routing is enabled")?;
    let token_file = std::env::var("EDGER_TENANCIT_TOKEN_FILE")
        .context("EDGER_TENANCIT_TOKEN_FILE is required when tenant routing is enabled")?;
    let endpoint = reqwest::Url::parse(&endpoint).context("invalid Tenancit identify URL")?;
    let token = std::fs::read_to_string(&token_file).context("cannot read Tenancit token file")?;
    let token = token.trim_end_matches(['\r', '\n']);
    let client = TenantIdentityClient::new(endpoint, token).map_err(anyhow::Error::msg)?;
    server.set_tenant_identity_client(client);
    Ok(())
}

fn opt_in_flag(name: &str) -> anyhow::Result<bool> {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(_) => anyhow::bail!("{name} must be true or false"),
        Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => anyhow::bail!("{name} must be true or false"),
        },
    }
}

/// Read the `EDGER_COMPRESSION*` envs into a `CompressionConfig` (EDG-6).
/// Unset variables keep the defaults; an invalid value logs a `warn` and
/// falls back to the default of that variable (boot never fails on them).
fn compression_config_from_env() -> CompressionConfig {
    let mut config = CompressionConfig::default();
    if let Ok(raw) = std::env::var("EDGER_COMPRESSION") {
        match CompressionConfig::parse_enabled(&raw) {
            Some(enabled) => config.enabled = enabled,
            None => tracing::warn!(
                var = "EDGER_COMPRESSION",
                value = %raw,
                "invalid value (expected on or off); using the default (on)"
            ),
        }
    }
    if let Ok(raw) = std::env::var("EDGER_COMPRESSION_MIN_BYTES") {
        match raw.trim().parse::<u64>() {
            Ok(parsed) => config.min_bytes = parsed,
            Err(_) => tracing::warn!(
                var = "EDGER_COMPRESSION_MIN_BYTES",
                value = %raw,
                "invalid value (expected a non-negative integer); using the default ({MIN_COMPRESSIBLE_BYTES})"
            ),
        }
    }
    if let Ok(raw) = std::env::var("EDGER_COMPRESSION_LEVEL") {
        match CompressionConfig::parse_level(&raw) {
            Some(level) => config.level = level,
            None => tracing::warn!(
                var = "EDGER_COMPRESSION_LEVEL",
                value = %raw,
                "invalid value (expected default, fastest, best or an integer); using the default"
            ),
        }
    }
    config
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
    // (EDG-9) Preserve the reason and the drain sub-cause on the
    // operational surface (the old fields above stay untouched): the REUSE
    // reason (`stream_abandoned_drained`) rides the `DrainCompleted`
    // event's code, and the RECYCLE sub-cause (`bytes_limit`,
    // `time_limit`, `stream_error`, `socket_poisoned`) rides the
    // `Terminated` event's code — its outcome is already the real reason
    // (`stream_abandoned_recycled`).
    let code = match record.kind {
        WorkerLifecycleEventKind::DrainCompleted => Some(record.reason.to_string()),
        WorkerLifecycleEventKind::Terminated => record.detail.map(|detail| detail.to_string()),
        _ => None,
    };
    // (EDG-9 slice 2, amended) A `DrainCompleted` also carries the drain
    // sub-cause (`cancelled` for the cancel end): the reason keeps its
    // code slot, so the sub-cause rides the message — the SAME plain
    // detail string `Terminated` puts in its code — and the message no
    // longer depends solely on `drained_count` (which is `None` for
    // stream-abandon drains).
    let message = if matches!(record.kind, WorkerLifecycleEventKind::DrainCompleted) {
        record.detail.map(|detail| detail.to_string()).or_else(|| {
            record
                .drained_count
                .map(|count| format!("drained waitUntil promises: {count}"))
        })
    } else {
        record
            .drained_count
            .map(|count| format!("drained waitUntil promises: {count}"))
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
        code,
        message,
        truncated: None,
        dropped_count: None,
        method: None,
        path: None,
        content_type: None,
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
        method: None,
        path: None,
        content_type: None,
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

/// Wiring do console em open mode (sem root key/OIDC) — P1: nenhuma degradação
/// silenciosa para API admin aberta.
///
/// - Semente válida (`EDGER_ROOT_PASSWORD_FILE`): a senha é a única
///   credencial — cria/abre o store e semeia o root; o gate fecha. Store ou
///   seed que falhe neste modo falha o boot (NUNCA segue aberto).
/// - Sem semente e DB existente: adota o store se ele ABRIR e RESPONDER a
///   consulta; erro de open/consulta do banco pré-existente falha o boot.
///   Com root no banco o gate fecha; sem root continua open (sem credencial
///   de senha nenhuma).
/// - Sem arquivo e sem semente: open mode legado preservado (nenhuma
///   credencial configurada).
fn wire_console_open_mode(
    auth: ControlAuth,
    db_path: &Path,
    seed: Option<&str>,
) -> anyhow::Result<ControlAuth> {
    if let Some(seed) = seed {
        let service = edger_orchestrator::ConsoleAuthService::open(db_path).map_err(|err| {
            anyhow::anyhow!("cannot open console store at {}: {err}", db_path.display())
        })?;
        match service.seed_root_if_empty(Some(seed)) {
            Ok(true) => {
                tracing::info!(
                    path = %db_path.display(),
                    "console root user seeded from EDGER_ROOT_PASSWORD_FILE; control-plane auth is not open"
                );
            }
            Ok(false) => {
                tracing::warn!("root user already present; EDGER_ROOT_PASSWORD_FILE ignored");
            }
            Err(err) => {
                anyhow::bail!("cannot seed console root user: {err}")
            }
        }
        return Ok(auth.with_console_service(Arc::new(service)));
    }
    if !db_path.exists() {
        // Open mode legado: sem arquivo, sem semente, sem root key/OIDC.
        return Ok(auth);
    }
    let service = edger_orchestrator::ConsoleAuthService::open(db_path).map_err(|err| {
        anyhow::anyhow!(
            "cannot open existing console store at {}: {err}",
            db_path.display()
        )
    })?;
    match service.has_root_user() {
        Ok(true) => {
            tracing::warn!(
                path = %db_path.display(),
                "console root user present; control-plane auth is not open"
            );
            Ok(auth.with_console_service(Arc::new(service)))
        }
        Ok(false) => {
            // Store sem root: não há credencial de senha — segue open.
            Ok(auth)
        }
        Err(err) => {
            tracing::warn!(code = %err.code, "console root user check failed: {}", err.message);
            anyhow::bail!(
                "cannot verify console root user at {}: {err}",
                db_path.display()
            )
        }
    }
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

    #[test]
    fn tenant_and_weighted_routing_are_explicit_opt_ins() {
        let _guard = env_lock().lock().unwrap();
        for name in [
            "EDGER_TENANT_ROUTING_ENABLED",
            "EDGER_WEIGHTED_ROUTING_ENABLED",
        ] {
            std::env::remove_var(name);
            assert!(!opt_in_flag(name).unwrap());
            std::env::set_var(name, "true");
            assert!(opt_in_flag(name).unwrap());
            std::env::set_var(name, "0");
            assert!(!opt_in_flag(name).unwrap());
            std::env::set_var(name, "maybe");
            assert!(opt_in_flag(name).is_err());
            std::env::remove_var(name);
        }
    }

    // --- EDG-6: compression env reading --------------------------------------

    fn clear_compression_envs() {
        std::env::remove_var("EDGER_COMPRESSION");
        std::env::remove_var("EDGER_COMPRESSION_MIN_BYTES");
        std::env::remove_var("EDGER_COMPRESSION_LEVEL");
    }

    #[test]
    fn compression_env_unset_keeps_defaults() {
        let _guard = env_lock().lock().unwrap();
        clear_compression_envs();
        assert_eq!(compression_config_from_env(), CompressionConfig::default());
    }

    #[test]
    fn compression_env_valid_values_are_applied() {
        let _guard = env_lock().lock().unwrap();
        clear_compression_envs();
        std::env::set_var("EDGER_COMPRESSION", "off");
        std::env::set_var("EDGER_COMPRESSION_MIN_BYTES", "4096");
        std::env::set_var("EDGER_COMPRESSION_LEVEL", "best");
        assert_eq!(
            compression_config_from_env(),
            CompressionConfig {
                enabled: false,
                min_bytes: 4096,
                level: tower_http::compression::CompressionLevel::Best,
            }
        );
        // Precise level via integer.
        std::env::set_var("EDGER_COMPRESSION", "ON");
        std::env::set_var("EDGER_COMPRESSION_LEVEL", "11");
        assert_eq!(
            compression_config_from_env(),
            CompressionConfig {
                enabled: true,
                min_bytes: 4096,
                level: tower_http::compression::CompressionLevel::Precise(11),
            }
        );
        clear_compression_envs();
    }

    #[test]
    fn compression_env_invalid_values_fall_back_to_defaults() {
        let _guard = env_lock().lock().unwrap();
        clear_compression_envs();
        std::env::set_var("EDGER_COMPRESSION", "maybe");
        std::env::set_var("EDGER_COMPRESSION_MIN_BYTES", "-3");
        std::env::set_var("EDGER_COMPRESSION_LEVEL", "maximum");
        assert_eq!(compression_config_from_env(), CompressionConfig::default());
        clear_compression_envs();
    }

    #[test]
    fn compression_env_invalid_values_mix_with_valid_ones() {
        let _guard = env_lock().lock().unwrap();
        clear_compression_envs();
        std::env::set_var("EDGER_COMPRESSION", "off");
        std::env::set_var("EDGER_COMPRESSION_MIN_BYTES", "not-a-number");
        std::env::set_var("EDGER_COMPRESSION_LEVEL", "fastest");
        assert_eq!(
            compression_config_from_env(),
            CompressionConfig {
                enabled: false,
                min_bytes: MIN_COMPRESSIBLE_BYTES,
                level: tower_http::compression::CompressionLevel::Fastest,
            }
        );
        clear_compression_envs();
    }

    // --- P1: open mode + senha como credencial (wire_console_open_mode) ---

    const STRONG_PW: &str = "Str0ng!Passw0rd";
    const STRONG_PW_2: &str = "An0ther!Passw0rd";

    fn open_mode_auth() -> ControlAuth {
        ControlAuth::new(edger_orchestrator::ControlAuthConfig::default())
    }

    #[test]
    fn password_only_fresh_db_seeds_root_and_closes_gate() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("api-keys.db");
        let auth = wire_console_open_mode(open_mode_auth(), &db, Some(STRONG_PW)).unwrap();
        // Senha única: o gate FECHA e o login do root funciona.
        assert!(!auth.is_open(), "senha única precisa fechar o gate");
        let service = auth.console_service().unwrap().clone();
        assert!(service
            .login(std::net::IpAddr::from([10u8, 0, 0, 1]), "root", STRONG_PW)
            .is_ok());
    }

    #[tokio::test]
    async fn password_only_admin_without_credential_is_unauthenticated() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("api-keys.db");
        let auth = wire_console_open_mode(open_mode_auth(), &db, Some(STRONG_PW)).unwrap();
        // Sem credencial nenhuma o request não autentica — o pipeline mapeia
        // `None` para 401 nos endpoints admin (não é open mode).
        assert!(auth
            .authenticate_headers(&axum::http::HeaderMap::new())
            .await
            .is_none());
    }

    #[test]
    fn password_only_existing_root_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("api-keys.db");
        // Boot anterior já semeou root com outra senha.
        let boot = edger_orchestrator::ConsoleAuthService::open(&db).unwrap();
        boot.seed_root_if_empty(Some(STRONG_PW_2)).unwrap();
        let auth = wire_console_open_mode(open_mode_auth(), &db, Some(STRONG_PW)).unwrap();
        assert!(!auth.is_open());
        let service = auth.console_service().unwrap().clone();
        let ip = std::net::IpAddr::from([10u8, 0, 0, 2]);
        assert!(service.login(ip, "root", STRONG_PW_2).is_ok());
        // A semente nova NÃO substitui a senha existente.
        assert!(service.login(ip, "root", STRONG_PW).is_err());
    }

    #[test]
    fn corrupted_existing_db_fails_boot_without_other_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("api-keys.db");
        // Arquivo pré-existente que NÃO é um SQLite válido (volume corrompido).
        std::fs::write(&db, b"this is not a sqlite database at all").unwrap();
        // Sem semente e sem root key/OIDC: NUNCA pode degradar para open.
        let result = wire_console_open_mode(open_mode_auth(), &db, None);
        assert!(
            result.is_err(),
            "DB corrompido precisa falhar o boot, não abrir admin"
        );
        // Erro sem expor conteúdo do banco.
        let message = match result {
            Err(err) => err.to_string(),
            Ok(_) => panic!("esperava falha de boot"),
        };
        assert!(!message.contains("not a sqlite"));
    }

    #[test]
    fn no_file_and_no_seed_preserves_legacy_open_mode() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("api-keys.db");
        let auth = wire_console_open_mode(open_mode_auth(), &db, None).unwrap();
        assert!(
            auth.is_open(),
            "sem nenhuma credencial segue open mode legado"
        );
        assert!(auth.console_service().is_none());
    }

    // (EDG-9) The lifecycle consumer must PRESERVE the reason and the drain
    // sub-cause on the operational surface: `stream_abandoned_drained` on
    // reuse (the DrainCompleted event) and `stream_abandoned_recycled`
    // WITH the sub-cause on recycle (the Terminated event) — the serialized
    // operational event is what an operator sees.
    #[test]
    fn lifecycle_reason_and_detail_reach_the_serialized_operational_event() {
        use edger_orchestrator::observability::OperationalEventQuery;

        let worker_ref = edger_core::create_worker_ref(
            std::path::PathBuf::from("/workers/lifecycle-cause"),
            edger_core::WorkerManifest {
                name: "lifecycle-cause".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let store = OperationalStore::default();

        // Reuse: the pool's completion ran after the EDG-9 drain finished —
        // the cancel end carries its sub-cause (slice 2).
        record_lifecycle_event(
            &store,
            WorkerLifecycleEvent {
                kind: WorkerLifecycleEventKind::DrainCompleted,
                worker_ref: worker_ref.clone(),
                process_id: None,
                drained_count: None,
                duration_ms: Some(12),
                reason: "stream_abandoned_drained",
                detail: Some("cancelled"),
            },
        );
        // Recycle: the drain stopped at the byte limit.
        record_lifecycle_event(
            &store,
            WorkerLifecycleEvent {
                kind: WorkerLifecycleEventKind::Terminated,
                worker_ref: worker_ref.clone(),
                process_id: Some("proc-1".into()),
                drained_count: None,
                duration_ms: Some(34),
                reason: "stream_abandoned_recycled",
                detail: Some("bytes_limit"),
            },
        );

        let page = store.query(OperationalEventQuery::default());
        let json = serde_json::to_string(&page.events).unwrap();

        // The old fields stay intact.
        assert!(json.contains("\"kind\":\"process.drain.completed\""));
        assert!(json.contains("\"outcome\":\"completed\""));
        assert!(json.contains("\"kind\":\"process.terminated\""));
        assert!(json.contains("\"outcome\":\"stream_abandoned_recycled\""));
        // (EDG-9) reason and sub-cause now ride the operational event.
        assert!(
            json.contains("stream_abandoned_drained"),
            "the reuse reason must reach the serialized event: {json}"
        );
        assert!(
            json.contains("\"code\":\"bytes_limit\""),
            "the recycle sub-cause must reach the serialized event: {json}"
        );
        assert!(
            json.contains("\"message\":\"cancelled\""),
            "the cancel sub-cause must reach the serialized event: {json}"
        );

        // And the fields are on the RIGHT events (not just somewhere).
        let drained = page
            .events
            .iter()
            .find(|event| event.kind == "process.drain.completed")
            .expect("drain completed event");
        assert_eq!(drained.code.as_deref(), Some("stream_abandoned_drained"));
        // (EDG-9 slice 2, amended) The reason AND the sub-cause ride the
        // SAME event: the reason keeps the code slot, the sub-cause rides
        // the message (the same plain detail string `Terminated` puts in
        // its code) — even with `drained_count: None`.
        assert_eq!(
            drained.message.as_deref(),
            Some("cancelled"),
            "the cancel sub-cause must be preserved on the drain-completed event"
        );
        let terminated = page
            .events
            .iter()
            .find(|event| event.kind == "process.terminated")
            .expect("terminated event");
        assert_eq!(
            terminated.outcome.as_deref(),
            Some("stream_abandoned_recycled")
        );
        assert_eq!(terminated.code.as_deref(), Some("bytes_limit"));
    }
}
