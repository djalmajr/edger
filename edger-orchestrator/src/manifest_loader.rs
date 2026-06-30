//! Filesystem manifest discovery for worker directories (story 07.01).

use std::fs;
use std::path::{Path, PathBuf};

use edger_core::{CoreError, WorkerManifest};
use serde::Deserialize;

use crate::manifest_index_stub::ManifestIndex;

const ENTRYPOINT_CANDIDATES: [&str; 6] = [
    "index.html",
    "index.ts",
    "index.js",
    "index.mjs",
    "index.wasm",
    "index.wat",
];
const MANIFEST_CANDIDATES: [&str; 2] = ["manifest.yaml", "manifest.yml"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageJson {
    main: Option<String>,
    module: Option<String>,
    name: Option<String>,
    version: Option<String>,
}

/// Parse `RUNTIME_WORKER_DIRS` syntax (`:` separated) into paths.
pub fn parse_runtime_worker_dirs(raw: &str) -> Vec<PathBuf> {
    raw.split(':')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Load all worker manifests from root directories or direct worker directories.
pub fn load_manifests_from_dirs(paths: &[PathBuf]) -> Result<ManifestIndex, CoreError> {
    let mut index = ManifestIndex::new();

    for worker_dir in discover_worker_dirs(paths)? {
        let manifest = load_worker_manifest(&worker_dir)?;
        if manifest.enabled == Some(false) {
            continue;
        }
        index.insert(worker_dir, manifest)?;
    }

    Ok(index)
}

fn discover_worker_dirs(paths: &[PathBuf]) -> Result<Vec<PathBuf>, CoreError> {
    let mut dirs = Vec::new();

    for path in paths {
        if is_worker_dir(path) {
            dirs.push(path.clone());
            continue;
        }

        let entries = fs::read_dir(path).map_err(|e| {
            CoreError::new(
                "MANIFEST_IO",
                format!("failed to read worker root {}: {e}", path.display()),
            )
        })?;

        let mut children = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && is_worker_dir(path))
            .collect::<Vec<_>>();
        children.sort();
        dirs.extend(children);
    }

    dirs.sort();
    Ok(dirs)
}

fn is_worker_dir(path: &Path) -> bool {
    MANIFEST_CANDIDATES
        .iter()
        .any(|name| path.join(name).is_file())
        || path.join("package.json").is_file()
        || ENTRYPOINT_CANDIDATES
            .iter()
            .any(|entry| path.join(entry).is_file())
}

fn load_worker_manifest(worker_dir: &Path) -> Result<WorkerManifest, CoreError> {
    for manifest_name in MANIFEST_CANDIDATES {
        let path = worker_dir.join(manifest_name);
        if path.is_file() {
            let text = fs::read_to_string(&path).map_err(|e| {
                CoreError::new(
                    "MANIFEST_IO",
                    format!("failed to read {}: {e}", path.display()),
                )
            })?;
            let manifest = serde_yaml::from_str(&text)
                .map_err(|e| CoreError::parse(format!("failed to parse {}: {e}", path.display())));
            return manifest.and_then(|manifest| complete_manifest(worker_dir, manifest));
        }
    }

    if worker_dir.join("package.json").is_file() {
        return load_package_json_manifest(worker_dir);
    }

    Ok(default_manifest(worker_dir, None, None))
}

fn complete_manifest(
    worker_dir: &Path,
    mut manifest: WorkerManifest,
) -> Result<WorkerManifest, CoreError> {
    let package = read_package_json(worker_dir)?;
    if manifest.name.is_empty() {
        manifest.name = package
            .as_ref()
            .and_then(|package| package.name.clone())
            .unwrap_or_else(|| dir_name(worker_dir));
    }
    if manifest.version.is_none() {
        manifest.version = package.as_ref().and_then(|package| package.version.clone());
    }
    if manifest.entrypoint.is_none() {
        manifest.entrypoint = package
            .and_then(|package| package.module.or(package.main))
            .or_else(|| infer_entrypoint(worker_dir));
    }
    Ok(manifest)
}

fn load_package_json_manifest(worker_dir: &Path) -> Result<WorkerManifest, CoreError> {
    let package = read_package_json(worker_dir)?.ok_or_else(|| {
        CoreError::new(
            "MANIFEST_IO",
            format!("missing package.json in {}", worker_dir.display()),
        )
    })?;
    let entrypoint = package
        .module
        .or(package.main)
        .or_else(|| infer_entrypoint(worker_dir));

    let mut manifest = default_manifest(worker_dir, package.name, package.version);
    manifest.entrypoint = entrypoint;
    Ok(manifest)
}

fn read_package_json(worker_dir: &Path) -> Result<Option<PackageJson>, CoreError> {
    let path = worker_dir.join("package.json");
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| {
        CoreError::new(
            "MANIFEST_IO",
            format!("failed to read {}: {e}", path.display()),
        )
    })?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| CoreError::parse(format!("failed to parse {}: {e}", path.display())))
}

fn default_manifest(
    worker_dir: &Path,
    name: Option<String>,
    version: Option<String>,
) -> WorkerManifest {
    WorkerManifest {
        name: name.unwrap_or_else(|| dir_name(worker_dir)),
        version,
        entrypoint: infer_entrypoint(worker_dir),
        ..Default::default()
    }
}

fn infer_entrypoint(worker_dir: &Path) -> Option<String> {
    ENTRYPOINT_CANDIDATES
        .iter()
        .find(|entry| worker_dir.join(entry).is_file())
        .map(|entry| (*entry).to_string())
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("worker")
        .to_string()
}
