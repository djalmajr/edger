//! Filesystem manifest discovery for worker directories (story 07.01).

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};

use edger_core::{AdminWorkerInfo, CoreError, WorkerManifest, WorkerOrigin, WorkerVisibility};
use serde::{Deserialize, Serialize};

use crate::deploy::{claim_worker_mutation_slot, clear_worker_staged, WorkerMutationSlot};
use crate::manifest_index_stub::ManifestIndex;
use crate::routing_policy::{parse_routing_policy, RoutingPolicy, RoutingPolicyTable};

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
            if origin == WorkerOrigin::CoreOverlay {
                // D8: bundled e overlay com a mesma `name@version` não mais
                // derrubam o boot — o bundled vence e a entrada do overlay é
                // ignorada com um aviso no log.
                let worker = edger_core::create_worker_ref(worker_dir.clone(), manifest.clone())?;
                let already_bundled = index.admin_workers().into_iter().any(|existing| {
                    existing.name == worker.name
                        && existing.version == worker.version
                        && existing.origin == WorkerOrigin::CoreBundled
                });
                if already_bundled {
                    tracing::warn!(
                        worker = %worker.name,
                        version = %worker.version,
                        dir = %worker_dir.display(),
                        "overlay core worker has the same version as the bundled one; the bundled version wins"
                    );
                    continue;
                }
            }
            index.insert_with_origin(worker_dir, manifest, origin)?;
        }
    }

    index.set_root_config(
        core_bundled_roots.to_vec(),
        core_overlay_root.cloned(),
        user_roots.to_vec(),
    );
    reload_persisted_default_versions(&index);
    reload_persisted_routing_policies(&index)?;
    Ok(index)
}

const DEFAULT_VERSIONS_DIR: &str = ".edger-defaults";
const ROUTING_POLICIES_DIR: &str = ".edger-routing";

/// Serializa PUT/DELETE com a troca de tabela do rescan. O rescan pode ter
/// lido o disco antes do diff de workers; a releitura que vale é a que
/// acontece com este guard, senão um PUT no meio perde a política nova.
static ROUTING_POLICY_IO: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn routing_policy_io_guard() -> Result<MutexGuard<'static, ()>, CoreError> {
    ROUTING_POLICY_IO.lock().map_err(|_| {
        CoreError::new(
            "LOCK_ERROR",
            "routing policy lock poisoned; the allowlist was left unchanged",
        )
    })
}

#[cfg(test)]
thread_local! {
    static RESCAN_BEFORE_POLICY_RESTORE: std::cell::Cell<Option<fn(&ManifestIndex)>> =
        std::cell::Cell::new(None);
}

#[cfg(test)]
pub(crate) fn set_rescan_before_policy_restore_for_test(hook: fn(&ManifestIndex)) {
    RESCAN_BEFORE_POLICY_RESTORE.with(|cell| cell.set(Some(hook)));
}

#[cfg(test)]
pub(crate) fn run_rescan_before_policy_restore_for_test(index: &ManifestIndex) {
    if let Some(hook) = RESCAN_BEFORE_POLICY_RESTORE.with(|cell| cell.take()) {
        hook(index);
    }
}

#[cfg(test)]
thread_local! {
    static CLEAR_BEFORE_POLICY_REMOVE: std::cell::Cell<Option<fn(&ManifestIndex)>> =
        std::cell::Cell::new(None);
}

#[cfg(test)]
pub(crate) fn set_clear_before_remove_for_test(hook: fn(&ManifestIndex)) {
    CLEAR_BEFORE_POLICY_REMOVE.with(|cell| cell.set(Some(hook)));
}

#[cfg(test)]
fn run_clear_before_remove_for_test(index: &ManifestIndex) {
    if let Some(hook) = CLEAR_BEFORE_POLICY_REMOVE.with(|cell| cell.take()) {
        hook(index);
    }
}

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
    // Segura o slot da versão (D36) durante a escrita do ponteiro e do
    // marcador `staged`: o export de estado não pode capturar o
    // `.edger-defaults/` e a versão no meio do promote. O install já entra
    // com o slot; este aqui cobre o promote HTTP/MCP que roda sozinho.
    let _slot = claim_worker_mutation_slot(
        source.parent().unwrap_or_else(|| Path::new("")),
        name,
        version,
    )?;
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
    clear_worker_staged(&source)?;
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
            if let Err(error) = index.restore_promoted_worker(&pointer.name, &pointer.version) {
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
    Ok(operational_root_for_source(index, source)?
        .join(DEFAULT_VERSIONS_DIR)
        .join(pointer_file_name(name)))
}

fn operational_root_for_source(index: &ManifestIndex, source: &Path) -> Result<PathBuf, CoreError> {
    let roots = index.all_roots();
    let (root, origin) = match roots
        .iter()
        .filter(|(root, _)| source == root || source.starts_with(root))
        .max_by_key(|(root, _)| root.components().count())
        .cloned()
    {
        Some((root, origin)) => (Some(root), Some(origin)),
        None => (None, None),
    };
    // D17: a raiz bundled é somente leitura na imagem; o ponteiro de default
    // de uma versão que só existe no bundled vai para a raiz de overlay, que
    // é gravável.
    let root = match (root, origin) {
        (Some(bundled), Some(WorkerOrigin::CoreBundled)) => Some(
            roots
                .iter()
                .find(|(_, origin)| *origin == WorkerOrigin::CoreOverlay)
                .map(|(root, _)| root.clone())
                .unwrap_or(bundled),
        ),
        (root, _) => root,
    };
    root.and_then(|root| pointer_root_for_configured_root(&root))
        .or_else(|| source.parent().map(Path::to_path_buf))
        .ok_or_else(|| {
            CoreError::new(
                "DEPLOY_IO",
                format!("cannot locate worker root for {}", source.display()),
            )
        })
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

/// Grava a política e só então troca o snapshot. `policy` precisa ter saído
/// de [`parse_routing_policy`]: este caminho confere os invariantes de novo,
/// mas não vê chaves JSON duplicadas que o serde já teria colapsado.
///
/// O slot de mutação de cada versão indexada do nome cobre a janela da
/// escrita: um deploy/delete daquelas versões recebe `DEPLOY_IN_PROGRESS`,
/// e um export de estado em curso recebe `STATE_EXPORT_IN_PROGRESS`. O slot
/// não cobre o diff de workers do rescan. PUT, DELETE e a troca de tabela
/// do rescan compartilham `ROUTING_POLICY_IO`: o rescan relê o disco com
/// esse guard imediatamente antes de aplicar, então um PUT no meio do diff
/// não é coberto por um snapshot antigo. Arquivo inválido não apaga a
/// allowlist. Um `.tmp` deixado por crash continua fora do filtro de export.
pub fn persist_routing_policy(
    index: &ManifestIndex,
    policy: &RoutingPolicy,
) -> Result<(), CoreError> {
    index.validate_routing_policy(policy)?;
    let _slots = claim_policy_slots(index, &policy.name, true)?;
    index.validate_routing_policy(policy)?;
    let _policy_io = routing_policy_io_guard()?;
    let directory = routing_policy_directory(index, &policy.name)?;
    let file_name = pointer_file_name(&policy.name);
    let path = directory.join(&file_name);
    let previous = read_policy_file(&path)?;
    let mut bytes = serde_json::to_vec(policy).map_err(|error| {
        CoreError::new(
            "DEPLOY_IO",
            format!("failed to encode routing policy: {error}"),
        )
    })?;
    bytes.push(b'\n');
    publish_atomic(&directory, &file_name, &bytes)?;
    if let Err(error) = index.apply_routing_policy(policy.clone()) {
        if let Err(restore_error) = restore_policy_file(&directory, &file_name, previous.as_deref())
        {
            return Err(CoreError::new(
                "DEPLOY_IO",
                format!(
                    "routing policy update failed ({error}) and restoring the previous policy failed ({restore_error})"
                ),
            ));
        }
        return Err(error);
    }
    Ok(())
}

/// Apaga os arquivos da política e só então tira o snapshot da memória.
/// Ausência do arquivo é sucesso. Conferência de todo caminho antes da
/// primeira remoção; se uma raiz falha depois, os arquivos já removidos
/// voltam e a allowlist permanece. O mesmo slot da persistência cobre as
/// versões ainda indexadas.
pub fn clear_persisted_routing_policy(index: &ManifestIndex, name: &str) -> Result<(), CoreError> {
    let _slots = claim_policy_slots(index, name, false)?;
    let _policy_io = routing_policy_io_guard()?;
    let mut existing = Vec::new();
    for path in routing_policy_files(index, name)? {
        if let Some(bytes) = read_policy_file(&path)? {
            existing.push((path, bytes));
        }
    }
    let mut removed = Vec::new();
    for (path, bytes) in &existing {
        #[cfg(test)]
        run_clear_before_remove_for_test(index);
        // Revalida o diretorio imediatamente antes de remover: ele pode ter
        // virado symlink entre a leitura e a remocao. Falha aqui compensa
        // como falha de remocao: restaura o que ja saiu.
        if let Err(error) = ensure_policy_file_parent_not_symlink(path) {
            return Err(rollback_policy_deletes(&removed, error));
        }
        if let Err(error) = fs::remove_file(path) {
            return Err(rollback_policy_deletes(
                &removed,
                CoreError::new(
                    "DEPLOY_IO",
                    format!(
                        "failed to remove routing policy {}: {error}",
                        path.display()
                    ),
                ),
            ));
        }
        if let Some(directory) = path.parent() {
            sync_directory(directory);
        }
        removed.push((path.clone(), bytes.clone()));
    }
    if let Err(error) = index.clear_routing_policy(name) {
        return Err(rollback_policy_deletes(&removed, error));
    }
    Ok(())
}

fn rollback_policy_deletes(removed: &[(PathBuf, Vec<u8>)], error: CoreError) -> CoreError {
    for (path, bytes) in removed.iter().rev() {
        let (Some(directory), Some(file_name)) = (
            path.parent(),
            path.file_name().and_then(|name| name.to_str()),
        ) else {
            return CoreError::new(
                "DEPLOY_IO",
                format!(
                    "routing policy delete failed ({error}) and {} has no parent directory to restore",
                    path.display()
                ),
            );
        };
        if let Err(restore_error) = publish_atomic(directory, file_name, bytes) {
            return CoreError::new(
                "DEPLOY_IO",
                format!(
                    "routing policy delete failed ({error}) and restoring {} failed ({restore_error})",
                    path.display()
                ),
            );
        }
    }
    error
}

fn reload_persisted_routing_policies(index: &ManifestIndex) -> Result<(), CoreError> {
    let policies = read_persisted_routing_policies(index)?;
    index.restore_routing_policy_table(policies)
}

/// Relê e troca a tabela com o mesmo guard de PUT/DELETE. Falha de leitura
/// não chama `restore_routing_policy_table`, então a allowlist fica.
pub(crate) fn reload_routing_policies_for_rescan(index: &ManifestIndex) -> Result<(), CoreError> {
    let _policy_io = routing_policy_io_guard()?;
    let policies = read_persisted_routing_policies(index)?;
    index.restore_routing_policy_table(policies)
}

pub(crate) fn read_persisted_routing_policies(
    index: &ManifestIndex,
) -> Result<RoutingPolicyTable, CoreError> {
    let mut directories = configured_operational_roots(index)
        .into_iter()
        .map(|root| root.join(ROUTING_POLICIES_DIR))
        .collect::<Vec<_>>();
    directories.sort();
    directories.dedup();
    let mut seen = HashSet::new();
    let mut policies = RoutingPolicyTable::default();
    for directory in directories {
        load_routing_directory(&directory, &mut policies, &mut seen)?;
    }
    Ok(policies)
}

fn load_routing_directory(
    directory: &Path,
    policies: &mut RoutingPolicyTable,
    seen: &mut HashSet<String>,
) -> Result<(), CoreError> {
    let meta = match fs::symlink_metadata(directory) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(CoreError::new(
                "DEPLOY_IO",
                format!(
                    "failed to read routing policy directory {}: {error}",
                    directory.display()
                ),
            ))
        }
    };
    if meta.file_type().is_symlink() {
        return Err(CoreError::validation(
            "routingPolicy",
            format!("routing policy path is a symlink: {}", directory.display()),
        ));
    }
    if !meta.is_dir() {
        return Err(CoreError::validation(
            "routingPolicy",
            format!(
                "routing policy path is not a directory: {}",
                directory.display()
            ),
        ));
    }
    let entries = fs::read_dir(directory).map_err(|error| {
        CoreError::new(
            "DEPLOY_IO",
            format!(
                "failed to read routing policy directory {}: {error}",
                directory.display()
            ),
        )
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            CoreError::new(
                "DEPLOY_IO",
                format!(
                    "failed to read routing policy directory {}: {error}",
                    directory.display()
                ),
            )
        })?;
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(CoreError::validation(
                "routingPolicy",
                format!("routing policy file name is not UTF-8: {}", path.display()),
            ));
        };
        if name.ends_with(".tmp") {
            continue;
        }
        let file_meta = fs::symlink_metadata(&path).map_err(|error| {
            CoreError::new(
                "DEPLOY_IO",
                format!("failed to stat routing policy {}: {error}", path.display()),
            )
        })?;
        if file_meta.file_type().is_symlink() {
            return Err(CoreError::validation(
                "routingPolicy",
                format!("routing policy path is a symlink: {}", path.display()),
            ));
        }
        if !file_meta.is_file() {
            return Err(CoreError::validation(
                "routingPolicy",
                format!(
                    "routing policy path is not a regular file: {}",
                    path.display()
                ),
            ));
        }
        if !is_policy_file_name(name) {
            return Err(CoreError::validation(
                "routingPolicy",
                format!("unexpected routing policy file: {}", path.display()),
            ));
        }
        paths.push(path);
    }
    paths.sort();
    for path in paths {
        let bytes = fs::read(&path).map_err(|error| {
            CoreError::new(
                "DEPLOY_IO",
                format!("failed to read routing policy {}: {error}", path.display()),
            )
        })?;
        let policy = parse_routing_policy(&bytes).map_err(|error| {
            CoreError::new(
                &error.code,
                format!("{}: {}", path.display(), error.message),
            )
        })?;
        if path.file_name().and_then(|name| name.to_str())
            != Some(pointer_file_name(&policy.name).as_str())
        {
            return Err(CoreError::validation(
                "routingPolicy",
                format!("{} does not match the policy name", path.display()),
            ));
        }
        if !seen.insert(policy.name.clone()) {
            return Err(CoreError::new(
                "COLLISION",
                format!(
                    "duplicate routing policy for {} at {}",
                    policy.name,
                    path.display()
                ),
            ));
        }
        policies.insert(policy);
    }
    Ok(())
}

fn is_policy_file_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    !stem.is_empty()
        && stem.len() % 2 == 0
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn configured_operational_roots(index: &ManifestIndex) -> Vec<PathBuf> {
    let mut roots = index
        .all_roots()
        .into_iter()
        .filter_map(|(root, _)| pointer_root_for_configured_root(&root))
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

fn routing_policy_directory(index: &ManifestIndex, name: &str) -> Result<PathBuf, CoreError> {
    let workers = index
        .worker_refs()
        .into_iter()
        .filter(|worker| worker.name == name)
        .collect::<Vec<_>>();
    if workers.is_empty() {
        return Err(CoreError::new(
            "NOT_FOUND",
            format!("worker not found: {name}"),
        ));
    }
    let mut roots = Vec::new();
    for worker in &workers {
        roots.push(operational_root_for_source(index, &worker.dir)?);
    }
    roots.sort();
    roots.dedup();
    if roots.len() != 1 {
        return Err(CoreError::validation(
            "routingPolicy",
            format!("routing policy for {name} resolves to multiple operational roots"),
        ));
    }
    Ok(roots.remove(0).join(ROUTING_POLICIES_DIR))
}

fn routing_policy_files(index: &ManifestIndex, name: &str) -> Result<Vec<PathBuf>, CoreError> {
    let file_name = pointer_file_name(name);
    let mut paths = Vec::new();
    if index.worker_refs().iter().any(|worker| worker.name == name) {
        if let Ok(directory) = routing_policy_directory(index, name) {
            paths.push(directory.join(&file_name));
        }
    }
    for root in configured_operational_roots(index) {
        paths.push(root.join(ROUTING_POLICIES_DIR).join(&file_name));
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn claim_policy_slots(
    index: &ManifestIndex,
    name: &str,
    require_worker: bool,
) -> Result<Vec<WorkerMutationSlot>, CoreError> {
    let mut workers = index
        .worker_refs()
        .into_iter()
        .filter(|worker| worker.name == name)
        .collect::<Vec<_>>();
    if workers.is_empty() {
        if require_worker {
            return Err(CoreError::new(
                "NOT_FOUND",
                format!("worker not found: {name}"),
            ));
        }
        return Ok(Vec::new());
    }
    workers.sort_by(|left, right| {
        left.version
            .cmp(&right.version)
            .then_with(|| left.dir.cmp(&right.dir))
    });
    let mut slots = Vec::with_capacity(workers.len());
    for worker in workers {
        let root = worker
            .dir
            .parent()
            .ok_or_else(|| CoreError::new("DEPLOY_INTERNAL", "worker directory has no parent"))?;
        slots.push(claim_worker_mutation_slot(
            root,
            &worker.name,
            &worker.version,
        )?);
    }
    Ok(slots)
}

/// Rejeita o diretorio `.edger-routing` quando ele e um symlink. O boot
/// (`load_routing_directory`) ja barra esse caso, mas o diretorio pode ter
/// sido trocado depois do boot; PUT/DELETE precisam repetir a barreira no
/// momento do uso, senao a escrita/remoção escapa para fora pelo link.
/// Ausencia do diretorio continua valida (sera criado pelo publish).
fn ensure_routing_directory_not_symlink(directory: &Path) -> Result<(), CoreError> {
    match fs::symlink_metadata(directory) {
        Ok(meta) if meta.file_type().is_symlink() => Err(CoreError::validation(
            "routingPolicy",
            format!("routing policy path is a symlink: {}", directory.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(CoreError::new(
            "DEPLOY_IO",
            format!(
                "failed to stat routing policy directory {}: {error}",
                directory.display()
            ),
        )),
    }
}

fn ensure_policy_file_parent_not_symlink(path: &Path) -> Result<(), CoreError> {
    if let Some(directory) = path.parent() {
        ensure_routing_directory_not_symlink(directory)?;
    }
    Ok(())
}

fn read_policy_file(path: &Path) -> Result<Option<Vec<u8>>, CoreError> {
    ensure_policy_file_parent_not_symlink(path)?;
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(CoreError::validation(
            "routingPolicy",
            format!("routing policy path is a symlink: {}", path.display()),
        )),
        Ok(meta) if !meta.is_file() => Err(CoreError::validation(
            "routingPolicy",
            format!(
                "routing policy path is not a regular file: {}",
                path.display()
            ),
        )),
        Ok(_) => fs::read(path).map(Some).map_err(|error| {
            CoreError::new(
                "DEPLOY_IO",
                format!("failed to read routing policy {}: {error}", path.display()),
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CoreError::new(
            "DEPLOY_IO",
            format!("failed to stat routing policy {}: {error}", path.display()),
        )),
    }
}

fn restore_policy_file(
    directory: &Path,
    file_name: &str,
    previous: Option<&[u8]>,
) -> Result<(), CoreError> {
    match previous {
        Some(bytes) => publish_atomic(directory, file_name, bytes),
        None => {
            let path = directory.join(file_name);
            ensure_policy_file_parent_not_symlink(&path)?;
            match fs::remove_file(&path) {
                Ok(()) => {
                    sync_directory(directory);
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(CoreError::new(
                    "DEPLOY_IO",
                    format!(
                        "failed to remove routing policy {}: {error}",
                        path.display()
                    ),
                )),
            }
        }
    }
}

fn publish_atomic(directory: &Path, file_name: &str, bytes: &[u8]) -> Result<(), CoreError> {
    if file_name.contains('/') || file_name.contains('\\') || file_name.contains("..") {
        return Err(CoreError::new(
            "DEPLOY_IO",
            "routing policy file name is not safe",
        ));
    }
    ensure_routing_directory_not_symlink(directory)?;
    fs::create_dir_all(directory).map_err(|error| {
        CoreError::new(
            "DEPLOY_IO",
            format!(
                "failed to create routing policy directory {}: {error}",
                directory.display()
            ),
        )
    })?;
    // O diretorio pode ter virado symlink depois do boot (ou entre a
    // checagem acima e a criacao); `create_dir_all` segue o link sem erro,
    // entao a barreira precisa valer tambem depois de criar.
    ensure_routing_directory_not_symlink(directory)?;
    let temporary = directory.join(format!(".{file_name}-{}.tmp", uuid::Uuid::new_v4()));
    let write_result = (|| -> Result<(), CoreError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                CoreError::new(
                    "DEPLOY_IO",
                    format!(
                        "failed to create routing policy temp file {}: {error}",
                        temporary.display()
                    ),
                )
            })?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                CoreError::new(
                    "DEPLOY_IO",
                    format!(
                        "failed to persist routing policy temp file {}: {error}",
                        temporary.display()
                    ),
                )
            })?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let path = directory.join(file_name);
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(CoreError::new(
            "DEPLOY_IO",
            format!(
                "failed to atomically publish routing policy {}: {error}",
                path.display()
            ),
        ));
    }
    sync_directory(directory);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn static_worker(root: &Path, directory: &str, name: &str, version: &str) {
        let worker = root.join(directory);
        fs::create_dir_all(&worker).unwrap();
        fs::write(
            worker.join("manifest.yaml"),
            format!("name: {name}\nversion: \"{version}\"\nentrypoint: index.html\nkind: static\n"),
        )
        .unwrap();
        fs::write(worker.join("index.html"), name).unwrap();
    }

    #[test]
    fn bundled_core_default_pointer_is_stored_in_the_overlay_root() {
        let bundled = tempfile::tempdir().unwrap();
        let overlay = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        static_worker(bundled.path(), "webide", "webide", "1.0.0");
        static_worker(bundled.path(), "webide@2.0.0", "webide", "2.0.0");

        let index = load_manifests_from_roots(
            &[bundled.path().to_path_buf()],
            Some(&overlay.path().to_path_buf()),
            &[user.path().to_path_buf()],
        )
        .unwrap();

        let promoted = persist_default_version(&index, "webide", "1.0.0").unwrap();
        assert_eq!(promoted.name, "webide");
        assert_eq!(promoted.version, "1.0.0");

        // D17: o ponteiro de uma versão somente-bundled fica na raiz de
        // overlay, que é gravável na imagem.
        let pointer = overlay
            .path()
            .join(DEFAULT_VERSIONS_DIR)
            .join(pointer_file_name("webide"));
        assert!(pointer.is_file());
        assert!(!bundled.path().join(DEFAULT_VERSIONS_DIR).exists());

        // E o boot seguinte revalida o ponteiro contra `default_version_path`
        // e serve a versão promovida.
        let reloaded = load_manifests_from_roots(
            &[bundled.path().to_path_buf()],
            Some(&overlay.path().to_path_buf()),
            &[user.path().to_path_buf()],
        )
        .unwrap();
        assert_eq!(
            reloaded.resolve_worker("webide", None).unwrap().version,
            "1.0.0"
        );
    }

    fn allowlist_policy(name: &str) -> RoutingPolicy {
        parse_routing_policy(
            format!(
                r#"{{"name":"{name}","tenantAccess":{{"mode":"allowlist","tenants":["acme"]}}}}"#
            )
            .as_bytes(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn routing_policy_persist_respects_the_mutation_plane() {
        let root = tempfile::tempdir().unwrap();
        static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
        static_worker(root.path(), "app@2.0.0", "app", "2.0.0");
        let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
        let policy = allowlist_policy("app");

        let slot = claim_worker_mutation_slot(root.path(), "app", "2.0.0").unwrap();
        let busy = persist_routing_policy(&index, &policy).unwrap_err();
        assert_eq!(busy.code, "DEPLOY_IN_PROGRESS", "{busy}");
        assert!(index.routing_policy("app").unwrap().is_none());
        assert!(!root.path().join(ROUTING_POLICIES_DIR).exists());
        drop(slot);

        let guard = crate::deploy::begin_state_export(std::time::Duration::from_secs(2))
            .await
            .unwrap();
        let exporting = persist_routing_policy(&index, &policy).unwrap_err();
        assert_eq!(exporting.code, "STATE_EXPORT_IN_PROGRESS", "{exporting}");
        assert!(index.routing_policy("app").unwrap().is_none());
        drop(guard);

        persist_routing_policy(&index, &policy).unwrap();
        assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));
    }

    #[test]
    fn rescan_reloads_a_persisted_policy_missing_from_memory() {
        let root = tempfile::tempdir().unwrap();
        static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
        let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
        let policy = allowlist_policy("app");
        persist_routing_policy(&index, &policy).unwrap();
        index.clear_routing_policy("app").unwrap();
        assert!(index.routing_policy("app").unwrap().is_none());

        crate::deploy::rescan_workers(&index, false).unwrap();

        assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));
    }

    #[test]
    fn rescan_keeps_a_put_that_lands_after_the_policy_read() {
        let root = tempfile::tempdir().unwrap();
        static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
        let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
        let original = allowlist_policy("app");
        persist_routing_policy(&index, &original).unwrap();
        let updated = parse_routing_policy(
            br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["beta"]}}"#,
        )
        .unwrap();
        POLICY_PUT_DURING_RESCAN.with(|slot| *slot.borrow_mut() = Some(updated.clone()));
        set_rescan_before_policy_restore_for_test(put_policy_during_rescan);

        crate::deploy::rescan_workers(&index, false).unwrap();

        assert_eq!(
            index.routing_policy("app").unwrap().as_ref(),
            Some(&updated)
        );
        let stored = fs::read(
            root.path()
                .join(ROUTING_POLICIES_DIR)
                .join(pointer_file_name("app")),
        )
        .unwrap();
        assert_eq!(parse_routing_policy(&stored).unwrap(), updated);
    }

    #[test]
    fn rescan_keeps_the_allowlist_when_reload_io_fails_after_the_read() {
        let root = tempfile::tempdir().unwrap();
        static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
        let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
        let policy = allowlist_policy("app");
        persist_routing_policy(&index, &policy).unwrap();
        set_rescan_before_policy_restore_for_test(plant_unexpected_policy_file);

        let err = crate::deploy::rescan_workers(&index, false).unwrap_err();

        assert_eq!(err.code, "VALIDATION_ERROR", "{err}");
        assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));
    }

    #[test]
    fn clear_rolls_back_when_a_later_root_becomes_symlink_mid_delete() {
        let base = tempfile::tempdir().unwrap();
        let user = base.path().join("a-user");
        let overlay = base.path().join("z-overlay");
        fs::create_dir_all(&user).unwrap();
        fs::create_dir_all(&overlay).unwrap();
        static_worker(&user, "app@1.0.0", "app", "1.0.0");
        let index = load_manifests_from_roots(&[], Some(&overlay), &[user.clone()]).unwrap();
        let policy = allowlist_policy("app");
        persist_routing_policy(&index, &policy).unwrap();

        let file_name = pointer_file_name("app");
        let user_file = user.join(ROUTING_POLICIES_DIR).join(&file_name);
        let before = fs::read(&user_file).unwrap();
        let overlay_dir = overlay.join(ROUTING_POLICIES_DIR);
        fs::create_dir_all(&overlay_dir).unwrap();
        fs::write(overlay_dir.join(&file_name), &before).unwrap();

        // O hook dispara na primeira iteracao (raiz anterior) e troca a raiz
        // posterior por symlink: a revalidacao dela precisa compensar o
        // arquivo ja removido em vez de retornar direto.
        set_clear_before_remove_for_test(swap_later_routing_root_to_symlink);

        let err = clear_persisted_routing_policy(&index, "app").unwrap_err();

        assert_eq!(err.code, "VALIDATION_ERROR", "{err}");
        assert!(err.message.contains("symlink"), "{err}");
        assert_eq!(fs::read(&user_file).unwrap(), before);
        assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));
        let leftovers: Vec<_> = fs::read_dir(user.join(ROUTING_POLICIES_DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        fs::remove_file(overlay.join(ROUTING_POLICIES_DIR)).unwrap();
        fs::rename(overlay.join(".edger-routing-real"), &overlay_dir).unwrap();
    }
}

#[cfg(test)]
thread_local! {
    static POLICY_PUT_DURING_RESCAN: std::cell::RefCell<Option<RoutingPolicy>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn put_policy_during_rescan(index: &ManifestIndex) {
    let policy = POLICY_PUT_DURING_RESCAN.with(|slot| slot.borrow_mut().take().unwrap());
    persist_routing_policy(index, &policy).unwrap();
}

#[cfg(test)]
fn plant_unexpected_policy_file(index: &ManifestIndex) {
    let (root, _) = index.all_roots().into_iter().next().unwrap();
    let directory = root.join(ROUTING_POLICIES_DIR);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("notes.json"), b"not a policy").unwrap();
}

#[cfg(test)]
fn swap_later_routing_root_to_symlink(index: &ManifestIndex) {
    let mut roots: Vec<PathBuf> = index
        .all_roots()
        .into_iter()
        .map(|(root, _)| root)
        .collect();
    roots.sort();
    let Some(later) = roots.last().cloned() else {
        return;
    };
    let routing = later.join(ROUTING_POLICIES_DIR);
    let backup = later.join(".edger-routing-real");
    if backup.exists() {
        return;
    }
    let Ok(meta) = fs::symlink_metadata(&routing) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    fs::rename(&routing, &backup).unwrap();
    std::os::unix::fs::symlink(roots[0].join(ROUTING_POLICIES_DIR), &routing).unwrap();
}
