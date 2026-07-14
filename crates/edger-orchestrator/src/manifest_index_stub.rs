//! In-memory manifest index for routing tests (story 05.02).
//!
//! Full multi-dir loading lands in story 07.01.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use edger_core::{
    create_worker_ref, principal_can_access_optional_namespace, AdminWorkerHealthCheckInfo,
    AdminWorkerInfo, ApiKeyPrincipal, CoreError, CronJob, WorkerHealthCheckMode, WorkerManifest,
    WorkerOrigin, WorkerRef,
};

use crate::router::PluginRef;

#[derive(Clone, Debug)]
pub struct ManifestEntry {
    pub worker: WorkerRef,
    pub plugin_base: Option<String>,
    pub origin: WorkerOrigin,
}

/// Minimal manifest index used by `resolve_route`.
#[derive(Clone, Debug, Default)]
pub struct ManifestIndex {
    inner: Arc<RwLock<ManifestIndexState>>,
}

#[derive(Clone, Debug, Default)]
struct ManifestIndexState {
    entries: HashMap<String, Vec<ManifestEntry>>,
    host_routes: HashMap<String, WorkerRef>,
    plugins: Vec<PluginRef>,
    homepage: Option<WorkerRef>,
    shell: Option<WorkerRef>,
    core_bundled_roots: Vec<PathBuf>,
    core_overlay_root: Option<PathBuf>,
    user_roots: Vec<PathBuf>,
}

impl ManifestIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_homepage(self, worker: WorkerRef) -> Self {
        self.inner.write().expect("manifest index lock").homepage = Some(worker);
        self
    }

    pub fn insert(&mut self, dir: PathBuf, manifest: WorkerManifest) -> Result<(), CoreError> {
        self.insert_with_origin(dir, manifest, WorkerOrigin::User)
    }

    pub fn insert_with_origin(
        &mut self,
        dir: PathBuf,
        manifest: WorkerManifest,
        origin: WorkerOrigin,
    ) -> Result<(), CoreError> {
        let worker = create_worker_ref(dir, manifest.clone())?;
        validate_origin_identity(&worker.name, manifest.base.as_deref(), origin)?;
        let key = worker.name.clone();
        let mut state = self.inner.write().map_err(|_| lock_err())?;
        if state
            .entries
            .get(&key)
            .is_some_and(|bucket| bucket.iter().any(|e| e.worker.version == worker.version))
        {
            return Err(CoreError::new(
                "COLLISION",
                format!("duplicate worker {}@{}", worker.name, worker.version),
            ));
        }

        let host_aliases = normalize_host_aliases(&manifest.hosts)?;
        for host in &host_aliases {
            if state.host_routes.contains_key(host) {
                return Err(CoreError::new(
                    "COLLISION",
                    format!("duplicate host route: {host}"),
                ));
            }
        }

        let plugin_base = manifest.base.as_deref().and_then(normalize_base);
        if plugin_base.as_deref() == Some("/") {
            state.homepage = Some(worker.clone());
            state.shell = Some(worker.clone());
        } else if let Some(base) = plugin_base.clone() {
            state.plugins.push(PluginRef {
                name: worker.name.clone(),
                base,
                dir: worker.dir.clone(),
                manifest: manifest.clone(),
            });
            state
                .plugins
                .sort_by(|a, b| b.base.len().cmp(&a.base.len()));
        }
        for host in host_aliases {
            state.host_routes.insert(host, worker.clone());
        }

        state.entries.entry(key).or_default().push(ManifestEntry {
            plugin_base,
            origin,
            worker,
        });
        Ok(())
    }

    pub fn resolve_worker(
        &self,
        name: &str,
        version: Option<&str>,
    ) -> Result<WorkerRef, CoreError> {
        let state = self.inner.read().map_err(|_| lock_err())?;
        let bucket = state
            .entries
            .get(name)
            .ok_or_else(|| CoreError::new("NOT_FOUND", format!("worker not found: {name}")))?;
        let enabled = bucket
            .iter()
            .filter(|entry| entry.worker.config.enabled)
            .collect::<Vec<_>>();
        if enabled.is_empty() {
            return Err(CoreError::new(
                "NOT_FOUND",
                format!("worker not found: {name}"),
            ));
        }

        let resolved_version = resolve_semver(
            enabled.iter().map(|e| e.worker.version.as_str()).collect(),
            version,
        )?;

        enabled
            .iter()
            .find(|e| e.worker.version == resolved_version)
            .map(|e| e.worker.clone())
            .ok_or_else(|| {
                CoreError::new(
                    "NOT_FOUND",
                    format!("worker {name}@{resolved_version} not found"),
                )
            })
    }

    pub fn resolve_plugin_worker(&self, plugin: &PluginRef) -> Result<WorkerRef, CoreError> {
        let state = self.inner.read().map_err(|_| lock_err())?;
        state
            .entries
            .get(&plugin.name)
            .and_then(|entries| {
                entries.iter().find(|entry| {
                    entry.worker.config.enabled
                        && entry.worker.dir == plugin.dir
                        && entry.plugin_base.as_deref() == Some(plugin.base.as_str())
                })
            })
            .map(|entry| entry.worker.clone())
            .ok_or_else(|| {
                CoreError::new("NOT_FOUND", format!("worker not found: {}", plugin.name))
            })
    }

    pub fn plugin_for_path(&self, path: &str) -> Option<(PluginRef, String)> {
        let state = self.inner.read().ok()?;
        for plugin in &state.plugins {
            if !state.plugin_is_enabled(plugin) {
                continue;
            }
            if path == plugin.base || path.starts_with(&format!("{}/", plugin.base)) {
                let remainder = path
                    .strip_prefix(&plugin.base)
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .to_string();
                return Some((plugin.clone(), remainder));
            }
        }
        None
    }

    pub fn worker_for_host(&self, host: &str) -> Option<WorkerRef> {
        let normalized = normalize_host_alias(host).ok()??;
        let state = self.inner.read().ok()?;
        let worker = state.host_routes.get(&normalized)?;
        state.worker_ref_is_enabled(worker).then(|| worker.clone())
    }

    pub fn homepage(&self) -> Option<WorkerRef> {
        let state = self.inner.read().ok()?;
        state
            .homepage
            .as_ref()
            .filter(|worker| state.worker_ref_is_enabled(worker))
            .cloned()
    }

    pub fn shell(&self) -> Option<WorkerRef> {
        let state = self.inner.read().ok()?;
        state
            .shell
            .as_ref()
            .filter(|worker| state.worker_ref_is_enabled(worker))
            .cloned()
    }

    pub fn admin_workers(&self) -> Vec<AdminWorkerInfo> {
        let Ok(state) = self.inner.read() else {
            return Vec::new();
        };
        let mut workers = state
            .entries
            .values()
            .flat_map(|entries| entries.iter().map(admin_worker_info))
            .collect::<Vec<_>>();
        workers.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.version.cmp(&b.version))
                .then_with(|| a.source.cmp(&b.source))
        });
        workers
    }

    pub fn admin_workers_for_principal(&self, principal: &ApiKeyPrincipal) -> Vec<AdminWorkerInfo> {
        self.admin_workers()
            .into_iter()
            .filter(|worker| {
                principal_can_access_optional_namespace(principal, worker.namespace.as_deref())
            })
            .collect()
    }

    pub fn worker_refs(&self) -> Vec<WorkerRef> {
        let Ok(state) = self.inner.read() else {
            return Vec::new();
        };
        let mut workers = state
            .entries
            .values()
            .flat_map(|entries| entries.iter().map(|entry| entry.worker.clone()))
            .collect::<Vec<_>>();
        workers.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.version.cmp(&b.version))
                .then_with(|| a.dir.cmp(&b.dir))
        });
        workers
    }

    pub fn enabled_cron_jobs(&self) -> Vec<(WorkerRef, Vec<CronJob>)> {
        let Ok(state) = self.inner.read() else {
            return Vec::new();
        };
        state
            .entries
            .values()
            .flat_map(|entries| {
                entries.iter().filter_map(|entry| {
                    if entry.worker.config.enabled && !entry.worker.config.cron.is_empty() {
                        Some((entry.worker.clone(), entry.worker.config.cron.clone()))
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    pub fn set_root_config(
        &self,
        core_bundled_roots: Vec<PathBuf>,
        core_overlay_root: Option<PathBuf>,
        user_roots: Vec<PathBuf>,
    ) {
        if let Ok(mut state) = self.inner.write() {
            state.core_bundled_roots = core_bundled_roots;
            state.core_overlay_root = core_overlay_root;
            state.user_roots = user_roots;
        }
    }

    pub fn set_roots(&self, user_roots: Vec<PathBuf>) {
        self.set_root_config(Vec::new(), None, user_roots);
    }

    pub fn all_roots(&self) -> Vec<(PathBuf, WorkerOrigin)> {
        self.inner
            .read()
            .map(|state| {
                state
                    .core_bundled_roots
                    .iter()
                    .cloned()
                    .map(|root| (root, WorkerOrigin::CoreBundled))
                    .chain(
                        state
                            .core_overlay_root
                            .iter()
                            .cloned()
                            .map(|root| (root, WorkerOrigin::CoreOverlay)),
                    )
                    .chain(
                        state
                            .user_roots
                            .iter()
                            .cloned()
                            .map(|root| (root, WorkerOrigin::User)),
                    )
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn install_root(&self, core: bool) -> Option<PathBuf> {
        self.inner.read().ok().and_then(|state| {
            if core {
                state.core_overlay_root.clone()
            } else {
                state.user_roots.first().cloned()
            }
        })
    }

    /// Remove one `name@version` entry (rescan of a worker deleted on disk).
    pub fn remove_worker(&self, name: &str, version: &str) -> Result<(), CoreError> {
        let mut state = self.inner.write().map_err(|_| lock_err())?;
        let bucket = state
            .entries
            .get_mut(name)
            .ok_or_else(|| CoreError::new("NOT_FOUND", format!("worker not found: {name}")))?;
        let Some(position) = bucket
            .iter()
            .position(|entry| entry.worker.version == version)
        else {
            return Err(CoreError::new(
                "NOT_FOUND",
                format!("worker {name}@{version} not found"),
            ));
        };
        let removed = bucket.remove(position);
        if removed.origin == WorkerOrigin::CoreBundled {
            bucket.insert(position, removed);
            return Err(CoreError::new(
                "CORE_BUNDLED_IMMUTABLE",
                format!("bundled core worker {name}@{version} cannot be removed"),
            ));
        }
        let removes_last_core_default = removed.origin != WorkerOrigin::User
            && removed.worker.config.enabled
            && !bucket
                .iter()
                .any(|entry| entry.origin != WorkerOrigin::User && entry.worker.config.enabled);
        if removes_last_core_default {
            bucket.insert(position, removed);
            return Err(CoreError::new(
                "CORE_DEFAULT_REQUIRED",
                format!("core worker {name} must keep at least one enabled default version"),
            ));
        }
        if bucket.is_empty() {
            state.entries.remove(name);
        }
        let removed_dir = removed.worker.dir.clone();
        state.host_routes.retain(|_, worker| {
            !(worker.name == name && worker.version == version && worker.dir == removed_dir)
        });
        state
            .plugins
            .retain(|plugin| !(plugin.name == name && plugin.dir == removed_dir));
        if state
            .homepage
            .as_ref()
            .is_some_and(|worker| worker.name == name && worker.dir == removed_dir)
        {
            state.homepage = None;
        }
        if state
            .shell
            .as_ref()
            .is_some_and(|worker| worker.name == name && worker.dir == removed_dir)
        {
            state.shell = None;
        }
        Ok(())
    }

    /// Toggle a worker's enabled flag. With `version = None` the latest version
    /// in the bucket is targeted; with `Some(v)` that exact version is toggled,
    /// which is how rollback disables a bad release while an older one keeps
    /// serving `latest`.
    pub fn set_worker_enabled(
        &self,
        name: &str,
        version: Option<&str>,
        enabled: bool,
    ) -> Result<AdminWorkerInfo, CoreError> {
        let mut state = self.inner.write().map_err(|_| lock_err())?;
        let bucket = state
            .entries
            .get_mut(name)
            .ok_or_else(|| CoreError::new("NOT_FOUND", format!("worker not found: {name}")))?;
        let target_version = match version {
            Some(version) => version.to_string(),
            None => resolve_semver(
                bucket.iter().map(|e| e.worker.version.as_str()).collect(),
                None,
            )?,
        };
        let target_is_enabled = bucket
            .iter()
            .any(|entry| entry.worker.version == target_version && entry.worker.config.enabled);
        let enabled_count = bucket
            .iter()
            .filter(|entry| entry.worker.config.enabled)
            .count();
        let is_core = bucket
            .iter()
            .any(|entry| entry.origin != WorkerOrigin::User);
        if is_core && !enabled && target_is_enabled && enabled_count <= 1 {
            return Err(CoreError::new(
                "CORE_DEFAULT_REQUIRED",
                format!("core worker {name} must keep at least one enabled default version"),
            ));
        }
        let entry = bucket
            .iter_mut()
            .find(|entry| entry.worker.version == target_version)
            .ok_or_else(|| {
                CoreError::new(
                    "NOT_FOUND",
                    format!("worker {name}@{target_version} not found"),
                )
            })?;
        entry.worker.config.enabled = enabled;
        Ok(admin_worker_info(entry))
    }
}

impl ManifestIndexState {
    fn plugin_is_enabled(&self, plugin: &PluginRef) -> bool {
        self.entries.get(&plugin.name).is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.worker.config.enabled
                    && entry.worker.dir == plugin.dir
                    && entry.plugin_base.as_deref() == Some(plugin.base.as_str())
            })
        })
    }

    fn worker_ref_is_enabled(&self, worker: &WorkerRef) -> bool {
        self.entries.get(&worker.name).is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry.worker.config.enabled
                    && entry.worker.version == worker.version
                    && entry.worker.dir == worker.dir
            })
        })
    }
}

fn admin_worker_info(entry: &ManifestEntry) -> AdminWorkerInfo {
    AdminWorkerInfo {
        kind: entry.worker.kind.clone(),
        name: entry.worker.name.clone(),
        namespace: entry.worker.namespace.clone(),
        origin: entry.origin,
        plugin_base: entry.plugin_base.clone(),
        source: entry.worker.dir.display().to_string(),
        status: if entry.worker.config.enabled {
            "loaded"
        } else {
            "disabled"
        }
        .into(),
        version: entry.worker.version.clone(),
        health_check: entry.worker.config.health_check.as_ref().map(|check| {
            AdminWorkerHealthCheckInfo {
                path: check.path.clone(),
                method: check.method.clone(),
                mode: match check.mode {
                    WorkerHealthCheckMode::Manual => "manual",
                    WorkerHealthCheckMode::OnDeploy => "on-deploy",
                }
                .into(),
                timeout_ms: check.timeout_ms,
            }
        }),
    }
}

fn validate_origin_identity(
    name: &str,
    base: Option<&str>,
    origin: WorkerOrigin,
) -> Result<(), CoreError> {
    const RESERVED: [&str; 2] = ["cpanel", "webide"];
    let normalized_base = base.and_then(normalize_base);
    let claims_reserved_path = normalized_base.as_deref().is_some_and(|path| {
        RESERVED
            .iter()
            .any(|reserved| path == format!("/{reserved}"))
    });
    if origin == WorkerOrigin::User && (RESERVED.contains(&name) || claims_reserved_path) {
        return Err(CoreError::new(
            "CORE_NAME_RESERVED",
            format!("worker name or pathname is reserved for a core app: {name}"),
        ));
    }
    if origin != WorkerOrigin::User && !RESERVED.contains(&name) {
        return Err(CoreError::new(
            "CORE_NAME_INVALID",
            format!("unknown core worker name: {name}"),
        ));
    }
    Ok(())
}

fn normalize_base(base: &str) -> Option<String> {
    let trimmed = base.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    Some(normalized)
}

fn normalize_host_aliases(hosts: &[String]) -> Result<Vec<String>, CoreError> {
    let mut aliases = Vec::new();
    for host in hosts {
        let Some(alias) = normalize_host_alias(host)? else {
            continue;
        };
        if !aliases.contains(&alias) {
            aliases.push(alias);
        }
    }
    Ok(aliases)
}

fn normalize_host_alias(host: &str) -> Result<Option<String>, CoreError> {
    let trimmed = host.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.contains("://")
        || trimmed
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '/' | '\\' | '*' | '[' | ']' | '@'))
    {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            format!("invalid host route: {host}"),
        ));
    }
    let without_port = strip_host_port(trimmed);
    if without_port.is_empty() || without_port.contains(':') {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            format!("invalid host route: {host}"),
        ));
    }
    Ok(Some(without_port.to_ascii_lowercase()))
}

fn strip_host_port(host: &str) -> &str {
    let Some((name, port)) = host.rsplit_once(':') else {
        return host;
    };
    if !name.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()) {
        name
    } else {
        host
    }
}

fn lock_err() -> CoreError {
    CoreError::new("LOCK_ERROR", "manifest index lock poisoned")
}

fn resolve_semver(available: Vec<&str>, requested: Option<&str>) -> Result<String, CoreError> {
    let req = requested.unwrap_or("latest");
    if req == "latest" {
        return pick_highest(available);
    }
    if available.contains(&req) {
        return Ok(req.to_string());
    }
    if looks_like_version_req(req) {
        return pick_highest_matching(available, req);
    }
    Err(CoreError::new(
        "NOT_FOUND",
        format!("version {req} not available"),
    ))
}

fn looks_like_version_req(req: &str) -> bool {
    req.starts_with(['^', '~', '>', '<', '='])
        || req.contains(',')
        || req.contains("||")
        || req.contains('*')
        || req.contains('x')
        || req.contains('X')
}

fn pick_highest_matching(available: Vec<&str>, req: &str) -> Result<String, CoreError> {
    let requirement = semver::VersionReq::parse(req)
        .map_err(|_| CoreError::new("PARSE_ERROR", format!("invalid semver requirement: {req}")))?;
    let mut best: Option<semver::Version> = None;
    let mut best_raw = String::new();
    for version in available {
        if version == "latest" {
            continue;
        }
        let parsed = semver::Version::parse(version)
            .map_err(|_| CoreError::new("PARSE_ERROR", format!("invalid semver: {version}")))?;
        if requirement.matches(&parsed) && best.as_ref().is_none_or(|current| parsed > *current) {
            best = Some(parsed);
            best_raw = version.to_string();
        }
    }
    if best_raw.is_empty() {
        return Err(CoreError::new(
            "NOT_FOUND",
            format!("no version satisfies {req}"),
        ));
    }
    Ok(best_raw)
}

fn pick_highest(available: Vec<&str>) -> Result<String, CoreError> {
    let mut best: Option<semver::Version> = None;
    let mut best_raw = String::new();
    let mut has_latest = false;
    for v in available {
        if v == "latest" {
            has_latest = true;
            continue;
        }
        let parsed = semver::Version::parse(v)
            .map_err(|_| CoreError::new("PARSE_ERROR", format!("invalid semver: {v}")))?;
        if best.as_ref().is_none_or(|b| parsed > *b) {
            best = Some(parsed);
            best_raw = v.to_string();
        }
    }
    if best_raw.is_empty() && has_latest {
        return Ok("latest".into());
    }
    if best_raw.is_empty() {
        return Err(CoreError::new("NOT_FOUND", "no versions available"));
    }
    Ok(best_raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use edger_core::WorkerManifest;

    fn manifest(name: &str, version: &str) -> WorkerManifest {
        WorkerManifest {
            name: name.into(),
            version: Some(version.into()),
            ..Default::default()
        }
    }

    #[test]
    fn insert_detects_collision() {
        let mut index = ManifestIndex::new();
        let dir = PathBuf::from("/w/hello");
        index
            .insert(dir.clone(), manifest("hello", "1.0.0"))
            .unwrap();
        let err = index.insert(dir, manifest("hello", "1.0.0")).unwrap_err();
        assert_eq!(err.code, "COLLISION");
    }

    #[test]
    fn latest_picks_highest_semver() {
        let mut index = ManifestIndex::new();
        index
            .insert(PathBuf::from("/w/a"), manifest("@acme/api", "1.0.0"))
            .unwrap();
        index
            .insert(PathBuf::from("/w/b"), manifest("@acme/api", "2.0.0"))
            .unwrap();
        let worker = index.resolve_worker("@acme/api", None).unwrap();
        assert_eq!(worker.version, "2.0.0");
    }

    #[test]
    fn semver_range_picks_highest_matching_version() {
        let mut index = ManifestIndex::new();
        index
            .insert(PathBuf::from("/w/a"), manifest("@acme/api", "1.2.0"))
            .unwrap();
        index
            .insert(PathBuf::from("/w/b"), manifest("@acme/api", "1.4.0"))
            .unwrap();
        index
            .insert(PathBuf::from("/w/c"), manifest("@acme/api", "2.0.0"))
            .unwrap();

        let worker = index.resolve_worker("@acme/api", Some("^1.0.0")).unwrap();

        assert_eq!(worker.version, "1.4.0");
    }

    #[test]
    fn exact_version_request_does_not_behave_like_caret_range() {
        let mut index = ManifestIndex::new();
        index
            .insert(PathBuf::from("/w/a"), manifest("@acme/api", "1.0.1"))
            .unwrap();

        let err = index
            .resolve_worker("@acme/api", Some("1.0.0"))
            .unwrap_err();

        assert_eq!(err.code, "NOT_FOUND");
    }

    #[test]
    fn semver_range_without_match_returns_not_found() {
        let mut index = ManifestIndex::new();
        index
            .insert(PathBuf::from("/w/a"), manifest("@acme/api", "2.0.0"))
            .unwrap();

        let err = index
            .resolve_worker("@acme/api", Some("~1.2.0"))
            .unwrap_err();

        assert_eq!(err.code, "NOT_FOUND");
    }

    #[test]
    fn cpanel_rejects_disabling_its_last_enabled_version() {
        let mut index = ManifestIndex::new();
        index
            .insert_with_origin(
                PathBuf::from("/w/cpanel"),
                manifest("cpanel", "1.0.0"),
                WorkerOrigin::CoreBundled,
            )
            .unwrap();

        let err = index
            .set_worker_enabled("cpanel", Some("1.0.0"), false)
            .unwrap_err();

        assert_eq!(err.code, "CORE_DEFAULT_REQUIRED");
        assert_eq!(
            index.resolve_worker("cpanel", None).unwrap().version,
            "1.0.0"
        );
    }

    #[test]
    fn cpanel_can_disable_one_version_when_another_default_remains() {
        let mut index = ManifestIndex::new();
        index
            .insert_with_origin(
                PathBuf::from("/w/cpanel-v1"),
                manifest("cpanel", "1.0.0"),
                WorkerOrigin::CoreBundled,
            )
            .unwrap();
        index
            .insert_with_origin(
                PathBuf::from("/w/cpanel-v2"),
                manifest("cpanel", "2.0.0"),
                WorkerOrigin::CoreOverlay,
            )
            .unwrap();

        index
            .set_worker_enabled("cpanel", Some("2.0.0"), false)
            .unwrap();

        assert_eq!(
            index.resolve_worker("cpanel", None).unwrap().version,
            "1.0.0"
        );
    }

    #[test]
    fn user_worker_cannot_claim_core_name_or_pathname() {
        let mut index = ManifestIndex::new();
        let err = index
            .insert(PathBuf::from("/w/cpanel"), manifest("cpanel", "1.0.0"))
            .unwrap_err();
        assert_eq!(err.code, "CORE_NAME_RESERVED");

        let mut claims_path = manifest("other", "1.0.0");
        claims_path.base = Some("/webide".into());
        let err = index
            .insert(PathBuf::from("/w/other"), claims_path)
            .unwrap_err();
        assert_eq!(err.code, "CORE_NAME_RESERVED");
    }

    #[test]
    fn bundled_core_worker_cannot_be_removed() {
        let mut index = ManifestIndex::new();
        index
            .insert_with_origin(
                PathBuf::from("/w/webide"),
                manifest("webide", "1.0.0"),
                WorkerOrigin::CoreBundled,
            )
            .unwrap();

        let err = index.remove_worker("webide", "1.0.0").unwrap_err();

        assert_eq!(err.code, "CORE_BUNDLED_IMMUTABLE");
        assert_eq!(
            index.resolve_worker("webide", None).unwrap().version,
            "1.0.0"
        );
    }

    #[test]
    fn last_overlay_core_worker_cannot_be_removed() {
        let mut index = ManifestIndex::new();
        index
            .insert_with_origin(
                PathBuf::from("/overlay/webide"),
                manifest("webide", "1.0.0"),
                WorkerOrigin::CoreOverlay,
            )
            .unwrap();

        let err = index.remove_worker("webide", "1.0.0").unwrap_err();

        assert_eq!(err.code, "CORE_DEFAULT_REQUIRED");
        assert_eq!(index.admin_workers().len(), 1);
    }

    #[test]
    fn base_root_registers_shell_without_plugin_wildcard() {
        let mut index = ManifestIndex::new();
        let mut manifest = manifest("shell-demo", "1.0.0");
        manifest.base = Some("/".into());
        index.insert(PathBuf::from("/w/shell"), manifest).unwrap();

        let shell = index.shell().unwrap();
        assert_eq!(shell.name, "shell-demo");
        assert!(index.plugin_for_path("/todos").is_none());
        assert!(index.homepage().is_some());
    }
}
