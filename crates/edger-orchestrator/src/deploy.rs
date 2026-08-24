//! Worker deploy: zip install + disk/index rescan (Epic 14, stories 14.01/14.02).

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use edger_core::{
    create_worker_ref, principal_can_access_optional_namespace, ApiKeyPrincipal, CoreError,
    WorkerConfig, WorkerManifest, WorkerOrigin, WorkerRef, WorkerVisibility,
};
use edger_worker::{WorkerError, WorkerPool};
use serde::Serialize;

use crate::manifest_index_stub::ManifestIndex;
use crate::manifest_loader::{
    clear_persisted_default_version, load_worker_manifest_with_name_fallback,
    reload_persisted_default_versions, scan_worker_manifests,
};
use crate::observability::{
    OperationalEventInput, OperationalEventLevel, OperationalEventSource, OperationalStore,
};

/// Compressed package cap for admin deploy and project upload/download.
/// Framework artifacts such as Next.js standalone routinely exceed the
/// request-path default while still remaining bounded operational payloads.
pub const MAX_DEPLOY_PACKAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_DEPLOY_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DEPLOY_ENTRIES: usize = 50_000;
/// Revision marker persisted INSIDE the worker version directory, so it
/// travels with the atomic swap, survives restart/rescan on the PVC and needs
/// no side registry. It is the compare side of the draft CAS below.
pub(crate) const REVISION_FILE: &str = ".edger-revision";

/// Current revision of an installed worker version, if it was installed by a
/// revision-aware deploy. Missing/empty means the target predates tracking.
pub(crate) fn worker_revision(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join(REVISION_FILE))
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|revision| !revision.is_empty())
}

/// Marca `name@version` como "mutação em andamento" durante a TRANSAÇÃO
/// inteira da operação que mexe no diretório do worker — install
/// (check→swap→release→health→commit/rollback), delete e upload de files.
/// O arquivo de revisão sozinho não é CAS, e um lock só até o swap também
/// não basta: o req A pode trocar o diretório, liberar, e — enquanto roda
/// release/health — um DELETE responder sucesso ou um upload mutar o
/// candidato; o ROLLBACK de A então ressuscitaria/clobberia o resultado.
/// O slot vive na operação (RAII) e cobre todos os desfechos.
#[derive(Debug)]
pub(crate) struct WorkerMutationSlot {
    key: String,
}

static MUTATIONS_IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub(crate) fn claim_worker_mutation_slot(
    root: &Path,
    name: &str,
    version: &str,
) -> Result<WorkerMutationSlot, CoreError> {
    // O root entra na chave (stores diferentes não competem) e é
    // CANONICALIZADO: quem chama ora traz o caminho cru do install_root, ora
    // o canônico do resolve — em macOS /var e /private/var são o mesmo lugar
    // e chaves distintas quebrariam a exclusão mútua.
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let key = format!("{}|{name}@{version}", root.display());
    let mut in_flight = MUTATIONS_IN_FLIGHT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !in_flight.insert(key.clone()) {
        return Err(CoreError::new(
            "DEPLOY_IN_PROGRESS",
            format!("another mutation of {name}@{version} is in flight; retry after it settles"),
        ));
    }
    Ok(WorkerMutationSlot { key })
}

impl Drop for WorkerMutationSlot {
    fn drop(&mut self) {
        MUTATIONS_IN_FLIGHT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.key);
    }
}

/// Avança a revisão CAS de um diretório de worker já instalado (upload de
/// files): sem o bump, um force com a revisão antiga passaria por cima da
/// edição sem conflito. Escrita temp+rename, como o resto da persistência.
pub(crate) fn bump_worker_revision(dir: &Path) -> Result<String, CoreError> {
    let revision = uuid::Uuid::new_v4().to_string();
    let temporary = dir.join(format!(".edger-revision-{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary, format!("{revision}\n"))
        .map_err(|err| deploy_io(format!("failed to stage revision bump: {err}")))?;
    fs::rename(&temporary, dir.join(REVISION_FILE)).map_err(|err| {
        let _ = fs::remove_file(&temporary);
        deploy_io(format!("failed to publish revision bump: {err}"))
    })?;
    Ok(revision)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledWorker {
    pub name: String,
    pub version: String,
    pub visibility: WorkerVisibility,
    pub url: String,
    pub kind: String,
    pub source: String,
    pub origin: WorkerOrigin,
    pub release: String,
    pub health: String,
    pub activation: String,
    pub default_version: String,
    pub revision: String,
}

#[derive(Debug)]
pub struct InstallTransaction {
    pub installed: InstalledWorker,
    replacement: Option<ReplacementBackup>,
    /// Mantém o name@version reservado até commit/rollback (RAII).
    _slot: WorkerMutationSlot,
}

#[derive(Debug)]
struct ReplacementBackup {
    path: PathBuf,
    manifest: WorkerManifest,
    origin: WorkerOrigin,
}

impl InstallTransaction {
    pub fn replaced_existing(&self) -> bool {
        self.replacement.is_some()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedWorker {
    pub name: String,
    pub versions: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RescanReport {
    pub dry_run: bool,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
}

/// Extract a zip package into the configured worker root, validate it with the
/// same rules as boot loading, then atomically install or replace it.
pub fn install_worker_from_zip(
    index: &ManifestIndex,
    principal: &ApiKeyPrincipal,
    bytes: &[u8],
    package_name_hint: Option<&str>,
    force: bool,
    expected_revision: Option<&str>,
) -> Result<InstallTransaction, CoreError> {
    let package_name_hint = package_name_hint.and_then(package_name_from_hint);
    let inspection = tempfile::Builder::new()
        .prefix(".edger-install-")
        .tempdir()
        .map_err(|err| deploy_io(format!("failed to create staging dir: {err}")))?;

    extract_zip(bytes, inspection.path())?;
    let inspected_package_dir = package_root(inspection.path())?;
    let inspected_manifest = load_worker_manifest_with_name_fallback(
        &inspected_package_dir,
        package_name_hint.as_deref(),
    )?;
    let inspected_worker = create_worker_ref(inspected_package_dir, inspected_manifest)?;
    let core = is_core_name(&inspected_worker.name);
    let root = install_root(index, core)?;
    let staging = tempfile::Builder::new()
        .prefix(".edger-install-")
        .tempdir_in(&root)
        .map_err(|err| deploy_io(format!("failed to create staging dir: {err}")))?;
    extract_zip(bytes, staging.path())?;
    let package_dir = package_root(staging.path())?;

    let mut manifest =
        load_worker_manifest_with_name_fallback(&package_dir, package_name_hint.as_deref())?;
    validate_package_manifest(
        &manifest,
        package_dir != staging.path()
            || package_name_hint.is_some()
            || package_declares_name(&package_dir)?,
    )?;
    if manifest.enabled == Some(false) {
        return Err(CoreError::new(
            "DEPLOY_INVALID_PACKAGE",
            "package manifest is disabled (enabled: false)",
        ));
    }

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

    // Grava a revisão nova DENTRO do pacote antes do swap: ela viaja junto na
    // troca atômica e passa a ser a verdade da versão instalada.
    let revision = uuid::Uuid::new_v4().to_string();
    fs::write(package_dir.join(REVISION_FILE), format!("{revision}\n"))
        .map_err(|err| deploy_io(format!("failed to stamp package revision: {err}")))?;

    // Daqui até o swap/index é seção crítica por name@version: a comparação de
    // revisão só vale se nenhum outro install do mesmo alvo entrelaçar — e a
    // reserva precisa durar até o commit/rollback lá no handler (via RAII).
    let slot = claim_worker_mutation_slot(&root, &worker.name, &worker.version)?;

    let existing = index
        .worker_refs()
        .into_iter()
        .find(|candidate| candidate.name == worker.name && candidate.version == worker.version);
    if existing.is_some() && !force {
        return Err(CoreError::new(
            "COLLISION",
            format!("duplicate worker {}@{}", worker.name, worker.version),
        ));
    }
    if force && existing.is_none() {
        return Err(CoreError::new(
            "DEPLOY_FORCE_TARGET_REQUIRED",
            format!(
                "force install requires an existing internal worker {}@{}",
                worker.name, worker.version
            ),
        ));
    }

    let origin = if core {
        WorkerOrigin::CoreOverlay
    } else {
        WorkerOrigin::User
    };
    let mut replacement = None;
    let target = if let Some(existing) = existing {
        let info = index
            .admin_workers()
            .into_iter()
            .find(|candidate| {
                candidate.name == existing.name
                    && candidate.version == existing.version
                    && Path::new(&candidate.source) == existing.dir
            })
            .ok_or_else(|| {
                CoreError::new("DEPLOY_INTERNAL", "replacement worker metadata is missing")
            })?;
        if info.origin == WorkerOrigin::CoreBundled {
            return Err(CoreError::new(
                "CORE_BUNDLED_IMMUTABLE",
                format!(
                    "bundled core worker {}@{} cannot be replaced",
                    worker.name, worker.version
                ),
            ));
        }
        if existing.config.visibility != WorkerVisibility::Internal {
            return Err(CoreError::new(
                "DEPLOY_PUBLIC_VERSION_IMMUTABLE",
                format!(
                    "public worker {}@{} is immutable; install a new public version and promote it",
                    worker.name, worker.version
                ),
            ));
        }
        if existing.config.visibility != worker.config.visibility {
            return Err(CoreError::new(
                "DEPLOY_VISIBILITY_IMMUTABLE",
                format!(
                    "worker {}@{} visibility cannot change during force install",
                    worker.name, worker.version
                ),
            ));
        }
        // CAS do canal draft, DEPOIS das imutabilidades: só chega aqui alvo
        // internal legítimo. Sem revisão esperada não é compare-and-swap, e
        // dois autosaves concorrentes publicariam o mais velho por último.
        let Some(expected) = expected_revision else {
            return Err(CoreError::new(
                "DEPLOY_REVISION_REQUIRED",
                format!(
                    "force install is a compare-and-swap: send x-edger-expected-revision; the current revision of {}@{} is {}",
                    worker.name,
                    worker.version,
                    worker_revision(&existing.dir).unwrap_or_else(|| "unknown".into()),
                ),
            ));
        };
        let Some(current_revision) = worker_revision(&existing.dir) else {
            return Err(CoreError::new(
                "DEPLOY_REVISION_UNKNOWN",
                format!(
                    "worker {}@{} predates revision tracking; delete it and install the draft again",
                    worker.name, worker.version
                ),
            ));
        };
        if expected != current_revision {
            return Err(CoreError::new(
                "DEPLOY_REVISION_STALE",
                format!(
                    "stale draft write for {}@{}: expected revision {expected}, current is {current_revision}",
                    worker.name, worker.version
                ),
            ));
        }
        let target = existing.dir.clone();
        if !target.starts_with(&root) {
            return Err(CoreError::new(
                "DEPLOY_TARGET_MISMATCH",
                "replacement target is outside the configured worker root",
            ));
        }
        let mut previous_manifest =
            load_worker_manifest_with_name_fallback(&target, Some(&worker.name))?;
        previous_manifest.enabled = Some(existing.config.enabled);
        let backup = replacement_backup_path(&target)?;
        fs::rename(&target, &backup)
            .map_err(|err| deploy_io(format!("failed to stage current deployment: {err}")))?;
        if let Err(error) = fs::rename(&package_dir, &target) {
            let _ = fs::rename(&backup, &target);
            return Err(deploy_io(format!(
                "failed to swap replacement package into place: {error}"
            )));
        }
        manifest.enabled = Some(false);
        if let Err(error) = index.replace_with_origin(target.clone(), manifest.clone(), info.origin)
        {
            let _ = fs::remove_dir_all(&target);
            let _ = fs::rename(&backup, &target);
            return Err(error);
        }
        replacement = Some(ReplacementBackup {
            path: backup,
            manifest: previous_manifest,
            origin: info.origin,
        });
        target
    } else {
        let target = target_dir(&root, &worker.name, &worker.version)?;
        fs::rename(&package_dir, &target)
            .map_err(|err| deploy_io(format!("failed to move package into place: {err}")))?;
        manifest.enabled = Some(false);
        let mut mutable_index = index.clone();
        if let Err(error) =
            mutable_index.insert_with_origin(target.clone(), manifest.clone(), origin)
        {
            let _ = fs::remove_dir_all(&target);
            return Err(error);
        }
        target
    };

    Ok(InstallTransaction {
        installed: InstalledWorker {
            url: format!("/{}", worker.name),
            kind: kind_label(&worker.kind),
            visibility: worker.config.visibility,
            name: worker.name,
            version: worker.version,
            source: target.display().to_string(),
            origin,
            release: "pending".into(),
            health: "pending".into(),
            activation: "indexed".into(),
            default_version: String::new(),
            revision,
        },
        replacement,
        _slot: slot,
    })
}

pub fn commit_install(transaction: InstallTransaction) -> Result<InstalledWorker, CoreError> {
    if let Some(replacement) = &transaction.replacement {
        fs::remove_dir_all(&replacement.path).map_err(|error| {
            deploy_io(format!(
                "failed to remove replacement backup {}: {error}",
                replacement.path.display()
            ))
        })?;
    }
    Ok(transaction.installed)
}

pub fn rollback_install(
    index: &ManifestIndex,
    transaction: &InstallTransaction,
) -> Result<(), CoreError> {
    let target = PathBuf::from(&transaction.installed.source);
    let Some(replacement) = &transaction.replacement else {
        index.remove_worker(&transaction.installed.name, &transaction.installed.version)?;
        fs::remove_dir_all(&target).map_err(|error| {
            CoreError::new(
                "DEPLOY_ROLLBACK_FAILED",
                format!(
                    "failed to remove rejected candidate {}@{} from {}: {error}",
                    transaction.installed.name,
                    transaction.installed.version,
                    target.display()
                ),
            )
        })?;
        return Ok(());
    };

    let rejected = replacement_backup_path(&target)?;
    fs::rename(&target, &rejected).map_err(|error| {
        deploy_io(format!(
            "failed to stage rejected replacement {}: {error}",
            target.display()
        ))
    })?;
    if let Err(error) = fs::rename(&replacement.path, &target) {
        let _ = fs::rename(&rejected, &target);
        return Err(deploy_io(format!(
            "failed to restore previous deployment {}: {error}",
            target.display()
        )));
    }
    if let Err(error) = index.replace_with_origin(
        target.clone(),
        replacement.manifest.clone(),
        replacement.origin,
    ) {
        let _ = fs::rename(&target, &replacement.path);
        let _ = fs::rename(&rejected, &target);
        return Err(error);
    }
    fs::remove_dir_all(&rejected).map_err(|error| {
        deploy_io(format!(
            "failed to remove rejected replacement {}: {error}",
            rejected.display()
        ))
    })
}

pub fn delete_worker(
    index: &ManifestIndex,
    name: &str,
    version: Option<&str>,
) -> Result<DeletedWorker, CoreError> {
    let mut candidates = index
        .admin_workers()
        .into_iter()
        .filter(|worker| {
            worker.name == name && version.is_none_or(|version| worker.version == version)
        })
        .collect::<Vec<_>>();
    let promoted_version = index.default_version(name);
    if candidates.is_empty() {
        let target = version
            .map(|version| format!("{name}@{version}"))
            .unwrap_or_else(|| name.to_string());
        return Err(CoreError::new(
            "NOT_FOUND",
            format!("worker not found: {target}"),
        ));
    }
    if candidates
        .iter()
        .any(|worker| worker.origin != WorkerOrigin::User)
    {
        return Err(CoreError::new(
            "CORE_WORKER_IMMUTABLE",
            format!("core worker {name} cannot be deleted through the admin API"),
        ));
    }
    candidates.sort_by(|left, right| left.version.cmp(&right.version));

    struct Candidate {
        info: edger_core::AdminWorkerInfo,
        manifest: WorkerManifest,
        source: PathBuf,
        tombstone: PathBuf,
    }

    let mut prepared = Vec::with_capacity(candidates.len());
    for info in candidates {
        let source = PathBuf::from(&info.source);
        let mut manifest = load_worker_manifest_with_name_fallback(&source, Some(&info.name))?;
        manifest.enabled = Some(info.status != "disabled");
        prepared.push(Candidate {
            tombstone: replacement_backup_path(&source)?,
            info,
            manifest,
            source,
        });
    }

    // Delete disputa o MESMO slot do install/files, por candidato e em ordem
    // estável (já ordenado por versão acima): deletar no meio da transação de
    // um force responderia sucesso e o rollback do force ressuscitaria o
    // worker. Os slots vivem até o fim da função (RAII); falha em qualquer
    // claim solta os anteriores.
    let mut _slots = Vec::with_capacity(prepared.len());
    for candidate in &prepared {
        let root = candidate
            .source
            .parent()
            .ok_or_else(|| CoreError::new("DEPLOY_INTERNAL", "worker directory has no parent"))?;
        _slots.push(claim_worker_mutation_slot(
            root,
            &candidate.info.name,
            &candidate.info.version,
        )?);
    }

    for index in 0..prepared.len() {
        if let Err(error) = fs::rename(&prepared[index].source, &prepared[index].tombstone) {
            for candidate in prepared[..index].iter().rev() {
                let _ = fs::rename(&candidate.tombstone, &candidate.source);
            }
            return Err(deploy_io(format!(
                "failed to stage worker deletion {}: {error}",
                prepared[index].source.display()
            )));
        }
    }

    for (removed_count, candidate) in prepared.iter().enumerate() {
        if let Err(error) = index.remove_worker(&candidate.info.name, &candidate.info.version) {
            for candidate in prepared.iter().rev() {
                let _ = fs::rename(&candidate.tombstone, &candidate.source);
            }
            let mut mutable_index = index.clone();
            for removed in &prepared[..removed_count] {
                let _ = mutable_index.insert_with_origin(
                    removed.source.clone(),
                    removed.manifest.clone(),
                    removed.info.origin,
                );
            }
            return Err(error);
        }
    }

    for candidate in &prepared {
        fs::remove_dir_all(&candidate.tombstone).map_err(|error| {
            deploy_io(format!(
                "failed to remove deleted worker files {}: {error}",
                candidate.tombstone.display()
            ))
        })?;
    }
    if let Some(promoted_version) = promoted_version {
        if let Some(candidate) = prepared
            .iter()
            .find(|candidate| candidate.info.version == promoted_version)
        {
            clear_persisted_default_version(index, name, &candidate.source)?;
        }
    }
    Ok(DeletedWorker {
        name: name.to_string(),
        versions: prepared
            .iter()
            .map(|candidate| candidate.info.version.clone())
            .collect(),
    })
}

fn replacement_backup_path(target: &Path) -> Result<PathBuf, CoreError> {
    let parent = target
        .parent()
        .ok_or_else(|| deploy_io("worker target has no parent directory".into()))?;
    let swap_root = parent.join(".edger-swaps");
    fs::create_dir_all(&swap_root)
        .map_err(|error| deploy_io(format!("failed to create swap directory: {error}")))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("worker");
    Ok(swap_root.join(format!("{name}-{}", uuid::Uuid::new_v4())))
}

/// Diff workers on disk against the index; optionally apply the difference.
/// The disk is the source of truth; untouched entries keep their runtime
/// enable/disable overlay.
pub fn rescan_workers(index: &ManifestIndex, dry_run: bool) -> Result<RescanReport, CoreError> {
    let roots = index.all_roots();
    if roots.is_empty() {
        return Err(CoreError::new(
            "DEPLOY_NO_ROOT",
            "no worker roots configured for rescan",
        ));
    }

    let mut disk = Vec::new();
    let mut disk_keys = BTreeSet::new();
    for (root, origin) in roots {
        for (dir, manifest) in scan_worker_manifests(std::slice::from_ref(&root))? {
            let worker = create_worker_ref(dir.clone(), manifest.clone())?;
            let key = format!("{}@{}", worker.name, worker.version);
            disk_keys.insert(key.clone());
            disk.push((key, dir, manifest, origin));
        }
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
        for (key, dir, manifest, origin) in disk {
            if added.contains(&key) {
                index.insert_with_origin(dir, manifest, origin)?;
            }
        }
        for key in &removed {
            let (name, version) = key
                .rsplit_once('@')
                .ok_or_else(|| CoreError::new("DEPLOY_INTERNAL", "malformed worker key"))?;
            index.remove_worker(name, version)?;
        }
        reload_persisted_default_versions(&index);
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
        run_pending_releases(index).await?;
        prewarm_min_process_workers(index, pool).await?;
    }
    Ok(report)
}

pub async fn rescan_workers_and_prewarm_with_events(
    index: &ManifestIndex,
    pool: &WorkerPool,
    events: &OperationalStore,
    dry_run: bool,
) -> Result<RescanReport, CoreError> {
    let report = rescan_workers(index, dry_run)?;
    if !dry_run {
        run_pending_releases_with_events(index, events).await?;
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

/// Release timeout: migrations may be slow, but a hung command must not wedge the
/// deploy forever.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(300);

/// Runs each worker's `release` command once per deployed version, before it
/// serves — the place for migrations (edger owns the WHEN, the app owns the HOW).
/// The versioned worker dir plus a `.edger-release` marker make it idempotent per
/// node; the command itself must stay safe under concurrency (e.g. a pg advisory
/// lock) for multi-node deploys.
pub async fn run_pending_releases(index: &ManifestIndex) -> Result<usize, CoreError> {
    let mut ran = 0;
    for worker in index.worker_refs() {
        if worker.config.enabled && run_release(&worker.dir, &worker.config).await? {
            ran += 1;
        }
    }
    Ok(ran)
}

pub async fn run_pending_releases_with_events(
    index: &ManifestIndex,
    events: &OperationalStore,
) -> Result<usize, CoreError> {
    let mut ran = 0;
    for worker in index.worker_refs() {
        if worker.config.enabled && run_release_for_worker(&worker, events).await? {
            ran += 1;
        }
    }
    Ok(ran)
}

/// Runs the release lifecycle for one exact deployment candidate.
///
/// Unlike [`run_pending_releases_with_events`], this intentionally accepts a
/// disabled candidate: on-deploy health gating keeps a new version
/// unroutable until release/migrations and the health check have succeeded.
pub async fn run_worker_release_with_events(
    worker: &WorkerRef,
    events: &OperationalStore,
) -> Result<bool, CoreError> {
    run_release_for_worker(worker, events).await
}

async fn run_release_for_worker(
    worker: &WorkerRef,
    events: &OperationalStore,
) -> Result<bool, CoreError> {
    let Some(command) = worker.config.release_command.as_deref() else {
        return Ok(false);
    };
    if command.trim().is_empty() {
        return Ok(false);
    }

    record_release_event(
        events,
        worker,
        "release.started",
        OperationalEventLevel::Info,
        Some("started"),
        None,
        None,
    );
    let started = Instant::now();
    match run_release(&worker.dir, &worker.config).await {
        Ok(true) => {
            record_release_event(
                events,
                worker,
                "release.succeeded",
                OperationalEventLevel::Info,
                Some("succeeded"),
                Some(started.elapsed().as_millis() as u64),
                None,
            );
            Ok(true)
        }
        Ok(false) => {
            record_release_event(
                events,
                worker,
                "release.skipped",
                OperationalEventLevel::Info,
                Some("already_released"),
                Some(started.elapsed().as_millis() as u64),
                None,
            );
            Ok(false)
        }
        Err(error) => {
            record_release_event(
                events,
                worker,
                "release.failed",
                OperationalEventLevel::Error,
                Some("failed"),
                Some(started.elapsed().as_millis() as u64),
                Some(error.code.clone()),
            );
            Err(error)
        }
    }
}

fn record_release_event(
    events: &OperationalStore,
    worker: &WorkerRef,
    kind: &str,
    level: OperationalEventLevel,
    outcome: Option<&str>,
    duration_ms: Option<u64>,
    code: Option<String>,
) {
    events.record(OperationalEventInput {
        source: OperationalEventSource::Release,
        kind: kind.into(),
        level,
        namespace: worker.namespace.clone(),
        worker: Some(worker.name.clone()),
        version: Some(worker.version.clone()),
        process_id: None,
        request_id: None,
        trace_id: None,
        outcome: outcome.map(str::to_string),
        status: None,
        duration_ms,
        code,
        message: None,
        truncated: None,
        dropped_count: None,
    });
}

/// Runs `config.release_command` once (guarded by a `.edger-release` marker in
/// `dir`), with the worker's full manifest env (DATABASE_URL etc. — delivered
/// since server workers receive all declared env). The command is
/// operator-declared and runs as a trusted subprocess in `dir`. Returns whether
/// it actually ran. A non-zero exit fails the deploy so a broken migration never
/// reaches serving.
async fn run_release(dir: &Path, config: &WorkerConfig) -> Result<bool, CoreError> {
    let Some(command) = config.release_command.as_deref() else {
        return Ok(false);
    };
    if command.trim().is_empty() {
        return Ok(false);
    }
    let marker = dir.join(".edger-release");
    if marker.exists() {
        return Ok(false); // already released for this version
    }

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(dir)
        .envs(&config.env) // manifest env overlays the inherited toolchain env (PATH/HOME/...)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = match tokio::time::timeout(RELEASE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return Err(CoreError::new(
                "RELEASE_SPAWN_FAILED",
                format!("release command failed to run: {err}"),
            ));
        }
        Err(_) => {
            return Err(CoreError::new(
                "RELEASE_TIMEOUT",
                format!("release command exceeded {}s", RELEASE_TIMEOUT.as_secs()),
            ));
        }
    };
    if !output.status.success() {
        return Err(CoreError::new(
            "RELEASE_FAILED",
            format!(
                "release command exited with {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    fs::write(&marker, "released\n")
        .map_err(|err| deploy_io(format!("failed to write release marker: {err}")))?;
    Ok(true)
}

fn install_root(index: &ManifestIndex, core: bool) -> Result<PathBuf, CoreError> {
    let root = index
        .install_root(core)
        .ok_or_else(|| CoreError::new("DEPLOY_NO_ROOT", "no worker roots configured"))?;
    fs::create_dir_all(&root)
        .map_err(|err| deploy_io(format!("worker root unavailable: {err}")))?;
    root.canonicalize()
        .map_err(|err| deploy_io(format!("worker root unavailable: {err}")))
}

fn is_core_name(name: &str) -> bool {
    matches!(name, "cpanel" | "webide")
}

pub(crate) fn extract_zip(bytes: &[u8], destination: &Path) -> Result<(), CoreError> {
    if bytes.len() > MAX_DEPLOY_PACKAGE_BYTES {
        return Err(CoreError::new(
            "PAYLOAD_TOO_LARGE",
            format!("deploy package exceeds the {MAX_DEPLOY_PACKAGE_BYTES} byte limit"),
        ));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|err| CoreError::new("DEPLOY_INVALID_PACKAGE", format!("invalid zip: {err}")))?;
    if archive.is_empty() {
        return Err(CoreError::new("DEPLOY_INVALID_PACKAGE", "zip is empty"));
    }
    if archive.len() > MAX_DEPLOY_ENTRIES {
        return Err(CoreError::new(
            "DEPLOY_INVALID_PACKAGE",
            "zip contains too many entries",
        ));
    }
    let mut expanded_bytes = 0_u64;
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
        expanded_bytes = expanded_bytes
            .checked_add(entry.size())
            .ok_or_else(|| CoreError::new("DEPLOY_INVALID_PACKAGE", "zip is too large"))?;
        if expanded_bytes > MAX_DEPLOY_EXPANDED_BYTES {
            return Err(CoreError::new(
                "DEPLOY_INVALID_PACKAGE",
                "expanded zip exceeds the safety limit",
            ));
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

fn validate_package_manifest(
    manifest: &WorkerManifest,
    has_stable_name: bool,
) -> Result<(), CoreError> {
    if !has_stable_name || manifest.name.trim().is_empty() {
        return Err(CoreError::new(
            "DEPLOY_INVALID_PACKAGE",
            "package name could not be inferred; include a name in manifest.yaml or package.json, wrap the files in a named folder, or send x-edger-package-name",
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

fn package_declares_name(dir: &Path) -> Result<bool, CoreError> {
    for candidate in ["manifest.yaml", "manifest.yml"] {
        let path = dir.join(candidate);
        if path.is_file() {
            let text = fs::read_to_string(&path)
                .map_err(|err| deploy_io(format!("failed to read {}: {err}", path.display())))?;
            let value: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|err| {
                CoreError::parse(format!("failed to parse {}: {err}", path.display()))
            })?;
            if value
                .get("name")
                .and_then(serde_yaml::Value::as_str)
                .is_some_and(|name| !name.trim().is_empty())
            {
                return Ok(true);
            }
        }
    }

    let path = dir.join("package.json");
    if path.is_file() {
        let text = fs::read_to_string(&path)
            .map_err(|err| deploy_io(format!("failed to read {}: {err}", path.display())))?;
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|err| {
            CoreError::parse(format!("failed to parse {}: {err}", path.display()))
        })?;
        return Ok(value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| !name.trim().is_empty()));
    }

    Ok(false)
}

fn package_name_from_hint(hint: &str) -> Option<String> {
    let stem = Path::new(hint).file_stem()?.to_str()?;
    let name = sanitize_dir_name(stem);
    (!name.is_empty()).then_some(name)
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

#[cfg(test)]
mod release_tests {
    use super::*;
    use edger_core::parse_worker_config;

    #[tokio::test]
    async fn failed_release_emits_sanitized_lifecycle_without_marker() {
        let dir = tempfile::tempdir().unwrap();
        let worker = create_worker_ref(
            dir.path().to_path_buf(),
            WorkerManifest {
                name: "bad-observed".into(),
                version: Some("2.0.0".into()),
                release: Some("echo secret-value-and-path-$PWD >&2; exit 3".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let events = crate::observability::OperationalStore::default();

        let error = run_release_for_worker(&worker, &events).await.unwrap_err();
        assert_eq!(error.code, "RELEASE_FAILED");
        assert!(!dir.path().join(".edger-release").exists());

        let page = events.query(crate::observability::OperationalEventQuery {
            worker: Some("bad-observed".into()),
            version: Some("2.0.0".into()),
            source: Some("release".into()),
            ..Default::default()
        });
        assert_eq!(page.events.len(), 2);
        assert_eq!(page.events[0].kind, "release.failed");
        assert_eq!(page.events[0].code.as_deref(), Some("RELEASE_FAILED"));
        assert_eq!(page.events[1].kind, "release.started");
        let serialized = serde_json::to_string(&page).unwrap();
        assert!(!serialized.contains("secret-value"));
        assert!(!serialized.contains(dir.path().to_string_lossy().as_ref()));
        assert!(!serialized.contains("echo "));
    }

    #[tokio::test]
    async fn successful_release_then_skip_are_observable_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let worker = create_worker_ref(
            dir.path().to_path_buf(),
            WorkerManifest {
                name: "observed".into(),
                version: Some("1.0.0".into()),
                release: Some("true".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let events = crate::observability::OperationalStore::default();

        assert!(run_release_for_worker(&worker, &events).await.unwrap());
        assert!(!run_release_for_worker(&worker, &events).await.unwrap());
        assert!(dir.path().join(".edger-release").exists());

        let page = events.query(crate::observability::OperationalEventQuery {
            worker: Some("observed".into()),
            source: Some("release".into()),
            ..Default::default()
        });
        let kinds = page
            .events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "release.skipped",
                "release.started",
                "release.succeeded",
                "release.started"
            ]
        );
        assert!(page
            .events
            .iter()
            .filter(|event| event.kind != "release.started")
            .all(|event| event.duration_ms.is_some()));
    }

    #[tokio::test]
    async fn run_release_runs_once_and_marks_done() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = WorkerManifest {
            name: "rel".into(),
            release: Some("echo hi > out.txt".into()),
            ..Default::default()
        };
        let config = parse_worker_config(&manifest);

        // First run executes the command and writes the marker.
        assert!(run_release(dir.path(), &config).await.unwrap());
        assert!(dir.path().join("out.txt").exists());
        assert!(dir.path().join(".edger-release").exists());

        // Second run is a no-op (marker present) — once per version.
        assert!(!run_release(dir.path(), &config).await.unwrap());
    }

    #[tokio::test]
    async fn run_release_without_command_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let config = parse_worker_config(&WorkerManifest {
            name: "no-rel".into(),
            ..Default::default()
        });
        assert!(!run_release(dir.path(), &config).await.unwrap());
    }

    #[tokio::test]
    async fn run_release_propagates_failure_without_marker() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = WorkerManifest {
            name: "bad".into(),
            release: Some("exit 3".into()),
            ..Default::default()
        };
        let config = parse_worker_config(&manifest);
        assert!(run_release(dir.path(), &config).await.is_err());
        // No marker on failure — the deploy must not be considered released.
        assert!(!dir.path().join(".edger-release").exists());
    }
}
