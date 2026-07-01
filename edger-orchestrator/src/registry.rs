//! Extension registry — ordered middleware storage (story 05.05).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use edger_core::{
    AdminExtensionInfo, AuthProvider, BindingKind, CoreError, DurableSqlProvider,
    ExtensionCapability, ExtensionDependency, KeyValueProvider, Middleware, QueueProvider,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Registry of middleware extensions sorted by `priority()` (lower runs first).
#[derive(Clone, Default)]
pub struct ExtensionRegistry {
    middlewares: Arc<Vec<Arc<dyn Middleware>>>,
    auth_provider: Arc<Option<Arc<dyn AuthProvider>>>,
    durable_sql_provider: Arc<Option<Arc<dyn DurableSqlProvider>>>,
    extension_status: Arc<RwLock<BTreeMap<String, bool>>>,
    extension_status_store: Arc<RwLock<Option<PathBuf>>>,
    key_value_provider: Arc<Option<Arc<dyn KeyValueProvider>>>,
    queue_provider: Arc<Option<Arc<dyn QueueProvider>>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionStatusDocument {
    extensions: BTreeMap<String, bool>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, middleware: Arc<dyn Middleware>) -> Result<(), CoreError> {
        self.ensure_dependencies(middleware.name(), middleware.dependencies())?;
        let name = middleware.name();
        let entries = Arc::make_mut(&mut self.middlewares);
        if entries
            .iter()
            .any(|existing| existing.name() == middleware.name())
        {
            return Err(CoreError::new(
                "COLLISION",
                format!("extension already registered: {}", middleware.name()),
            ));
        }
        entries.push(middleware);
        entries.sort_by_key(|m| m.priority());
        self.ensure_status(name);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.middlewares.len()
    }

    pub fn is_empty(&self) -> bool {
        self.middlewares.is_empty()
    }

    pub fn middlewares(&self) -> &[Arc<dyn Middleware>] {
        &self.middlewares
    }

    pub fn active_middlewares(&self) -> Vec<Arc<dyn Middleware>> {
        self.middlewares()
            .iter()
            .filter(|middleware| self.is_extension_enabled(middleware.name()))
            .cloned()
            .collect()
    }

    pub fn register_auth_provider(
        &mut self,
        provider: Arc<dyn AuthProvider>,
    ) -> Result<(), CoreError> {
        let expected = ExtensionCapability::auth_provider();
        self.ensure_capability_declared(provider.name(), &expected, provider.capabilities())?;
        self.ensure_dependencies(provider.name(), provider.dependencies())?;
        let name = provider.name();
        {
            let slot = Arc::make_mut(&mut self.auth_provider);
            if slot.is_some() {
                return Err(CoreError::new(
                    "COLLISION",
                    "auth provider already registered".to_string(),
                ));
            }
            *slot = Some(provider);
        }
        self.ensure_status(name);
        Ok(())
    }

    pub fn auth_provider(&self) -> Option<Arc<dyn AuthProvider>> {
        self.registered_auth_provider()
            .filter(|provider| self.is_extension_enabled(provider.name()))
    }

    pub fn durable_sql_provider(&self) -> Option<Arc<dyn DurableSqlProvider>> {
        self.registered_durable_sql_provider()
            .filter(|provider| self.is_extension_enabled(provider.name()))
    }

    pub fn key_value_provider(&self) -> Option<Arc<dyn KeyValueProvider>> {
        self.registered_key_value_provider()
            .filter(|provider| self.is_extension_enabled(provider.name()))
    }

    pub fn queue_provider(&self) -> Option<Arc<dyn QueueProvider>> {
        self.registered_queue_provider()
            .filter(|provider| self.is_extension_enabled(provider.name()))
    }

    pub fn has_service_provider(&self, kind: &BindingKind) -> bool {
        match kind {
            BindingKind::DurableSql => self.durable_sql_provider().is_some(),
            BindingKind::KeyValue => self.key_value_provider().is_some(),
            BindingKind::Queue => self.queue_provider().is_some(),
        }
    }

    pub fn register_durable_sql_provider(
        &mut self,
        provider: Arc<dyn DurableSqlProvider>,
    ) -> Result<(), CoreError> {
        let expected = ExtensionCapability::service_provider(BindingKind::DurableSql);
        self.register_service_provider(
            provider.name(),
            provider.priority(),
            provider.capabilities(),
            provider.dependencies(),
            &expected,
        )?;
        let name = provider.name();
        {
            let slot = Arc::make_mut(&mut self.durable_sql_provider);
            if slot.is_some() {
                return Err(duplicate_provider_error(&expected));
            }
            *slot = Some(provider);
        }
        self.ensure_status(name);
        Ok(())
    }

    pub fn register_key_value_provider(
        &mut self,
        provider: Arc<dyn KeyValueProvider>,
    ) -> Result<(), CoreError> {
        let expected = ExtensionCapability::service_provider(BindingKind::KeyValue);
        self.register_service_provider(
            provider.name(),
            provider.priority(),
            provider.capabilities(),
            provider.dependencies(),
            &expected,
        )?;
        let name = provider.name();
        {
            let slot = Arc::make_mut(&mut self.key_value_provider);
            if slot.is_some() {
                return Err(duplicate_provider_error(&expected));
            }
            *slot = Some(provider);
        }
        self.ensure_status(name);
        Ok(())
    }

    pub fn register_queue_provider(
        &mut self,
        provider: Arc<dyn QueueProvider>,
    ) -> Result<(), CoreError> {
        let expected = ExtensionCapability::service_provider(BindingKind::Queue);
        self.register_service_provider(
            provider.name(),
            provider.priority(),
            provider.capabilities(),
            provider.dependencies(),
            &expected,
        )?;
        let name = provider.name();
        {
            let slot = Arc::make_mut(&mut self.queue_provider);
            if slot.is_some() {
                return Err(duplicate_provider_error(&expected));
            }
            *slot = Some(provider);
        }
        self.ensure_status(name);
        Ok(())
    }

    pub fn is_extension_enabled(&self, name: &str) -> bool {
        self.extension_status
            .read()
            .expect("extension status lock")
            .get(name)
            .copied()
            .unwrap_or(true)
    }

    pub fn set_extension_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<AdminExtensionInfo, CoreError> {
        let name = self.find_extension_name(name)?;
        let statuses = {
            let mut next = self
                .extension_status
                .read()
                .expect("extension status lock")
                .clone();
            next.insert(name.clone(), enabled);
            next
        };
        self.persist_extension_statuses(&statuses)?;
        *self
            .extension_status
            .write()
            .expect("extension status lock") = statuses;
        self.admin_extension(&name).ok_or_else(|| {
            CoreError::new(
                "NOT_FOUND",
                format!("extension not found after status update: {name}"),
            )
        })
    }

    pub fn load_extension_status_store(&self, path: impl AsRef<Path>) -> Result<(), CoreError> {
        let path = path.as_ref().to_path_buf();
        let document = read_extension_status_document(&path)?;
        *self
            .extension_status_store
            .write()
            .expect("extension status store lock") = Some(path);
        if let Some(document) = document {
            self.apply_extension_statuses(document.extensions);
        }
        Ok(())
    }

    pub fn admin_extension(&self, name: &str) -> Option<AdminExtensionInfo> {
        self.admin_extensions()
            .into_iter()
            .find(|extension| extension.name == name)
    }

    pub fn admin_extensions(&self) -> Vec<AdminExtensionInfo> {
        let mut by_name: BTreeMap<String, AdminExtensionInfo> = BTreeMap::new();

        for middleware in self.middlewares() {
            upsert_admin_extension(
                &mut by_name,
                AdminExtensionRegistration {
                    capabilities: middleware.capabilities(),
                    dependencies: middleware.dependencies(),
                    diagnostics: middleware.diagnostics(),
                    kind: "middleware",
                    name: middleware.name(),
                    priority: middleware.priority(),
                    status: status_label(self.is_extension_enabled(middleware.name())),
                },
            );
        }
        if let Some(provider) = self.registered_auth_provider() {
            upsert_admin_extension(
                &mut by_name,
                AdminExtensionRegistration {
                    capabilities: provider.capabilities(),
                    dependencies: provider.dependencies(),
                    diagnostics: provider.diagnostics(),
                    kind: "authProvider",
                    name: provider.name(),
                    priority: provider.priority(),
                    status: status_label(self.is_extension_enabled(provider.name())),
                },
            );
        }
        if let Some(provider) = self.registered_durable_sql_provider() {
            upsert_admin_extension(
                &mut by_name,
                AdminExtensionRegistration {
                    capabilities: provider.capabilities(),
                    dependencies: provider.dependencies(),
                    diagnostics: provider.diagnostics(),
                    kind: "serviceProvider",
                    name: provider.name(),
                    priority: provider.priority(),
                    status: status_label(self.is_extension_enabled(provider.name())),
                },
            );
        }
        if let Some(provider) = self.registered_key_value_provider() {
            upsert_admin_extension(
                &mut by_name,
                AdminExtensionRegistration {
                    capabilities: provider.capabilities(),
                    dependencies: provider.dependencies(),
                    diagnostics: provider.diagnostics(),
                    kind: "serviceProvider",
                    name: provider.name(),
                    priority: provider.priority(),
                    status: status_label(self.is_extension_enabled(provider.name())),
                },
            );
        }
        if let Some(provider) = self.registered_queue_provider() {
            upsert_admin_extension(
                &mut by_name,
                AdminExtensionRegistration {
                    capabilities: provider.capabilities(),
                    dependencies: provider.dependencies(),
                    diagnostics: provider.diagnostics(),
                    kind: "serviceProvider",
                    name: provider.name(),
                    priority: provider.priority(),
                    status: status_label(self.is_extension_enabled(provider.name())),
                },
            );
        }

        let mut extensions = by_name.into_values().collect::<Vec<_>>();

        extensions.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.name.cmp(&b.name))
        });
        extensions
    }

    fn ensure_status(&self, name: &str) {
        self.extension_status
            .write()
            .expect("extension status lock")
            .entry(name.to_string())
            .or_insert(true);
    }

    fn apply_extension_statuses(&self, statuses: BTreeMap<String, bool>) {
        let known = self.extension_names().into_iter().collect::<BTreeSet<_>>();
        let mut current = self
            .extension_status
            .write()
            .expect("extension status lock");
        for (name, enabled) in statuses {
            if known.contains(&name) {
                current.insert(name, enabled);
            }
        }
    }

    fn persist_extension_statuses(
        &self,
        statuses: &BTreeMap<String, bool>,
    ) -> Result<(), CoreError> {
        let path = self
            .extension_status_store
            .read()
            .expect("extension status store lock")
            .clone();
        let Some(path) = path else {
            return Ok(());
        };

        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|err| {
                CoreError::new(
                    "PERSISTENCE_ERROR",
                    format!("could not create extension status store directory: {err}"),
                )
            })?;
        }

        let known = self.extension_names().into_iter().collect::<BTreeSet<_>>();
        let document = ExtensionStatusDocument {
            extensions: statuses
                .iter()
                .filter(|(name, _)| known.contains(*name))
                .map(|(name, enabled)| (name.clone(), *enabled))
                .collect(),
        };
        let body = serde_json::to_vec_pretty(&document).map_err(|err| {
            CoreError::new(
                "PERSISTENCE_ERROR",
                format!("could not serialize extension status store: {err}"),
            )
        })?;
        fs::write(&path, body).map_err(|err| {
            CoreError::new(
                "PERSISTENCE_ERROR",
                format!(
                    "could not write extension status store {}: {err}",
                    path.display()
                ),
            )
        })
    }

    fn find_extension_name(&self, name: &str) -> Result<String, CoreError> {
        self.extension_names()
            .into_iter()
            .find(|candidate| candidate == name)
            .ok_or_else(|| CoreError::new("NOT_FOUND", format!("extension not found: {name}")))
    }

    fn extension_names(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        for middleware in self.middlewares() {
            names.insert(middleware.name().to_string());
        }
        if let Some(provider) = self.registered_auth_provider() {
            names.insert(provider.name().to_string());
        }
        if let Some(provider) = self.registered_durable_sql_provider() {
            names.insert(provider.name().to_string());
        }
        if let Some(provider) = self.registered_key_value_provider() {
            names.insert(provider.name().to_string());
        }
        if let Some(provider) = self.registered_queue_provider() {
            names.insert(provider.name().to_string());
        }
        names.into_iter().collect()
    }

    fn registered_auth_provider(&self) -> Option<Arc<dyn AuthProvider>> {
        (*self.auth_provider).clone()
    }

    fn registered_durable_sql_provider(&self) -> Option<Arc<dyn DurableSqlProvider>> {
        (*self.durable_sql_provider).clone()
    }

    fn registered_key_value_provider(&self) -> Option<Arc<dyn KeyValueProvider>> {
        (*self.key_value_provider).clone()
    }

    fn registered_queue_provider(&self) -> Option<Arc<dyn QueueProvider>> {
        (*self.queue_provider).clone()
    }

    fn ensure_capability_declared(
        &self,
        extension_name: &str,
        expected: &ExtensionCapability,
        capabilities: Vec<ExtensionCapability>,
    ) -> Result<(), CoreError> {
        if capabilities.iter().any(|capability| capability == expected) {
            Ok(())
        } else {
            Err(CoreError::new(
                "INVALID_EXTENSION",
                format!(
                    "extension {extension_name} does not declare capability {}",
                    expected.label()
                ),
            ))
        }
    }

    fn ensure_dependencies(
        &self,
        extension_name: &str,
        dependencies: Vec<ExtensionDependency>,
    ) -> Result<(), CoreError> {
        for dependency in dependencies {
            if !self.has_capability(&dependency.capability) {
                return Err(CoreError::new(
                    "MISSING_DEPENDENCY",
                    format!(
                        "extension {extension_name} requires capability {}",
                        dependency.capability.label()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn has_capability(&self, expected: &ExtensionCapability) -> bool {
        self.active_middlewares().iter().any(|extension| {
            extension
                .capabilities()
                .iter()
                .any(|capability| capability == expected)
        }) || self.auth_provider().is_some_and(|extension| {
            extension
                .capabilities()
                .iter()
                .any(|capability| capability == expected)
        }) || self.durable_sql_provider().is_some_and(|extension| {
            extension
                .capabilities()
                .iter()
                .any(|capability| capability == expected)
        }) || self.key_value_provider().is_some_and(|extension| {
            extension
                .capabilities()
                .iter()
                .any(|capability| capability == expected)
        }) || self.queue_provider().is_some_and(|extension| {
            extension
                .capabilities()
                .iter()
                .any(|capability| capability == expected)
        })
    }

    fn register_service_provider(
        &self,
        extension_name: &str,
        _priority: i32,
        capabilities: Vec<ExtensionCapability>,
        dependencies: Vec<ExtensionDependency>,
        expected: &ExtensionCapability,
    ) -> Result<(), CoreError> {
        self.ensure_capability_declared(extension_name, expected, capabilities)?;
        self.ensure_dependencies(extension_name, dependencies)
    }

    /// Build a registry from an explicit extension list (story 06.01 — chosen pattern).
    ///
    /// The `edger` binary is the composition root: each `edger-ext-*` crate exports a
    /// constructor; the bin calls `collect_extensions()` and passes the result here.
    pub fn from_explicit<I>(middlewares: I) -> Result<Self, CoreError>
    where
        I: IntoIterator<Item = Arc<dyn Middleware>>,
    {
        let mut registry = Self::new();
        for middleware in middlewares {
            registry.register(middleware)?;
        }
        Ok(registry)
    }
}

/// Composition helper — explicit static registration (no inventory/linkme in v1).
pub fn collect_extensions(
    middlewares: Vec<Arc<dyn Middleware>>,
) -> Result<ExtensionRegistry, CoreError> {
    ExtensionRegistry::from_explicit(middlewares)
}

fn duplicate_provider_error(capability: &ExtensionCapability) -> CoreError {
    CoreError::new(
        "COLLISION",
        format!("provider already registered for {}", capability.label()),
    )
}

struct AdminExtensionRegistration {
    capabilities: Vec<ExtensionCapability>,
    dependencies: Vec<ExtensionDependency>,
    diagnostics: Option<Value>,
    kind: &'static str,
    name: &'static str,
    priority: i32,
    status: &'static str,
}

fn upsert_admin_extension(
    by_name: &mut BTreeMap<String, AdminExtensionInfo>,
    extension: AdminExtensionRegistration,
) {
    let labels = extension
        .capabilities
        .into_iter()
        .map(|capability| capability.label())
        .collect::<Vec<_>>();
    let dependency_labels = extension
        .dependencies
        .into_iter()
        .map(|dependency| dependency.capability.label())
        .collect::<Vec<_>>();
    let entry = by_name
        .entry(extension.name.to_string())
        .or_insert_with(|| AdminExtensionInfo {
            capabilities: vec![],
            config_source: "staticRegistration".into(),
            dependencies: vec![],
            diagnostics: None,
            id: format!("extension:{}", extension.name),
            kind: extension.kind.into(),
            name: extension.name.into(),
            priority: extension.priority,
            status: extension.status.into(),
            version: env!("CARGO_PKG_VERSION").into(),
        });
    if entry.kind != extension.kind {
        entry.kind = "mixed".into();
    }
    entry.priority = entry.priority.min(extension.priority);
    entry.status = extension.status.into();
    if entry.diagnostics.is_none() {
        entry.diagnostics = extension.diagnostics.map(sanitize_diagnostics);
    }
    for label in labels {
        if !entry.capabilities.contains(&label) {
            entry.capabilities.push(label);
        }
    }
    entry.capabilities.sort();
    for label in dependency_labels {
        if !entry.dependencies.contains(&label) {
            entry.dependencies.push(label);
        }
    }
    entry.dependencies.sort();
}

fn sanitize_diagnostics(value: Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.into_iter().map(sanitize_diagnostics).collect())
        }
        Value::Object(values) => Value::Object(sanitize_diagnostic_object(values)),
        Value::String(value) if is_sensitive_diagnostic_value(&value) => "[redacted]".into(),
        value => value,
    }
}

fn sanitize_diagnostic_object(values: Map<String, Value>) -> Map<String, Value> {
    values
        .into_iter()
        .map(|(key, value)| {
            if is_sensitive_diagnostic_key(&key) {
                (key, "[redacted]".into())
            } else {
                (key, sanitize_diagnostics(value))
            }
        })
        .collect()
}

fn is_sensitive_diagnostic_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    normalized.contains("authorization")
        || normalized.contains("cookie")
        || normalized.contains("credential")
        || normalized.contains("header")
        || normalized.contains("password")
        || normalized.contains("path")
        || normalized.contains("secret")
        || normalized.contains("token")
}

fn is_sensitive_diagnostic_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("authorization")
        || lower.contains("bearer ")
        || lower.contains("token=")
        || lower.contains("api_key=")
        || lower.contains("apikey=")
        || lower.contains("secret=")
        || looks_like_sensitive_filesystem_path(value)
}

fn looks_like_sensitive_filesystem_path(value: &str) -> bool {
    value.starts_with("/Users/")
        || value.starts_with("/home/")
        || value.starts_with("/private/")
        || value.starts_with("/root/")
        || value.starts_with("/tmp/")
        || value.starts_with("/var/")
        || value.starts_with("~/")
        || value
            .get(1..3)
            .is_some_and(|prefix| prefix == ":/" || prefix == ":\\")
}

fn status_label(enabled: bool) -> &'static str {
    if enabled {
        "enabled"
    } else {
        "disabled"
    }
}

fn read_extension_status_document(
    path: &Path,
) -> Result<Option<ExtensionStatusDocument>, CoreError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(CoreError::new(
                "PERSISTENCE_ERROR",
                format!(
                    "could not read extension status store {}: {err}",
                    path.display()
                ),
            ))
        }
    };
    serde_json::from_str(&text).map(Some).map_err(|err| {
        CoreError::new(
            "PARSE_ERROR",
            format!("invalid extension status store: {err}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use edger_core::{
        Extension, ExtensionContext, Middleware, RequestContext, SerializedRequest,
        SerializedResponse,
    };

    struct NamedMiddleware {
        name: &'static str,
        priority: i32,
    }

    impl Extension for NamedMiddleware {
        fn name(&self) -> &'static str {
            self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
            Ok(())
        }
    }

    impl Middleware for NamedMiddleware {
        fn on_request(
            &self,
            _req: &mut SerializedRequest,
            _ctx: &RequestContext,
        ) -> Result<Option<SerializedResponse>> {
            Ok(None)
        }
    }

    #[test]
    fn rejects_duplicate_names() {
        let mut registry = ExtensionRegistry::new();
        registry
            .register(Arc::new(NamedMiddleware {
                name: "dup",
                priority: 0,
            }))
            .unwrap();
        let err = registry
            .register(Arc::new(NamedMiddleware {
                name: "dup",
                priority: 1,
            }))
            .unwrap_err();
        assert_eq!(err.code, "COLLISION");
    }

    #[test]
    fn sorts_by_priority() {
        let mut registry = ExtensionRegistry::new();
        registry
            .register(Arc::new(NamedMiddleware {
                name: "late",
                priority: 10,
            }))
            .unwrap();
        registry
            .register(Arc::new(NamedMiddleware {
                name: "early",
                priority: -10,
            }))
            .unwrap();
        assert_eq!(registry.middlewares()[0].name(), "early");
        assert_eq!(registry.middlewares()[1].name(), "late");
    }

    #[test]
    fn extension_status_store_survives_registry_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let status_path = dir.path().join("extension-status.json");
        let mut registry = ExtensionRegistry::new();
        registry
            .register(Arc::new(NamedMiddleware {
                name: "gateway",
                priority: 0,
            }))
            .unwrap();
        registry.load_extension_status_store(&status_path).unwrap();

        let disabled = registry.set_extension_enabled("gateway", false).unwrap();

        assert_eq!(disabled.status, "disabled");
        let stored: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&status_path).unwrap()).unwrap();
        assert_eq!(stored["extensions"]["gateway"], false);

        let mut rebuilt = ExtensionRegistry::new();
        rebuilt
            .register(Arc::new(NamedMiddleware {
                name: "gateway",
                priority: 0,
            }))
            .unwrap();
        rebuilt.load_extension_status_store(&status_path).unwrap();

        assert!(!rebuilt.is_extension_enabled("gateway"));
        assert_eq!(
            rebuilt.admin_extension("gateway").unwrap().status,
            "disabled"
        );
    }
}
