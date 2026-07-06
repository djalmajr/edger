//! Worker deploy: zip install + disk/index rescan (Epic 14, stories 14.01/14.02).

use std::collections::BTreeSet;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use edger_core::{
    create_worker_ref, principal_can_access_optional_namespace, ApiKeyPrincipal, CoreError,
    WorkerManifest,
};
use edger_worker::{WorkerError, WorkerPool};
use serde::Serialize;

use crate::manifest_index_stub::ManifestIndex;
use crate::manifest_loader::{load_worker_manifest, scan_worker_manifests};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledWorker {
    pub name: String,
    pub version: String,
    pub url: String,
    pub kind: String,
    pub source: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RescanReport {
    pub dry_run: bool,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
}

/// Extract a zip package into the first worker root, validate it with the
/// same rules as boot loading, move it atomically into place and index it.
pub fn install_worker_from_zip(
    index: &ManifestIndex,
    principal: &ApiKeyPrincipal,
    bytes: &[u8],
) -> Result<InstalledWorker, CoreError> {
    let root = install_root(index)?;
    let staging = tempfile::Builder::new()
        .prefix(".edger-install-")
        .tempdir_in(&root)
        .map_err(|err| deploy_io(format!("failed to create staging dir: {err}")))?;

    extract_zip(bytes, staging.path())?;
    let package_dir = package_root(staging.path())?;

    let manifest = load_worker_manifest(&package_dir)?;
    validate_package_manifest(&package_dir, &manifest)?;
    if manifest.enabled == Some(false) {
        return Err(CoreError::new(
            "DEPLOY_INVALID_PACKAGE",
            "package manifest is disabled (enabled: false)",
        ));
    }

    // Resolve canonical identity before committing anything.
    let worker = create_worker_ref(package_dir.clone(), manifest.clone())?;
    if !principal_can_access_optional_namespace(principal, worker.namespace.as_deref()) {
        return Err(CoreError::new(
            "FORBIDDEN",
            format!(
                "principal cannot install into namespace {}",
                worker.namespace.as_deref().unwrap_or("(root)")
            ),
        ));
    }

    let target = target_dir(&root, &worker.name, &worker.version)?;
    fs::rename(&package_dir, &target)
        .map_err(|err| deploy_io(format!("failed to move package into place: {err}")))?;

    let mut index = index.clone();
    if let Err(err) = index.insert(target.clone(), manifest) {
        let _ = fs::remove_dir_all(&target);
        return Err(err);
    }

    Ok(InstalledWorker {
        url: format!("/{}", worker.name),
        kind: kind_label(&worker.kind),
        name: worker.name,
        version: worker.version,
        source: target.display().to_string(),
    })
}

/// Diff workers on disk against the index; optionally apply the difference.
/// The disk is the source of truth; untouched entries keep their runtime
/// enable/disable overlay.
pub fn rescan_workers(index: &ManifestIndex, dry_run: bool) -> Result<RescanReport, CoreError> {
    let roots = index.roots();
    if roots.is_empty() {
        return Err(CoreError::new(
            "DEPLOY_NO_ROOT",
            "no worker roots configured for rescan",
        ));
    }

    let mut disk = Vec::new();
    let mut disk_keys = BTreeSet::new();
    for (dir, manifest) in scan_worker_manifests(&roots)? {
        let worker = create_worker_ref(dir.clone(), manifest.clone())?;
        let key = format!("{}@{}", worker.name, worker.version);
        disk_keys.insert(key.clone());
        disk.push((key, dir, manifest));
    }

    let indexed = index
        .admin_workers()
        .into_iter()
        .map(|worker| format!("{}@{}", worker.name, worker.version))
        .collect::<BTreeSet<_>>();

    let added = disk_keys.difference(&indexed).cloned().collect::<Vec<_>>();
    let removed = indexed.difference(&disk_keys).cloned().collect::<Vec<_>>();
    let unchanged = indexed.intersection(&disk_keys).count();

    if !dry_run {
        let mut index = index.clone();
        for (key, dir, manifest) in disk {
            if added.contains(&key) {
                index.insert(dir, manifest)?;
            }
        }
        for key in &removed {
            let (name, version) = key
                .rsplit_once('@')
                .ok_or_else(|| CoreError::new("DEPLOY_INTERNAL", "malformed worker key"))?;
            index.remove_worker(name, version)?;
        }
    }

    Ok(RescanReport {
        dry_run,
        added,
        removed,
        unchanged,
    })
}

pub async fn rescan_workers_and_prewarm(
    index: &ManifestIndex,
    pool: &WorkerPool,
    dry_run: bool,
) -> Result<RescanReport, CoreError> {
    let report = rescan_workers(index, dry_run)?;
    if !dry_run {
        prewarm_min_process_workers(index, pool).await?;
    }
    Ok(report)
}

pub async fn prewarm_min_process_workers(
    index: &ManifestIndex,
    pool: &WorkerPool,
) -> Result<usize, CoreError> {
    let mut spawned = 0;
    for worker in index.worker_refs() {
        if worker.config.enabled
            && worker.config.min_processes > 0
            && worker.kind.uses_process_backend()
        {
            spawned += pool
                .prewarm_worker(&worker)
                .await
                .map_err(|err| prewarm_error(&worker.name, &worker.version, err))?;
        }
    }
    Ok(spawned)
}

fn prewarm_error(name: &str, version: &str, err: WorkerError) -> CoreError {
    CoreError::new(
        "WORKER_PREWARM_FAILED",
        format!("failed to prewarm {name}@{version}: {err}"),
    )
}

fn install_root(index: &ManifestIndex) -> Result<PathBuf, CoreError> {
    let root = index
        .roots()
        .into_iter()
        .next()
        .ok_or_else(|| CoreError::new("DEPLOY_NO_ROOT", "no worker roots configured"))?;
    fs::create_dir_all(&root)
        .map_err(|err| deploy_io(format!("worker root unavailable: {err}")))?;
    root.canonicalize()
        .map_err(|err| deploy_io(format!("worker root unavailable: {err}")))
}

fn extract_zip(bytes: &[u8], destination: &Path) -> Result<(), CoreError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|err| CoreError::new("DEPLOY_INVALID_PACKAGE", format!("invalid zip: {err}")))?;
    if archive.is_empty() {
        return Err(CoreError::new("DEPLOY_INVALID_PACKAGE", "zip is empty"));
    }
    for entry_index in 0..archive.len() {
        let mut entry = archive.by_index(entry_index).map_err(|err| {
            CoreError::new(
                "DEPLOY_INVALID_PACKAGE",
                format!("invalid zip entry: {err}"),
            )
        })?;
        // `enclosed_name` rejects absolute paths and `..` traversal (zip-slip).
        let Some(relative) = entry.enclosed_name() else {
            return Err(CoreError::new(
                "DEPLOY_PATH_DENIED",
                format!("zip entry escapes package root: {}", entry.name()),
            ));
        };
        let target = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|err| deploy_io(format!("failed to create dir: {err}")))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| deploy_io(format!("failed to create dir: {err}")))?;
        }
        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(|err| deploy_io(format!("failed to read zip entry: {err}")))?;
        fs::write(&target, contents)
            .map_err(|err| deploy_io(format!("failed to write file: {err}")))?;
    }
    Ok(())
}

/// Zipping a folder usually nests everything under one top-level directory;
/// unwrap it so the package root is the worker dir itself.
fn package_root(staging: &Path) -> Result<PathBuf, CoreError> {
    let entries = fs::read_dir(staging)
        .map_err(|err| deploy_io(format!("failed to read staging dir: {err}")))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    match entries.as_slice() {
        [single] if single.is_dir() => Ok(single.clone()),
        [] => Err(CoreError::new("DEPLOY_INVALID_PACKAGE", "zip is empty")),
        _ => Ok(staging.to_path_buf()),
    }
}

fn validate_package_manifest(dir: &Path, manifest: &WorkerManifest) -> Result<(), CoreError> {
    // Without an explicit name the loader falls back to the (random) staging
    // dir name — meaningless as an app identity.
    let has_named_manifest = ["manifest.yaml", "manifest.yml", "package.json"]
        .iter()
        .any(|candidate| dir.join(candidate).is_file());
    if !has_named_manifest || manifest.name.trim().is_empty() {
        return Err(CoreError::new(
            "DEPLOY_INVALID_PACKAGE",
            "package must include manifest.yaml (or package.json) with a worker name",
        ));
    }
    if manifest.entrypoint.is_none() && manifest.ssr_entrypoint.is_none() {
        return Err(CoreError::new(
            "DEPLOY_INVALID_PACKAGE",
            "package has no entrypoint (manifest entrypoint, ssrEntrypoint or index.{html,ts,js,mjs,wasm,wat})",
        ));
    }
    Ok(())
}

fn target_dir(root: &Path, name: &str, version: &str) -> Result<PathBuf, CoreError> {
    let base = sanitize_dir_name(name);
    if base.is_empty() {
        return Err(CoreError::new(
            "DEPLOY_INVALID_PACKAGE",
            format!("worker name {name:?} does not map to a valid directory"),
        ));
    }
    let plain = root.join(&base);
    if !plain.exists() {
        return Ok(plain);
    }
    let versioned = root.join(format!("{base}@{version}"));
    if !versioned.exists() {
        return Ok(versioned);
    }
    Err(CoreError::new(
        "DEPLOY_TARGET_EXISTS",
        format!("target directory for {name}@{version} already exists"),
    ))
}

fn sanitize_dir_name(name: &str) -> String {
    name.trim_start_matches('@')
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches(['-', '.'])
        .to_string()
}

fn kind_label(kind: &edger_core::ExecutionKind) -> String {
    match kind {
        edger_core::ExecutionKind::FetchHandler => "FetchHandler",
        edger_core::ExecutionKind::RoutesTable => "RoutesTable",
        edger_core::ExecutionKind::StaticSpa { .. } => "StaticSpa",
        edger_core::ExecutionKind::WasmModule { .. } => "WasmModule",
        edger_core::ExecutionKind::Fullstack { .. } => "Fullstack",
    }
    .to_string()
}

fn deploy_io(message: String) -> CoreError {
    CoreError::new("DEPLOY_IO", message)
}
