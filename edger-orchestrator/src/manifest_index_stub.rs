//! In-memory manifest index for routing tests (story 05.02).
//!
//! Full multi-dir loading lands in story 07.01.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use edger_core::{
    create_worker_ref, principal_can_access_optional_namespace, AdminWorkerInfo, ApiKeyPrincipal,
    CoreError, CronJob, WorkerManifest, WorkerRef,
};

use crate::router::PluginRef;

#[derive(Clone, Debug)]
pub struct ManifestEntry {
    pub worker: WorkerRef,
    pub plugin_base: Option<String>,
}

/// Minimal manifest registry used by `resolve_route`.
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
        let worker = create_worker_ref(dir, manifest.clone())?;
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

    pub fn set_worker_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<AdminWorkerInfo, CoreError> {
        let mut state = self.inner.write().map_err(|_| lock_err())?;
        let bucket = state
            .entries
            .get_mut(name)
            .ok_or_else(|| CoreError::new("NOT_FOUND", format!("worker not found: {name}")))?;
        let resolved_version = resolve_semver(
            bucket.iter().map(|e| e.worker.version.as_str()).collect(),
            None,
        )?;
        let entry = bucket
            .iter_mut()
            .find(|entry| entry.worker.version == resolved_version)
            .ok_or_else(|| {
                CoreError::new(
                    "NOT_FOUND",
                    format!("worker {name}@{resolved_version} not found"),
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
        plugin_base: entry.plugin_base.clone(),
        public_routes: entry
            .worker
            .config
            .public_routes
            .as_ref()
            .map(|routes| routes.routes.clone())
            .unwrap_or_default(),
        source: entry.worker.dir.display().to_string(),
        status: if entry.worker.config.enabled {
            "loaded"
        } else {
            "disabled"
        }
        .into(),
        version: entry.worker.version.clone(),
        visibility: entry.worker.config.visibility.clone(),
    }
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
    fn base_root_registers_shell_without_plugin_wildcard() {
        let mut index = ManifestIndex::new();
        let mut manifest = manifest("shell-demo", "1.0.0");
        manifest.base = Some("/".into());
        manifest.shell_excludes = vec!["todos".into()];
        index.insert(PathBuf::from("/w/shell"), manifest).unwrap();

        let shell = index.shell().unwrap();
        assert_eq!(shell.name, "shell-demo");
        assert_eq!(shell.config.shell_excludes, vec!["todos"]);
        assert!(index.plugin_for_path("/todos").is_none());
        assert!(index.homepage().is_some());
    }
}
