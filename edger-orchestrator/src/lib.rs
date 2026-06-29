//! edger-orchestrator — HTTP server, routing, auth, extensions (Epic 05).

pub mod auth;
pub mod context;
pub mod hooks;
pub mod manifest_index_stub;
pub mod pipeline;
pub mod registry;
pub mod router;
pub mod server;
pub mod store;
pub mod wire;

pub use auth::{extract_api_key, is_public_route, AuthGate, AuthGateConfig};
pub use context::RequestContext;
pub use hooks::{run_on_init, run_on_request, run_on_response, run_on_server_start, run_on_shutdown};
pub use manifest_index_stub::{ManifestEntry, ManifestIndex};
pub use pipeline::{build_pipeline, OrchestratorState};
pub use registry::ExtensionRegistry;
pub use store::{ApiKeyStore, SqliteApiKeyStore};
pub use router::{resolve_route, PathParser, PluginRef, ReservedPath, ResolvedRoute};
pub use server::{port_from_env, router, serve, ServerConfig, ServerState};
pub use wire::{axum_to_serialized, serialized_to_axum, MAX_BODY_BYTES};