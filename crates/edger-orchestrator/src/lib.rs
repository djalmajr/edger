//! edger-orchestrator — HTTP server, routing, auth, and worker dispatch.
// Rust 1.99 clippy `double_must_use` rejects the `#[must_use]` that
// `async_trait` attributes to the generated `JwksSource` methods (a boxed
// `Future` is already considered `#[must_use]`). Lint suppression only —
// no code generation changes.
#![allow(clippy::double_must_use)]

pub mod admin_api;
pub mod api_keys;
pub mod auth;
pub mod compression;
mod conditional;
pub mod console_auth;
pub mod cron;
pub mod deploy;
pub mod manifest_index_stub;
pub mod manifest_loader;
pub mod mcp_http;
pub mod metrics;
pub mod observability;
pub mod oidc;
pub mod operational_log;
pub mod pipeline;
pub mod rate_limit;
pub mod router;
pub mod routing_policy;
pub mod security;
pub mod server;
pub mod state_export;
pub mod tenant_identity;
pub mod tracing_init;
pub mod wire;
pub mod worker_errors;

pub use admin_api::router as admin_router;
pub use api_keys::{ApiKeyService, API_KEY_PREFIX};
pub use auth::{extract_api_key, ControlAuth, ControlAuthConfig};
pub use console_auth::{load_seed_password, ConsoleAuthService, LoginLimiter, SESSION_PREFIX};
pub use cron::{collect_cron_registrations, CronMetrics, CronScheduler, CronSchedulerConfig};
pub use deploy::{
    install_worker_from_zip, prewarm_min_process_workers, rescan_workers,
    rescan_workers_and_prewarm, rescan_workers_and_prewarm_with_events, run_pending_releases,
    run_pending_releases_with_events, InstalledWorker, RescanReport, MAX_DEPLOY_PACKAGE_BYTES,
};
pub use manifest_index_stub::{ManifestEntry, ManifestIndex};
pub use manifest_loader::{
    clear_persisted_routing_policy, load_manifests_from_dirs, load_manifests_from_roots,
    parse_runtime_worker_dirs, persist_routing_policy,
};
pub use metrics::pool_metrics_prometheus;
pub use oidc::{JwksSource, OidcConfig, OidcDiscovery, OidcError, OidcValidator};
pub use pipeline::{build_pipeline, OrchestratorState};
pub use router::{
    resolve_host_route, resolve_route, PathParser, PluginRef, ReservedPath, ResolvedRoute,
};
pub use routing_policy::{parse_routing_policy, RoutingPolicy, RoutingTraffic, TenantAccess};
pub use security::validate_admin_mutation_security;
pub use server::{
    bind_ip_from_env, parse_bind_ip, parse_bind_os, port_from_env, router, serve, ServerConfig,
    ServerState,
};
pub use tracing_init::{init_tracing_from_env, TracingInitConfig};

pub use wire::{
    axum_to_serialized, axum_to_serialized_with_limit, serialized_to_axum, MAX_BODY_BYTES,
};
