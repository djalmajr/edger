//! edger-orchestrator — HTTP server, routing, auth, extensions (Epic 05).

pub mod admin_api;
pub mod auth;
pub mod context;
pub mod cron;
pub mod deploy;
pub mod hooks;
pub mod manifest_index_stub;
pub mod manifest_loader;
pub mod metrics;
pub mod operational_log;
pub mod pipeline;
pub mod registry;
pub mod router;
pub mod security;
pub mod server;
pub mod service_bindings;
pub mod shell_gateway;
pub mod tracing_init;
pub mod wire;
pub mod worker_errors;

pub use admin_api::router as admin_router;
pub use auth::{extract_api_key, is_public_route, AuthGate, AuthGateConfig};
pub use context::RequestContext;
pub use cron::{collect_cron_registrations, CronMetrics, CronScheduler, CronSchedulerConfig};
pub use deploy::{install_worker_from_zip, rescan_workers, InstalledWorker, RescanReport};
pub use hooks::{
    run_on_init, run_on_request, run_on_response, run_on_server_start, run_on_shutdown,
    run_on_worker_complete, run_on_worker_dispatch, run_on_worker_error,
};
pub use manifest_index_stub::{ManifestEntry, ManifestIndex};
pub use manifest_loader::{load_manifests_from_dirs, parse_runtime_worker_dirs};
pub use metrics::pool_metrics_prometheus;
pub use pipeline::{build_pipeline, OrchestratorState};
pub use registry::{collect_extensions, ExtensionRegistry};
pub use router::{
    resolve_host_route, resolve_route, PathParser, PluginRef, ReservedPath, ResolvedRoute,
};
pub use security::validate_admin_mutation_security;
pub use server::{port_from_env, router, serve, ServerConfig, ServerState};
pub use service_bindings::{resolve_service_bindings, SERVICE_BINDINGS_HEADER};
pub use shell_gateway::resolve_shell_worker;
pub use tracing_init::{init_tracing_from_env, TracingInitConfig};

pub use wire::{axum_to_serialized, serialized_to_axum, MAX_BODY_BYTES};
