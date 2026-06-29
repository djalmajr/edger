//! edger-orchestrator — HTTP server, routing, auth, extensions (Epic 05).

pub mod manifest_index_stub;
pub mod router;
pub mod server;

pub use manifest_index_stub::{ManifestEntry, ManifestIndex};
pub use router::{resolve_route, PathParser, PluginRef, ReservedPath, ResolvedRoute};
pub use server::{port_from_env, router, serve, ServerConfig, ServerState};