//! Service binding resolution for worker dispatch.

use edger_core::{
    binding_descriptor, default_binding_namespace, principal_can_access_optional_namespace,
    ApiKeyPrincipal, BindingSet, CoreError, WorkerRef,
};

use crate::registry::ExtensionRegistry;

pub const SERVICE_BINDINGS_HEADER: &str = "x-edger-bindings";

pub fn resolve_service_bindings(
    worker: &WorkerRef,
    principal: Option<&ApiKeyPrincipal>,
    registry: &ExtensionRegistry,
) -> Result<Option<BindingSet>, CoreError> {
    if worker.config.bindings.is_empty() {
        return Ok(None);
    }
    let principal = principal.ok_or_else(|| {
        CoreError::new(
            "FORBIDDEN",
            "service bindings require an authenticated principal",
        )
    })?;
    let default_namespace = default_binding_namespace(&worker.name, worker.namespace.as_deref());
    let mut bindings = Vec::with_capacity(worker.config.bindings.len());
    for binding in &worker.config.bindings {
        let descriptor = binding_descriptor(binding, &default_namespace)?;
        if !principal_can_access_optional_namespace(principal, Some(&descriptor.namespace)) {
            return Err(CoreError::new(
                "FORBIDDEN",
                format!("namespace {} is not allowed", descriptor.namespace),
            ));
        }
        if !registry.has_service_provider(&descriptor.kind) {
            return Err(CoreError::new(
                "MISSING_PROVIDER",
                format!("missing provider for binding kind {:?}", descriptor.kind),
            ));
        }
        bindings.push(descriptor);
    }
    Ok(Some(BindingSet {
        bindings,
        worker: worker.name.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use edger_core::{
        create_worker_ref, root_principal, ApiKeyPrincipal, BindingKind, BindingManifest,
        DurableSqlProvider, Extension, ExtensionCapability, ExtensionContext, KeyValueEntry,
        KeyValueProvider, QueueMessage, QueueProvider, SqlRow, StateValue, WorkerManifest,
    };

    use super::*;

    struct TestProviders;

    impl Extension for TestProviders {
        fn capabilities(&self) -> Vec<ExtensionCapability> {
            vec![
                ExtensionCapability::service_provider(BindingKind::DurableSql),
                ExtensionCapability::service_provider(BindingKind::KeyValue),
                ExtensionCapability::service_provider(BindingKind::Queue),
            ]
        }

        fn name(&self) -> &'static str {
            "test-providers"
        }

        fn on_init(&self, _ctx: &mut ExtensionContext) -> anyhow::Result<()> {
            Ok(())
        }
    }

    impl DurableSqlProvider for TestProviders {
        fn execute(
            &self,
            _namespace: &str,
            _sql: &str,
            _params: &[StateValue],
        ) -> Result<u64, CoreError> {
            Ok(0)
        }

        fn execute_batch(&self, _namespace: &str, _sql: &str) -> Result<(), CoreError> {
            Ok(())
        }

        fn query(
            &self,
            _namespace: &str,
            _sql: &str,
            _params: &[StateValue],
        ) -> Result<Vec<SqlRow>, CoreError> {
            Ok(vec![])
        }
    }

    impl KeyValueProvider for TestProviders {
        fn delete(&self, _namespace: &str, _key: &[String]) -> Result<bool, CoreError> {
            Ok(false)
        }

        fn get(
            &self,
            _namespace: &str,
            _key: &[String],
        ) -> Result<Option<KeyValueEntry>, CoreError> {
            Ok(None)
        }

        fn set(
            &self,
            _namespace: &str,
            key: &[String],
            value: StateValue,
            _expires_at: Option<u64>,
        ) -> Result<KeyValueEntry, CoreError> {
            Ok(KeyValueEntry {
                key: key.to_vec(),
                value,
                versionstamp: "1".into(),
            })
        }
    }

    impl QueueProvider for TestProviders {
        fn ack(&self, _namespace: &str, _id: &str) -> Result<bool, CoreError> {
            Ok(false)
        }

        fn dequeue(&self, _namespace: &str) -> Result<Option<QueueMessage>, CoreError> {
            Ok(None)
        }

        fn enqueue(&self, _namespace: &str, value: StateValue) -> Result<QueueMessage, CoreError> {
            Ok(QueueMessage {
                attempts: 0,
                id: "test".into(),
                value,
            })
        }
    }

    fn registry_with_providers() -> ExtensionRegistry {
        let provider = std::sync::Arc::new(TestProviders);
        let mut registry = ExtensionRegistry::new();
        registry
            .register_durable_sql_provider(provider.clone())
            .unwrap();
        registry
            .register_key_value_provider(provider.clone())
            .unwrap();
        registry.register_queue_provider(provider).unwrap();
        registry
    }

    fn principal(namespaces: Vec<&str>) -> ApiKeyPrincipal {
        ApiKeyPrincipal {
            id: 10,
            name: "operator".into(),
            key_prefix: "operator".into(),
            role: "operator".into(),
            permissions: vec!["workers:read".into()],
            namespaces: namespaces.into_iter().map(str::to_string).collect(),
            is_root: false,
            expires_at: None,
        }
    }

    fn worker(bindings: Vec<BindingManifest>) -> WorkerRef {
        create_worker_ref(
            PathBuf::from("/workers/team-checkout"),
            WorkerManifest {
                name: "@team/checkout".into(),
                version: Some("1.0.0".into()),
                bindings,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn returns_none_when_worker_has_no_bindings() {
        let registry = ExtensionRegistry::new();
        assert!(resolve_service_bindings(&worker(vec![]), None, &registry)
            .unwrap()
            .is_none());
    }

    #[test]
    fn resolves_descriptors_with_default_namespace() {
        let worker = worker(vec![BindingManifest {
            kind: BindingKind::KeyValue,
            name: "cache".into(),
            namespace: None,
            permissions: vec!["kv:read".into()],
        }]);
        let registry = registry_with_providers();
        let bindings = resolve_service_bindings(&worker, Some(&root_principal()), &registry)
            .unwrap()
            .unwrap();

        assert_eq!(bindings.worker, "@team/checkout");
        assert_eq!(bindings.bindings[0].name, "cache");
        assert_eq!(bindings.bindings[0].namespace, "@team");
        assert_eq!(bindings.bindings[0].permissions, vec!["kv:read"]);
    }

    #[test]
    fn rejects_public_dispatch_for_bound_worker() {
        let worker = worker(vec![BindingManifest {
            kind: BindingKind::Queue,
            name: "jobs".into(),
            namespace: None,
            permissions: vec![],
        }]);
        let registry = registry_with_providers();
        let err = resolve_service_bindings(&worker, None, &registry).unwrap_err();

        assert_eq!(err.code, "FORBIDDEN");
    }

    #[test]
    fn rejects_principal_outside_binding_namespace() {
        let worker = worker(vec![BindingManifest {
            kind: BindingKind::DurableSql,
            name: "db".into(),
            namespace: Some("@team".into()),
            permissions: vec![],
        }]);
        let registry = registry_with_providers();
        let err = resolve_service_bindings(&worker, Some(&principal(vec!["@other"])), &registry)
            .unwrap_err();

        assert_eq!(err.code, "FORBIDDEN");
    }

    #[test]
    fn rejects_binding_when_provider_is_missing() {
        let worker = worker(vec![BindingManifest {
            kind: BindingKind::Queue,
            name: "jobs".into(),
            namespace: None,
            permissions: vec![],
        }]);
        let registry = ExtensionRegistry::new();
        let err =
            resolve_service_bindings(&worker, Some(&root_principal()), &registry).unwrap_err();

        assert_eq!(err.code, "MISSING_PROVIDER");
    }
}
