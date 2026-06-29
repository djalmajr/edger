//! In-memory manifest index for routing tests (story 05.02).
//!
//! Full multi-dir loading lands in story 07.01.

use std::collections::HashMap;
use std::path::PathBuf;

use edger_core::{create_worker_ref, CoreError, WorkerManifest, WorkerRef};

use crate::router::PluginRef;

#[derive(Clone, Debug)]
pub struct ManifestEntry {
    pub worker: WorkerRef,
    pub plugin_base: Option<String>,
}

/// Minimal manifest registry used by `resolve_route`.
#[derive(Clone, Debug, Default)]
pub struct ManifestIndex {
    entries: HashMap<String, Vec<ManifestEntry>>,
    plugins: Vec<PluginRef>,
    homepage: Option<WorkerRef>,
}

impl ManifestIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_homepage(mut self, worker: WorkerRef) -> Self {
        self.homepage = Some(worker);
        self
    }

    pub fn insert(&mut self, dir: PathBuf, manifest: WorkerManifest) -> Result<(), CoreError> {
        let worker = create_worker_ref(dir, manifest.clone())?;
        let key = worker.name.clone();
        let bucket = self.entries.entry(key).or_default();

        if bucket
            .iter()
            .any(|e| e.worker.version == worker.version)
        {
            return Err(CoreError::new(
                "COLLISION",
                format!("duplicate worker {}@{}", worker.name, worker.version),
            ));
        }

        let plugin_base = manifest.base.as_ref().map(|b| normalize_base(b));
        if let Some(base) = plugin_base.clone() {
            self.plugins.push(PluginRef {
                name: worker.name.clone(),
                base,
                dir: worker.dir.clone(),
                manifest: manifest.clone(),
            });
            self.plugins
                .sort_by(|a, b| b.base.len().cmp(&a.base.len()));
        }

        bucket.push(ManifestEntry {
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
        let bucket = self
            .entries
            .get(name)
            .ok_or_else(|| CoreError::new("NOT_FOUND", format!("worker not found: {name}")))?;

        let resolved_version = resolve_semver(
            bucket
                .iter()
                .map(|e| e.worker.version.as_str())
                .collect(),
            version,
        )?;

        bucket
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
        for plugin in &self.plugins {
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

    pub fn homepage(&self) -> Option<WorkerRef> {
        self.homepage.clone()
    }
}

fn normalize_base(base: &str) -> String {
    let trimmed = base.trim();
    if trimmed.is_empty() {
        return "/".into();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn resolve_semver(available: Vec<&str>, requested: Option<&str>) -> Result<String, CoreError> {
    let req = requested.unwrap_or("latest");
    if req == "latest" {
        return pick_highest(available);
    }
    if available.contains(&req) {
        return Ok(req.to_string());
    }
    Err(CoreError::new(
        "NOT_FOUND",
        format!("version {req} not available"),
    ))
}

fn pick_highest(available: Vec<&str>) -> Result<String, CoreError> {
    let mut best: Option<semver::Version> = None;
    let mut best_raw = String::new();
    for v in available {
        if v == "latest" {
            continue;
        }
        let parsed = semver::Version::parse(v).map_err(|_| {
            CoreError::new("PARSE_ERROR", format!("invalid semver: {v}"))
        })?;
        if best.as_ref().is_none_or(|b| parsed > *b) {
            best = Some(parsed);
            best_raw = v.to_string();
        }
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
            .insert(
                PathBuf::from("/w/a"),
                manifest("@acme/api", "1.0.0"),
            )
            .unwrap();
        index
            .insert(
                PathBuf::from("/w/b"),
                manifest("@acme/api", "2.0.0"),
            )
            .unwrap();
        let worker = index.resolve_worker("@acme/api", None).unwrap();
        assert_eq!(worker.version, "2.0.0");
    }
}