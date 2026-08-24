//! Filesystem manifest discovery for worker directories (story 07.01).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use edger_core::{AdminWorkerInfo, CoreError, WorkerManifest, WorkerOrigin, WorkerVisibility};
use serde::{Deserialize, Serialize};

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
    load_manifests_from_roots(&[], None, paths)
}

pub fn load_manifests_from_roots(
    core_bundled_roots: &[PathBuf],
    core_overlay_root: Option<&PathBuf>,
    user_roots: &[PathBuf],
) -> Result<ManifestIndex, CoreError> {
    let mut index = ManifestIndex::new();

    for (roots, origin) in [
        (core_bundled_roots, WorkerOrigin::CoreBundled),
        (
            core_overlay_root
                .filter(|root| root.exists())
                .map(std::slice::from_ref)
                .unwrap_or(&[]),
            WorkerOrigin::CoreOverlay,
        ),
        (user_roots, WorkerOrigin::User),
    ] {
        for (worker_dir, manifest) in scan_worker_manifests(roots)? {
            index.insert_with_origin(worker_dir, manifest, origin)?;
        }
    }

    index.set_root_config(
        core_bundled_roots.to_vec(),
        core_overlay_root.cloned(),
        user_roots.to_vec(),
    );
    reload_persisted_default_versions(&index);
    Ok(index)
}

const DEFAULT_VERSIONS_DIR: &str = ".edger-defaults";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedDefaultVersion {
    name: String,
    version: String,
}

pub(crate) fn persist_default_version(
    index: &ManifestIndex,
    name: &str,
    version: &str,
) -> Result<AdminWorkerInfo, CoreError> {
    let candidate = index.validate_promotion(name, version)?;
    let source = PathBuf::from(&candidate.source);
    let path = default_version_path(index, name, &source)?;
    let directory = path
        .parent()
        .ok_or_else(|| CoreError::new("DEPLOY_IO", "default version path has no parent"))?;
    fs::create_dir_all(directory).map_err(|error| {
        CoreError::new(
            "DEPLOY_IO",
            format!(
                "failed to create default version directory {}: {error}",
                directory.display()
            ),
        )
    })?;
    let temporary = directory.join(format!(
        ".{}-{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("default"),
        uuid::Uuid::new_v4()
    ));
    let mut file = fs::File::create(&temporary).map_err(|error| {
        CoreError::new(
            "DEPLOY_IO",
            format!(
                "failed to create default version temp file {}: {error}",
                temporary.display()
            ),
        )
    })?;
    serde_json::to_writer(
        &mut file,
        &PersistedDefaultVersion {
            name: name.to_string(),
            version: version.to_string(),
        },
    )
    .map_err(|error| {
        CoreError::new(
            "DEPLOY_IO",
            format!("failed to encode default version pointer: {error}"),
        )
    })?;
    file.write_all(b"\n")
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            CoreError::new(
                "DEPLOY_IO",
                format!(
                    "failed to persist default version temp file {}: {error}",
                    temporary.display()
                ),
            )
        })?;
    fs::rename(&temporary, &path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        CoreError::new(
            "DEPLOY_IO",
            format!(
                "failed to atomically publish default version {}: {error}",
                path.display()
            ),
        )
    })?;
    sync_directory(directory);
    index.promote_worker(name, version)
}

pub(crate) fn clear_persisted_default_version(
    index: &ManifestIndex,
    name: &str,
    source: &Path,
) -> Result<(), CoreError> {
    let path = default_version_path(index, name, source)?;
    match fs::remove_file(&path) {
        Ok(()) => {
            if let Some(directory) = path.parent() {
                sync_directory(directory);
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CoreError::new(
            "DEPLOY_IO",
            format!(
                "failed to remove default version pointer {}: {error}",
                path.display()
            ),
        )),
    }
}

pub(crate) fn reload_persisted_default_versions(index: &ManifestIndex) {
    index.clear_default_versions();
    let mut directories = index
        .all_roots()
        .into_iter()
        .filter_map(|(root, _)| pointer_root_for_configured_root(&root))
        .map(|root| root.join(DEFAULT_VERSIONS_DIR))
        .collect::<Vec<_>>();
    directories.sort();
    directories.dedup();
    for directory in directories {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let pointer = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<PersistedDefaultVersion>(&bytes).ok());
            let Some(pointer) = pointer else {
                tracing::warn!(
                    path = %path.display(),
                    "ignoring malformed persisted worker default version"
                );
                continue;
            };
            if path.file_name().and_then(|name| name.to_str())
                != Some(pointer_file_name(&pointer.name).as_str())
            {
                tracing::warn!(
                    path = %path.display(),
                    worker = %pointer.name,
                    "ignoring mismatched persisted worker default version filename"
                );
                continue;
            }
            let valid_source = index.worker_refs().into_iter().any(|worker| {
                worker.name == pointer.name
                    && worker.version == pointer.version
                    && worker.config.visibility == WorkerVisibility::Public
                    && default_version_path(index, &worker.name, &worker.dir)
                        .ok()
                        .as_ref()
                        == Some(&path)
            });
            if !valid_source {
                tracing::warn!(
                    worker = %pointer.name,
                    version = %pointer.version,
                    "persisted default version is unavailable or non-public; using semver fallback"
                );
                continue;
            }
            if let Err(error) = index.promote_worker(&pointer.name, &pointer.version) {
                tracing::warn!(
                    worker = %pointer.name,
                    version = %pointer.version,
                    error = %error,
                    "failed to restore persisted worker default version; using semver fallback"
                );
            }
        }
    }
}

fn default_version_path(
    index: &ManifestIndex,
    name: &str,
    source: &Path,
) -> Result<PathBuf, CoreError> {
    let root = index
        .all_roots()
        .into_iter()
        .map(|(root, _)| root)
        .filter(|root| source == root || source.starts_with(root))
        .max_by_key(|root| root.components().count())
        .and_then(|root| pointer_root_for_configured_root(&root))
        .or_else(|| source.parent().map(Path::to_path_buf))
        .ok_or_else(|| {
            CoreError::new(
                "DEPLOY_IO",
                format!("cannot locate worker root for {}", source.display()),
            )
        })?;
    Ok(root
        .join(DEFAULT_VERSIONS_DIR)
        .join(pointer_file_name(name)))
}

fn pointer_root_for_configured_root(root: &Path) -> Option<PathBuf> {
    if is_worker_dir(root) {
        root.parent().map(Path::to_path_buf)
    } else {
        Some(root.to_path_buf())
    }
}

fn pointer_file_name(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len() * 2 + 5);
    for byte in name.bytes() {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded.push_str(".json");
    encoded
}

fn sync_directory(directory: &Path) {
    if let Ok(file) = fs::File::open(directory) {
        let _ = file.sync_all();
    }
}

/// Scan worker roots and parse every enabled worker manifest, without
/// touching an index. Shared by boot loading and runtime rescan.
pub fn scan_worker_manifests(
    paths: &[PathBuf],
) -> Result<Vec<(PathBuf, WorkerManifest)>, CoreError> {
    let mut manifests = Vec::new();
    for worker_dir in discover_worker_dirs(paths)? {
        let manifest = load_worker_manifest(&worker_dir)?;
        if manifest.enabled == Some(false) {
            continue;
        }
        manifests.push((worker_dir, manifest));
    }
    Ok(manifests)
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

pub(crate) fn load_worker_manifest(worker_dir: &Path) -> Result<WorkerManifest, CoreError> {
    load_worker_manifest_with_name_fallback(worker_dir, None)
}

pub(crate) fn load_worker_manifest_with_name_fallback(
    worker_dir: &Path,
    name_fallback: Option<&str>,
) -> Result<WorkerManifest, CoreError> {
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
            return manifest
                .and_then(|manifest| complete_manifest(worker_dir, manifest, name_fallback));
        }
    }

    if worker_dir.join("package.json").is_file() {
        return load_package_json_manifest(worker_dir, name_fallback);
    }

    Ok(default_manifest(
        worker_dir,
        name_fallback.map(str::to_owned),
        None,
    ))
}

fn complete_manifest(
    worker_dir: &Path,
    mut manifest: WorkerManifest,
    name_fallback: Option<&str>,
) -> Result<WorkerManifest, CoreError> {
    let package = read_package_json(worker_dir)?;
    if manifest.name.is_empty() {
        manifest.name = package
            .as_ref()
            .and_then(|package| package.name.clone())
            .or_else(|| name_fallback.map(str::to_owned))
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

fn load_package_json_manifest(
    worker_dir: &Path,
    name_fallback: Option<&str>,
) -> Result<WorkerManifest, CoreError> {
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

    let mut manifest = default_manifest(
        worker_dir,
        package.name.or_else(|| name_fallback.map(str::to_owned)),
        package.version,
    );
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
