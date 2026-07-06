//! edger-orchestrator — HTTP server, routing, auth, and worker dispatch.

pub mod admin_api;
pub mod auth;
pub mod cron;
pub mod deploy;
pub mod manifest_index_stub;
pub mod manifest_loader;
pub mod metrics;
pub mod oidc;
pub mod operational_log;
pub mod pipeline;
pub mod router;
pub mod security;
pub mod server;
pub mod tracing_init;
pub mod wire;
pub mod worker_errors;

pub use admin_api::router as admin_router;
pub use auth::{extract_api_key, ControlAuth, ControlAuthConfig};
pub use cron::{collect_cron_registrations, CronMetrics, CronScheduler, CronSchedulerConfig};
pub use deploy::{
    install_worker_from_zip, prewarm_min_process_workers, rescan_workers,
    rescan_workers_and_prewarm, InstalledWorker, RescanReport,
};
pub use manifest_index_stub::{ManifestEntry, ManifestIndex};
pub use manifest_loader::{load_manifests_from_dirs, parse_runtime_worker_dirs};
pub use metrics::pool_metrics_prometheus;
pub use oidc::{JwksSource, OidcConfig, OidcDiscovery, OidcError, OidcValidator};
pub use pipeline::{build_pipeline, OrchestratorState};
pub use router::{
    resolve_host_route, resolve_route, PathParser, PluginRef, ReservedPath, ResolvedRoute,
};
pub use security::validate_admin_mutation_security;
pub use server::{port_from_env, router, serve, ServerConfig, ServerState};
pub use tracing_init::{init_tracing_from_env, TracingInitConfig};

pub use wire::{
    axum_to_serialized, axum_to_serialized_with_limit, serialized_to_axum, MAX_BODY_BYTES,
};
