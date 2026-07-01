//! Provider registry contract tests (Story 08.06).

use std::path::PathBuf;
use std::sync::Arc;

use edger_core::{create_worker_ref, root_principal, BindingKind, BindingManifest, WorkerManifest};
use edger_ext_gateway::GatewayExtension;
use edger_ext_keyval::SqlKeyValueProvider;
use edger_ext_turso::LocalSqliteProvider;
use edger_orchestrator::{collect_extensions, resolve_service_bindings, ExtensionRegistry};

fn state_providers() -> (
    Arc<LocalSqliteProvider>,
    Arc<SqlKeyValueProvider>,
    ExtensionRegistry,
) {
    let sql_provider = Arc::new(LocalSqliteProvider::in_memory());
    let keyval_provider = Arc::new(SqlKeyValueProvider::new(sql_provider.clone()));
    let mut registry = ExtensionRegistry::new();
    registry
        .register_durable_sql_provider(sql_provider.clone())
        .unwrap();
    registry
        .register_key_value_provider(keyval_provider.clone())
        .unwrap();
    registry
        .register_queue_provider(keyval_provider.clone())
        .unwrap();
    (sql_provider, keyval_provider, registry)
}

fn worker_with_binding(kind: BindingKind) -> edger_core::WorkerRef {
    create_worker_ref(
        PathBuf::from("/workers/stateful"),
        WorkerManifest {
            bindings: vec![BindingManifest {
                kind,
                name: "state".into(),
                namespace: Some("@team".into()),
                permissions: vec![],
            }],
            name: "@team/stateful".into(),
            version: Some("1.0.0".into()),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn registers_service_providers_and_exposes_admin_capabilities() {
    // Mutation captured: dropping service-provider registration leaves the admin
    // inventory without provider labels and breaks binding lookup.
    let (_, _, registry) = state_providers();

    assert!(registry.has_service_provider(&BindingKind::DurableSql));
    assert!(registry.has_service_provider(&BindingKind::KeyValue));
    assert!(registry.has_service_provider(&BindingKind::Queue));

    let extensions = registry.admin_extensions();
    let turso = extensions
        .iter()
        .find(|extension| extension.name == "turso")
        .expect("turso provider in admin inventory");
    assert_eq!(turso.kind, "serviceProvider");
    assert!(turso
        .capabilities
        .contains(&"provider:durableSql".to_string()));

    let keyval = extensions
        .iter()
        .find(|extension| extension.name == "keyval")
        .expect("keyval provider in admin inventory");
    assert!(keyval
        .capabilities
        .contains(&"provider:keyValue".to_string()));
    assert!(keyval.capabilities.contains(&"provider:queue".to_string()));
    assert_eq!(keyval.manifest.hooks, Vec::<String>::new());
    assert_eq!(keyval.manifest.menus, vec![]);
    assert_eq!(
        keyval.manifest.provides,
        vec![
            "provider:keyValue".to_string(),
            "provider:queue".to_string()
        ]
    );
    assert_eq!(
        keyval.manifest.requirements,
        vec!["provider:durableSql".to_string()]
    );
    assert_eq!(keyval.manifest.config.keys, Vec::<String>::new());
    assert!(keyval.manifest.config.redacted);
    assert_eq!(keyval.manifest.config.source, "staticRegistration");
}

#[test]
fn middleware_manifest_groups_gateway_hooks_and_safe_config() {
    // Mutation captured: flattening hooks into generic capabilities would leave
    // UI/MCP consumers unable to discover the gateway request/response hooks.
    let registry = collect_extensions(vec![GatewayExtension::middleware()]).unwrap();

    let gateway = registry
        .admin_extension("gateway")
        .expect("gateway middleware in admin inventory");

    assert_eq!(gateway.kind, "middleware");
    assert_eq!(
        gateway.capabilities,
        vec![
            "menu:Gateway".to_string(),
            "middleware".to_string(),
            "onRequest".to_string(),
            "onResponse".to_string()
        ]
    );
    assert_eq!(
        gateway.manifest.hooks,
        vec!["onRequest".to_string(), "onResponse".to_string()]
    );
    assert_eq!(gateway.manifest.menus[0].name, "Gateway");
    assert_eq!(gateway.manifest.provides, vec!["middleware".to_string()]);
    assert_eq!(gateway.manifest.requirements, Vec::<String>::new());
    assert_eq!(gateway.manifest.config.keys, Vec::<String>::new());
    assert!(gateway.manifest.config.redacted);
    assert_eq!(gateway.manifest.config.source, "staticRegistration");
}

#[test]
fn rejects_duplicate_provider_for_same_binding_kind() {
    // Mutation captured: allowing a second SQL provider makes binding resolution
    // ambiguous instead of failing at startup.
    let mut registry = ExtensionRegistry::new();
    registry
        .register_durable_sql_provider(Arc::new(LocalSqliteProvider::in_memory()))
        .unwrap();

    let err = registry
        .register_durable_sql_provider(Arc::new(LocalSqliteProvider::in_memory()))
        .unwrap_err();

    assert_eq!(err.code, "COLLISION");
    assert!(err.message.contains("provider:durableSql"));
}

#[test]
fn rejects_provider_when_dependency_is_missing() {
    // Mutation captured: skipping dependency validation lets keyval start
    // without a SQL backend, so its first request fails later and less clearly.
    let keyval_provider = Arc::new(SqlKeyValueProvider::new(Arc::new(
        LocalSqliteProvider::in_memory(),
    )));
    let mut registry = ExtensionRegistry::new();

    let err = registry
        .register_key_value_provider(keyval_provider)
        .unwrap_err();

    assert_eq!(err.code, "MISSING_DEPENDENCY");
    assert!(err.message.contains("provider:durableSql"));
}

#[test]
fn binding_lookup_rejects_missing_provider_before_dispatch() {
    // Mutation captured: removing the registry provider check injects a binding
    // descriptor even though no runtime service can satisfy it.
    let worker = worker_with_binding(BindingKind::Queue);
    let registry = ExtensionRegistry::new();

    let err = resolve_service_bindings(&worker, Some(&root_principal()), &registry).unwrap_err();

    assert_eq!(err.code, "MISSING_PROVIDER");
    assert!(err.message.contains("Queue"));
}

#[test]
fn binding_lookup_accepts_declared_provider_and_preserves_namespace() {
    // Mutation captured: ignoring the provider registry or namespace would return
    // a descriptor for the wrong service boundary.
    let (_, _, registry) = state_providers();
    let worker = worker_with_binding(BindingKind::KeyValue);

    let bindings = resolve_service_bindings(&worker, Some(&root_principal()), &registry)
        .unwrap()
        .unwrap();

    assert_eq!(bindings.worker, "@team/stateful");
    assert_eq!(bindings.bindings[0].kind, BindingKind::KeyValue);
    assert_eq!(bindings.bindings[0].namespace, "@team");
}

#[test]
fn disabled_service_provider_is_removed_from_binding_lookup() {
    // Mutation captured: treating enable/disable as inventory-only would leave
    // bindings usable after an operator removes the provider capability.
    let (_, _, registry) = state_providers();
    let worker = worker_with_binding(BindingKind::KeyValue);

    assert!(registry.has_service_provider(&BindingKind::KeyValue));

    let disabled = registry.set_extension_enabled("keyval", false).unwrap();
    assert_eq!(disabled.status, "disabled");
    assert!(!registry.has_service_provider(&BindingKind::KeyValue));
    assert!(!registry.has_service_provider(&BindingKind::Queue));

    let err = resolve_service_bindings(&worker, Some(&root_principal()), &registry).unwrap_err();
    assert_eq!(err.code, "MISSING_PROVIDER");

    let enabled = registry.set_extension_enabled("keyval", true).unwrap();
    assert_eq!(enabled.status, "enabled");
    assert!(registry.has_service_provider(&BindingKind::KeyValue));
}
