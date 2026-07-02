//! edger-core: pure vocabulary. No I/O.
//!
//! Leaf crate — manifests, configs, wire formats, traits, errors.
//! Higher crates (`edger-worker`, `edger-isolation`, `edger-orchestrator`) depend on this.

pub mod admin;
pub mod api_key_store;
pub mod auth;
pub mod bindings;
pub mod config;
pub mod context;
pub mod error;
pub mod execution;
pub mod extension;
pub mod isolate;
pub mod manifest;
pub mod principal;
pub mod security;
pub mod wire;
pub mod worker_ref;

pub use admin::{
    AdminApiKeyInfo, AdminApiKeysResponse, AdminCatalogItem, AdminCatalogResponse,
    AdminCreateApiKeyRequest, AdminCreateApiKeyResponse, AdminErrorResponse, AdminExtensionInfo,
    AdminExtensionManifest, AdminExtensionManifestConfig, AdminExtensionManifestMenu,
    AdminExtensionReconcileAction, AdminExtensionReconcileActionKind,
    AdminExtensionReconcileClassification, AdminExtensionReconcileDiagnostics,
    AdminExtensionReconcileRequest, AdminExtensionReconcileResponse,
    AdminExtensionReconcileSummary, AdminExtensionsResponse, AdminMutationResponse,
    AdminRevokeApiKeyResponse, AdminSessionResponse, AdminWorkerInfo, AdminWorkersResponse,
};
pub use api_key_store::ApiKeyStore;
pub use auth::{extract_api_key_from_pairs, AuthProvider, HeaderPairs};
pub use bindings::{
    binding_descriptor, default_binding_namespace, BindingDescriptor, BindingKind, BindingManifest,
    BindingSet, DurableSqlProvider, KeyValueEntry, KeyValueProvider, QueueMessage, QueueProvider,
    SqlRow, StateKey, StateValue,
};
pub use config::{
    infer_execution_kind, parse_duration_string_to_ms, parse_duration_to_ms, parse_size_to_bytes,
    parse_worker_config, WorkerConfig,
};
pub use context::{ExtensionContext, RequestContext, ServerHandle};
pub use error::{CoreError, IsolationError};
pub use execution::ExecutionKind;
pub use extension::{
    Extension, ExtensionCapability, ExtensionDependency, ExtensionHook, Middleware, WorkerHandler,
};
pub use isolate::Isolate;
pub use manifest::{CronJob, PublicRoutesConfig, WorkerManifest};
pub use principal::{principal_can_access_namespace, root_principal, ApiKeyPrincipal};
pub use security::{
    is_mutating_method, is_sensitive_env_key, principal_can_access_optional_namespace,
    principal_has_permission, require_same_origin, INTERNAL_REQUEST_HEADER,
};
pub use wire::{
    validate_headers, SerializedRequest, SerializedResponse, MAX_HEADERS, MAX_HEADER_BYTES,
    MAX_HEADER_VALUE_BYTES,
};
pub use worker_ref::{create_worker_ref, parse_namespaced_name, WorkerRef};

/// Crate identity marker for module layout tests.
pub const CRATE_PURE_VOCABULARY: &str = "edger-core";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modules_declared_and_reexported() {
        assert_eq!(CRATE_PURE_VOCABULARY, "edger-core");
        let _ = std::any::type_name::<AdminWorkerInfo>();
        let _ = std::any::type_name::<BindingDescriptor>();
        let _ = std::any::type_name::<WorkerManifest>();
        let _ = std::any::type_name::<SerializedRequest>();
        let _ = std::any::type_name::<ApiKeyPrincipal>();
        assert_eq!(INTERNAL_REQUEST_HEADER, "x-edger-internal");
    }

    #[test]
    fn execution_kind_roundtrips() {
        let kind = ExecutionKind::FetchHandler;
        let json = serde_json::to_string(&kind).unwrap();
        let back: ExecutionKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }
}
