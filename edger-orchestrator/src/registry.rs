//! Extension registry — ordered middleware storage (story 05.05).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use edger_core::{
    AdminExtensionInfo, AdminExtensionManifest, AdminExtensionManifestConfig,
    AdminExtensionManifestMenu, AdminExtensionReconcileAction, AdminExtensionReconcileActionKind,
    AdminExtensionReconcileClassification, AdminExtensionReconcileDiagnostics,
    AdminExtensionReconcileRequest, AdminExtensionReconcileResponse,
    AdminExtensionReconcileSummary, CoreError, ExtensionCapability, ExtensionDependency,
    Middleware,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Registry of middleware extensions sorted by `priority()` (lower runs first).
#[derive(Clone, Default)]
pub struct ExtensionRegistry {
    middlewares: Arc<Vec<Arc<dyn Middleware>>>,
    extension_status: Arc<RwLock<BTreeMap<String, bool>>>,
    extension_status_store: Arc<RwLock<Option<PathBuf>>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionStatusDocument {
    extensions: BTreeMap<String, bool>,
}

struct ReconcileStatusStore {
    desired_source: &'static str,
    label: &'static str,
}

impl ReconcileStatusStore {
    fn extension_status_file() -> Self {
        Self {
            desired_source: "extensionStatusFile",
            label: "configured",
        }
    }

    fn in_memory_overlay() -> Self {
        Self {
            desired_source: "inMemoryOverlay",
            label: "notConfigured",
        }
    }

    fn desired_source(&self) -> &str {
        self.desired_source
    }

    fn label(&self) -> &str {
        self.label
    }
}

const STATIC_REGISTRATION_CONFIG_SOURCE: &str = "staticRegistration";

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
        let mut extensions = by_name.into_values().collect::<Vec<_>>();

        extensions.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.name.cmp(&b.name))
        });
        extensions
    }

    pub fn reconcile_extensions(
        &self,
        request_id: String,
        request: &AdminExtensionReconcileRequest,
    ) -> Result<AdminExtensionReconcileResponse, CoreError> {
        let (desired, status_store) = self.desired_extension_statuses()?;
        let registered = self
            .admin_extensions()
            .into_iter()
            .map(|extension| extension.name)
            .collect::<BTreeSet<_>>();
        let mut actions = Vec::with_capacity(desired.len());

        for (name, desired_enabled) in desired {
            if registered.contains(&name) {
                let effective = self.is_extension_enabled(&name);
                let action = reconcile_action_kind(effective, desired_enabled);
                let applied = !request.dry_run && action != AdminExtensionReconcileActionKind::Noop;
                if applied {
                    self.set_extension_enabled_in_memory(&name, desired_enabled);
                }
                actions.push(AdminExtensionReconcileAction {
                    action,
                    applied,
                    classification: AdminExtensionReconcileClassification::Runtime,
                    from_enabled: Some(effective),
                    name: name.clone(),
                    to_enabled: Some(desired_enabled),
                });
            } else {
                actions.push(AdminExtensionReconcileAction {
                    action: desired_action_kind(desired_enabled),
                    applied: false,
                    classification: AdminExtensionReconcileClassification::RestartRequired,
                    from_enabled: None,
                    name: name.clone(),
                    to_enabled: Some(desired_enabled),
                });
            }
        }

        let summary = summarize_reconcile_actions(&actions);
        Ok(AdminExtensionReconcileResponse {
            actions,
            diagnostics: AdminExtensionReconcileDiagnostics {
                desired_source: status_store.desired_source().into(),
                dry_run: request.dry_run,
                dynamic_loading: false,
                effective_source: "inMemoryRegistry".into(),
                mode: if request.dry_run { "dryRun" } else { "apply" }.into(),
                status_store: status_store.label().into(),
            },
            request_id,
            summary,
        })
    }

    fn desired_extension_statuses(
        &self,
    ) -> Result<(BTreeMap<String, bool>, ReconcileStatusStore), CoreError> {
        let path = self
            .extension_status_store
            .read()
            .expect("extension status store lock")
            .clone();
        if let Some(path) = path {
            let statuses = read_extension_status_document(&path)?
                .map(|document| document.extensions)
                .unwrap_or_default();
            return Ok((statuses, ReconcileStatusStore::extension_status_file()));
        }

        let statuses = self
            .extension_status
            .read()
            .expect("extension status lock")
            .clone();
        Ok((statuses, ReconcileStatusStore::in_memory_overlay()))
    }

    fn set_extension_enabled_in_memory(&self, name: &str, enabled: bool) {
        self.extension_status
            .write()
            .expect("extension status lock")
            .insert(name.to_string(), enabled);
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
        names.into_iter().collect()
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
        })
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

fn reconcile_action_kind(effective: bool, desired: bool) -> AdminExtensionReconcileActionKind {
    if effective == desired {
        AdminExtensionReconcileActionKind::Noop
    } else {
        desired_action_kind(desired)
    }
}

fn desired_action_kind(desired: bool) -> AdminExtensionReconcileActionKind {
    if desired {
        AdminExtensionReconcileActionKind::Enable
    } else {
        AdminExtensionReconcileActionKind::Disable
    }
}

fn summarize_reconcile_actions(
    actions: &[AdminExtensionReconcileAction],
) -> AdminExtensionReconcileSummary {
    let mut summary = AdminExtensionReconcileSummary {
        total: actions.len() as u64,
        ..Default::default()
    };
    for action in actions {
        if action.applied {
            summary.applied += 1;
        }
        if action.action == AdminExtensionReconcileActionKind::Noop {
            summary.noop += 1;
        }
        match action.classification {
            AdminExtensionReconcileClassification::RestartRequired => {
                summary.restart_required += 1;
            }
            AdminExtensionReconcileClassification::Runtime => {
                summary.runtime += 1;
            }
        }
    }
    summary
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
        .iter()
        .map(|capability| capability.label())
        .collect::<Vec<_>>();
    let dependency_labels = extension
        .dependencies
        .iter()
        .map(|dependency| dependency.capability.label())
        .collect::<Vec<_>>();
    let manifest = admin_extension_manifest(&extension.capabilities, &extension.dependencies);
    let entry = by_name
        .entry(extension.name.to_string())
        .or_insert_with(|| AdminExtensionInfo {
            capabilities: vec![],
            config_source: STATIC_REGISTRATION_CONFIG_SOURCE.into(),
            dependencies: vec![],
            diagnostics: None,
            id: format!("extension:{}", extension.name),
            kind: extension.kind.into(),
            manifest: empty_admin_extension_manifest(),
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
    merge_admin_extension_manifest(&mut entry.manifest, manifest);
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

fn admin_extension_manifest(
    capabilities: &[ExtensionCapability],
    dependencies: &[ExtensionDependency],
) -> AdminExtensionManifest {
    let mut manifest = empty_admin_extension_manifest();
    for capability in capabilities {
        match capability {
            ExtensionCapability::LifecycleHook { hook } => {
                push_unique_string(&mut manifest.hooks, hook.label().into());
            }
            ExtensionCapability::MenuContribution { name } => {
                push_unique_menu(&mut manifest.menus, name.clone());
            }
            ExtensionCapability::RequestHook => {
                push_unique_string(&mut manifest.hooks, capability.label());
            }
            ExtensionCapability::ResponseHook => {
                push_unique_string(&mut manifest.hooks, capability.label());
            }
            ExtensionCapability::HostRouting
            | ExtensionCapability::Middleware
            | ExtensionCapability::WorkerHandler => {
                push_unique_string(&mut manifest.provides, capability.label());
            }
        }
    }
    for dependency in dependencies {
        push_unique_string(&mut manifest.requirements, dependency.capability.label());
    }
    sort_admin_extension_manifest(&mut manifest);
    manifest
}

fn empty_admin_extension_manifest() -> AdminExtensionManifest {
    AdminExtensionManifest {
        config: AdminExtensionManifestConfig {
            keys: vec![],
            redacted: true,
            source: STATIC_REGISTRATION_CONFIG_SOURCE.into(),
        },
        hooks: vec![],
        menus: vec![],
        provides: vec![],
        requirements: vec![],
    }
}

fn merge_admin_extension_manifest(
    entry: &mut AdminExtensionManifest,
    manifest: AdminExtensionManifest,
) {
    for key in manifest.config.keys {
        push_unique_string(&mut entry.config.keys, key);
    }
    entry.config.redacted |= manifest.config.redacted;
    for hook in manifest.hooks {
        push_unique_string(&mut entry.hooks, hook);
    }
    for menu in manifest.menus {
        push_unique_menu(&mut entry.menus, menu.name);
    }
    for capability in manifest.provides {
        push_unique_string(&mut entry.provides, capability);
    }
    for requirement in manifest.requirements {
        push_unique_string(&mut entry.requirements, requirement);
    }
    sort_admin_extension_manifest(entry);
}

fn sort_admin_extension_manifest(manifest: &mut AdminExtensionManifest) {
    manifest.config.keys.sort();
    manifest.hooks.sort();
    manifest.menus.sort_by(|a, b| a.name.cmp(&b.name));
    manifest.provides.sort();
    manifest.requirements.sort();
}

fn push_unique_menu(menus: &mut Vec<AdminExtensionManifestMenu>, name: String) {
    if !menus.iter().any(|menu| menu.name == name) {
        menus.push(AdminExtensionManifestMenu { name });
    }
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
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
