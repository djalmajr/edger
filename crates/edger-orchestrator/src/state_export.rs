//! Export de estado online (D36) — `GET /api/admin/state/export`.
//!
//! Monta um ZIP, dentro do plano de mutações (`deploy.rs`):
//! - espera os slots de mutação em curso até um limite (`STATE_BUSY` no
//!   timeout); enquanto o ZIP é montado, mutação nova falha com
//!   `STATE_EXPORT_IN_PROGRESS`;
//! - guarda os roots e produz o arquivo em disco (temp file) na pool de
//!   `spawn_blocking`, para não segurar o runtime async com I/O de arquivo;
//! - o guard é soltado ANTES do arquivo ser streamado: o export nunca atrasa
//!   deploys depois de pronto; ele vive na closure da tarefa de montagem e
//!   cai quando a montagem termina — mesmo com a requisição cancelada no
//!   meio (a `spawn_blocking` segue destacada);
//!
//! Conteúdo do ZIP:
//! - `edger-state.json` — manifesto (formato, versão do EdgeR, data e
//!   caminhos das origens);
//! - `user-roots/<n>/` — conteúdo de cada raiz de usuário, na ordem de
//!   `all_roots()`;
//! - `core-overlay/` — raiz de overlay dos workers de core, quando existe;
//! - `api-keys.db` — cópia consistente do store (`VACUUM INTO`), quando
//!   configurado.
//!
//! Excluídos: o `.edger/` do topo de cada raiz de usuário (o banco e o
//! overlay entram pelas entradas próprias; é o estado interno do EdgeR), o
//! arquivo do banco e os sidecars dele (`-journal`, `-wal`, `-shm`), o
//! `.edger-swaps/`, os `.edger-install-*`, os `.edger-revision-*.tmp`, os
//! `*.tmp` do `.edger-defaults/`, a cache do Deno quando `EDGER_DENO_CACHE_ROOT`
//! aponta para dentro de uma raiz exportada, e symlinks (pulados e contados
//! em `skippedSymlinks`).

use std::collections::BTreeSet;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use edger_core::{CoreError, WorkerOrigin};
use serde::Serialize;

use crate::api_keys::ApiKeyService;
use crate::deploy::begin_state_export;
use crate::manifest_index_stub::ManifestIndex;

/// Limite de espera do export na rota (D36): o `begin_state_export` recebe o
/// limite como parâmetro — curto nos testes de unidade, 30 s aqui.
pub(crate) const STATE_EXPORT_WAIT_LIMIT: Duration = Duration::from_secs(30);

/// Saída montada do export: o ZIP é dono RAII do arquivo da montagem até o
/// handoff — se o resultado for descartado sem ser consumido (requisição
/// cancelada durante `spawn_blocking` ou `File::open`), o `TempPath` remove
/// o arquivo. `into_owned` transfere a posse para o stream
/// (`StateExportBody`), que apaga o ZIP no EOF/abort.
#[derive(Debug)]
pub(crate) struct StateExport {
    /// Onde o ZIP está (usado no `File::open` antes do handoff).
    pub(crate) path: PathBuf,
    pub(crate) filename: String,
    /// Posse enquanto o stream não assumiu. `None` após `into_owned` — a
    /// remoção passa a ser do body.
    owned: Option<tempfile::TempPath>,
}

impl StateExport {
    /// Transfere a posse do ZIP para o stream: a remoção passa a ser
    /// responsabilidade do `StateExportBody` (EOF/abort). Só depois que o
    /// `File::open` passou.
    pub(crate) fn into_owned(mut self) -> (PathBuf, String) {
        if let Some(temp) = self.owned.take() {
            // `keep` libera o RAII do `TempPath` (o arquivo fica no disco;
            // em erro raro a posse — e a limpeza — seguem no erro).
            let _ = temp.keep();
        }
        (self.path, self.filename)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateManifest {
    format: u32,
    edger_version: String,
    created_at: String,
    user_roots: Vec<String>,
    core_worker_overlay_dir: Option<String>,
    api_keys_db: Option<String>,
    skipped_symlinks: u64,
}

/// Inicia o export (aguardando o plano de mutações) e monta o ZIP em
/// `spawn_blocking`. O guard vive na closure da tarefa: cai quando a
/// montagem termina — mesmo se o future da requisição for descartado no
/// meio — e as mutações só voltam a esse ponto. O resultado é dono RAII do
/// ZIP até o stream assumir via `into_owned` (cancelamento remove o
/// arquivo). `temp_dir` sobrepõe o diretório temporário (testes).
pub(crate) async fn export_state(
    index: &ManifestIndex,
    key_service: Option<&Arc<ApiKeyService>>,
    temp_dir: Option<&Path>,
) -> Result<StateExport, CoreError> {
    let guard = begin_state_export(STATE_EXPORT_WAIT_LIMIT).await?;
    let user_roots: Vec<PathBuf> = index
        .all_roots()
        .into_iter()
        .filter(|(_, origin)| matches!(origin, WorkerOrigin::User))
        .map(|(root, _)| root)
        .collect();
    let overlay_root: Option<PathBuf> = index
        .all_roots()
        .into_iter()
        .find(|(_, origin)| matches!(origin, WorkerOrigin::CoreOverlay))
        .map(|(root, _)| root);
    let keys: Option<Arc<ApiKeyService>> = key_service.cloned();
    let temp_dir = temp_dir.map(PathBuf::from);
    let built = tokio::task::spawn_blocking(move || {
        // O guard vive com a tarefa (revisão P2): se o future da requisição
        // for descartado, a montagem segue destacada e o plano só libera
        // quando a closure terminar.
        let _guard = guard;
        build_zip(
            &user_roots,
            overlay_root.as_deref(),
            keys.as_deref(),
            temp_dir.as_deref(),
        )
    })
    .await
    .map_err(|err| {
        CoreError::new(
            "STATE_EXPORT_FAILED",
            format!("state export task failed: {err}"),
        )
    })?;
    built
}

/// Monta o ZIP no disco (roda na pool de threads). `temp_dir` sobrepõe o
/// diretório temporário padrão (usado pelos testes; `None` em produção).
/// Qualquer falha no walk/cópia/finish remove o arquivo temporário. No
/// sucesso, o `TempPath` do resultado mantém a posse até o handoff ao stream.
fn build_zip(
    user_roots: &[PathBuf],
    overlay_root: Option<&Path>,
    keys: Option<&ApiKeyService>,
    temp_dir: Option<&Path>,
) -> Result<StateExport, CoreError> {
    #[cfg(test)]
    test_build_pause();

    let now = Utc::now();
    let default_dir = std::env::temp_dir();
    let dir = temp_dir.unwrap_or(&default_dir);
    let mut zip_file = tempfile::Builder::new()
        .prefix("edger-state-")
        .suffix(".zip")
        .tempfile_in(dir)
        .map_err(|err| {
            CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot create temp zip file: {err}"),
            )
        })?;
    // O `NamedTempFile` vive até a montagem terminar com sucesso: qualquer
    // erro abaixo cai o handle e REMOVE o arquivo (a limpeza em falha não
    // depende do stream da rota). O `keep()` só roda no handoff ao stream.
    let mut writer = zip::ZipWriter::new(&mut zip_file);
    let mut skipped_symlinks: u64 = 0;

    let excluded_paths = excluded_paths(keys);
    for (position, root) in user_roots.iter().enumerate() {
        walk_directory(
            &mut writer,
            root,
            root,
            &format!("user-roots/{position}"),
            true,
            &excluded_paths,
            &mut skipped_symlinks,
        )?;
    }
    if let Some(overlay) = overlay_root {
        walk_directory(
            &mut writer,
            overlay,
            overlay,
            "core-overlay",
            false,
            &excluded_paths,
            &mut skipped_symlinks,
        )?;
    }

    // Cópia consistente do banco (VACUUM INTO) — destino fresco (o VACUUM INTO
    // não sobrepõe arquivo existente), lida pro ZIP e descartada.
    let mut api_keys_db: Option<String> = None;
    if let Some(keys) = keys {
        let scratch = tempfile::tempdir().map_err(|err| {
            CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot create temp dir for db copy: {err}"),
            )
        })?;
        let copy = scratch.path().join("api-keys.db");
        keys.export_db(&copy)?;
        add_file_to_zip(&mut writer, &copy, "api-keys.db")?;
        if let Some(db) = keys.db_path() {
            api_keys_db = Some(absolute_display(&db));
        }
    }

    // O manifesto vai por último: precisa do total de symlinks pulados.
    // Caminhos ABSOLUTOS no manifesto (revisão ponto 9): o restore usa esses
    // valores sem depender do cwd do processo que fez o export.
    let manifest = StateManifest {
        format: 1,
        edger_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        user_roots: user_roots
            .iter()
            .map(|path| absolute_display(path))
            .collect(),
        core_worker_overlay_dir: overlay_root.map(absolute_display),
        api_keys_db,
        skipped_symlinks,
    };
    writer
        .start_file("edger-state.json", zip::write::SimpleFileOptions::default())
        .map_err(|err| {
            CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot archive state manifest: {err}"),
            )
        })?;
    writer
        .write_all(&serde_json::to_vec(&manifest).map_err(|err| {
            CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot encode state manifest: {err}"),
            )
        })?)
        .map_err(|err| {
            CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot archive state manifest: {err}"),
            )
        })?;
    writer.finish().map_err(|err| {
        CoreError::new("STATE_EXPORT_FAILED", format!("cannot finalize zip: {err}"))
    })?;

    // Sucesso: o resultado assume a posse RAII do ZIP (revisão P2, órfão
    // no cancelamento) — o `TempPath` só para de limpar quando o stream
    // assume via `into_owned`, após o `File::open` passar. Se algo acima
    // falhou, o `NamedTempFile` saiu de escopo e removeu o arquivo.
    let zip_path = zip_file.into_temp_path();
    let path = (*zip_path).to_path_buf();
    #[cfg(test)]
    test_build_pause_end();
    Ok(StateExport {
        path,
        filename: format!("edger-state-{}.zip", now.format("%Y%m%dT%H%M%SZ")),
        owned: Some(zip_path),
    })
}

/// Caminhos absolutos excluídos em qualquer ponto da árvore: o arquivo do
/// banco e os sidecars dele, e a cache do Deno quando fica dentro de uma raiz
/// exportada. Canonicalização do lado do banco espelha a do walk (o entry é
/// canonicalizado ao ser visitado).
fn excluded_paths(keys: Option<&ApiKeyService>) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    if let Some(db) = keys.and_then(ApiKeyService::db_path) {
        let db_str = db.to_string_lossy();
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let candidate = PathBuf::from(format!("{db_str}{suffix}"));
            paths.insert(canonical_or_raw(&candidate));
        }
    }
    if let Some(cache_root) = std::env::var_os("EDGER_DENO_CACHE_ROOT").map(PathBuf::from) {
        paths.insert(canonical_or_raw(&cache_root));
    }
    paths
}

fn canonical_or_raw(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Caminho absoluto para o manifesto (revisão ponto 9): `std::path::absolute`
/// não exige que o caminho exista (diferente de `canonicalize`). Se nem o
/// cwd estiver disponível, cai para o caminho como veio.
fn absolute_display(path: &Path) -> String {
    std::path::absolute(path)
        .map(|abs| abs.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

/// O formato clássico do ZIP guarda o tamanho de cada arquivo em 32 bits:
/// arquivo maior ou igual a `u32::MAX` precisa dos extended records ZIP64 do
/// próprio arquivo (o upgrade automático do container central, por contagem
/// de entradas, não cobre isso).
fn file_needs_zip64(size: u64) -> bool {
    size >= u32::MAX as u64
}

fn file_zip_options(size: u64) -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default().large_file(file_needs_zip64(size))
}

/// Percorre `dir` recursivamente, entrando no ZIP sob `prefix`. `user_root`
/// ativa a exclusão do `.edger/` do topo (somente na primeira profundidade).
fn walk_directory<W: Write + Seek>(
    writer: &mut zip::ZipWriter<W>,
    base: &Path,
    dir: &Path,
    prefix: &str,
    user_root: bool,
    excluded: &BTreeSet<PathBuf>,
    skipped: &mut u64,
) -> Result<(), CoreError> {
    if !dir.exists() {
        tracing::warn!(path = %dir.display(), "state export: root does not exist, skipping");
        return Ok(());
    }
    let mut entries = std::fs::read_dir(dir)
        .map_err(|err| {
            CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot read directory {}: {err}", dir.display()),
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| {
            CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot read directory {}: {err}", dir.display()),
            )
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let file_type = entry.file_type().map_err(|err| {
            CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot inspect entry {}: {err}", path.display()),
            )
        })?;
        // Symlink: pular e contar (nada de seguir destino — nem pra fora da raiz).
        if file_type.is_symlink() {
            *skipped += 1;
            continue;
        }
        let name_str = name.to_string_lossy();
        if file_type.is_dir() {
            // Estado interno do EdgeR no topo da raiz de usuário: o banco e o
            // overlay (chart: subPath `.edger/core-overlays`) entram pelas
            // entradas próprias — não podem se duplicar aqui.
            if user_root && dir == base && name_str == ".edger" {
                continue;
            }
            if name_str == ".edger-swaps" || name_str.starts_with(".edger-install-") {
                continue;
            }
            if excluded.contains(&canonical_or_raw(&path)) {
                continue;
            }
            writer
                .add_directory(
                    format!("{prefix}/{name_str}/"),
                    zip::write::SimpleFileOptions::default(),
                )
                .map_err(|err| {
                    CoreError::new(
                        "STATE_EXPORT_FAILED",
                        format!("cannot archive directory {name_str}: {err}"),
                    )
                })?;
            walk_directory(
                writer,
                base,
                &path,
                &format!("{prefix}/{name_str}"),
                false,
                excluded,
                skipped,
            )?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        // Transientes de deploy.
        if name_str.starts_with(".edger-revision-") && name_str.ends_with(".tmp") {
            continue;
        }
        if dir
            .file_name()
            .map(|dir_name| dir_name == ".edger-defaults")
            .unwrap_or(false)
            && name_str.ends_with(".tmp")
        {
            continue;
        }
        if excluded.contains(&canonical_or_raw(&path)) {
            continue;
        }
        add_file_to_zip(writer, &path, &format!("{prefix}/{name_str}"))?;
    }
    Ok(())
}

fn add_file_to_zip<W: Write + Seek>(
    writer: &mut zip::ZipWriter<W>,
    path: &Path,
    zip_name: &str,
) -> Result<(), CoreError> {
    let size = std::fs::metadata(path).map_err(|err| {
        CoreError::new(
            "STATE_EXPORT_FAILED",
            format!("cannot stat {}: {err}", path.display()),
        )
    })?;
    writer
        .start_file(zip_name, file_zip_options(size.len()))
        .map_err(|err| {
            CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot archive {zip_name}: {err}"),
            )
        })?;
    let mut file = std::fs::File::open(path).map_err(|err| {
        CoreError::new(
            "STATE_EXPORT_FAILED",
            format!("cannot read {}: {err}", path.display()),
        )
    })?;
    std::io::copy(&mut file, writer).map_err(|err| {
        CoreError::new(
            "STATE_EXPORT_FAILED",
            format!("cannot archive {zip_name}: {err}"),
        )
    })?;
    Ok(())
}

/// Gancho de teste (revisão P2, ciclo de vida do guard): congela a montagem
/// no início do `build_zip` — com o guard já vivo na closure da tarefa —
/// para o teste descartar o future e observar o plano de mutações com a
/// tarefa ainda em andamento. `started` avisa que a montagem chegou aqui;
/// `release` termina a pausa quando o teste solta.
#[cfg(test)]
static TEST_BUILD_PAUSE: std::sync::Mutex<Option<BuildPause>> = std::sync::Mutex::new(None);

/// Segunda pausa (fim da montagem, com o ZIP criado e ainda dono do
/// resultado) — para o teste de cancelamento observar o ZIP e depois
/// descartar o future.
#[cfg(test)]
static TEST_BUILD_PAUSE_END: std::sync::Mutex<Option<BuildPause>> = std::sync::Mutex::new(None);

/// O gancho é global: um teste só arma a pausa com o build de outrem fora do
/// ar (testes paralelos não consomem o gancho do outro nem travam no meio
/// do `build_zip` com ele armado).
#[cfg(test)]
static TEST_BUILD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
struct BuildPause {
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
fn test_build_pause() {
    if let Some(pause) = TEST_BUILD_PAUSE.lock().unwrap().take() {
        let _ = pause.started.send(());
        let _ = pause.release.recv();
    }
}

#[cfg(test)]
fn test_build_pause_end() {
    if let Some(pause) = TEST_BUILD_PAUSE_END.lock().unwrap().take() {
        let _ = pause.started.send(());
        let _ = pause.release.recv();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    use crate::deploy::claim_worker_mutation_slot;
    use crate::manifest_loader::load_manifests_from_roots;

    /// Revisão P2: o guard vive com a tarefa de montagem. Mesmo com a task
    /// da requisição ABORTADA (cancelamento real — descartar o `JoinHandle`
    /// só desanexaria e não cancelaria), a tarefa bloqueante segue (o
    /// `spawn_blocking` não é cancelável) e o plano continua bloqueando
    /// mutações com `STATE_EXPORT_IN_PROGRESS` até a montagem terminar.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn guard_survives_future_drop_until_build_finishes() {
        let _lock = TEST_BUILD_LOCK.lock().unwrap();

        let root = tempfile::tempdir().unwrap();
        let worker = root.path().join("hook-app@1.0.0");
        std::fs::create_dir_all(&worker).unwrap();
        std::fs::write(
            worker.join("manifest.yaml"),
            "name: hook-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
        )
        .unwrap();
        std::fs::write(
            worker.join("index.ts"),
            "export default () => new Response('ok');",
        )
        .unwrap();
        let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *TEST_BUILD_PAUSE.lock().unwrap() = Some(BuildPause {
            started: started_tx,
            release: release_rx,
        });

        // O export roda em task própria; quando a montagem chega ao hook o
        // teste ABORTA a task e aguarda a conclusão cancelada — como um
        // cliente que cancelou no meio (descartar o `JoinHandle` só
        // desanexaria a task e não cancelaria o future exportador).
        let export = tokio::spawn(async move { export_state(&index, None, None).await });
        match started_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(()) => {}
            Err(_) => {
                *TEST_BUILD_PAUSE.lock().unwrap() = None;
                panic!("a montagem não chegou ao hook de pausa");
            }
        }
        export.abort();
        let err = export.await.unwrap_err();
        assert!(
            err.is_cancelled(),
            "a task deveria terminar cancelada, obtive: {err:?}"
        );

        // A tarefa bloqueante segue pausada no hook (o abort derrubou só o
        // future externo; o `spawn_blocking` não é cancelável): a mutação
        // segue bloqueada com o guard que vive na closure.
        let err = claim_worker_mutation_slot(root.path(), "hook-app", "1.0.0").unwrap_err();
        assert_eq!(err.code, "STATE_EXPORT_IN_PROGRESS");

        // Solta a pausa: a montagem termina e o guard cai com a tarefa.
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match claim_worker_mutation_slot(root.path(), "hook-app", "1.0.0") {
                Ok(slot) => {
                    drop(slot);
                    break;
                }
                Err(err) if err.code == "STATE_EXPORT_IN_PROGRESS" => {
                    assert!(Instant::now() < deadline, "guard não caiu após o build");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(err) => panic!("código inesperado: {}", err.code),
            }
        }
    }

    /// Revisão P2: falha no meio do build remove o temp file — nenhum
    /// `edger-state-*.zip` fica no diretório temporário usado pelo teste.
    #[test]
    fn failure_during_build_removes_temp_zip() {
        let _lock = TEST_BUILD_LOCK.lock().unwrap();

        let temp = tempfile::tempdir().unwrap();
        // Raiz de usuário que é um ARQUIVO: o `read_dir` falha no meio do
        // walk (determinístico, sem depender de permissões/usuário).
        let not_a_dir = temp.path().join("not-a-dir");
        std::fs::write(&not_a_dir, "x").unwrap();

        let err = build_zip(&[not_a_dir], None, None, Some(temp.path())).unwrap_err();
        assert_eq!(err.code, "STATE_EXPORT_FAILED");

        let leftovers: Vec<String> = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !leftovers
                .iter()
                .any(|name| name.starts_with("edger-state-") && name.ends_with(".zip")),
            "temp zip ficou para trás: {leftovers:?}"
        );
    }

    /// Revisão P2: a decisão ZIP64 por arquivo usa o limite de 4 GiB —
    /// testada sem gerar arquivo de 4 GiB.
    #[test]
    fn zip64_decision_uses_u32_max_threshold() {
        assert!(!file_needs_zip64(0));
        assert!(!file_needs_zip64(u32::MAX as u64 - 1));
        assert!(file_needs_zip64(u32::MAX as u64));
        assert!(file_needs_zip64(u32::MAX as u64 + 1));
    }

    /// Revisão ponto 9: caminhos absolutos no manifesto, sem exigir que o
    /// caminho exista.
    #[test]
    fn absolute_display_makes_paths_absolute_without_fs() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("does-not-exist");
        let abs = absolute_display(&absent);
        assert!(
            std::path::Path::new(&abs).is_absolute(),
            "esperado absoluto, obtive {abs:?}"
        );
        let rel = absolute_display(Path::new("relative/path"));
        assert!(
            std::path::Path::new(&rel).is_absolute(),
            "esperado absoluto, obtive {rel:?}"
        );
    }

    /// Revisão P2 (ZIP órfão no cancelamento): CANCELAR a requisição durante
    /// a montagem (abort de verdade) deixa **zero** ZIPs no diretório
    /// temporário controlado — o resultado não consumido (`StateExport`) é
    /// dono RAII do arquivo. A montagem bloqueante não é cancelável e
    /// termina; o resultado que ela devolve, sem owner, é descartado e o
    /// `TempPath` apaga o ZIP.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_export_leaves_zero_zips() {
        let _lock = TEST_BUILD_LOCK.lock().unwrap();

        let root = tempfile::tempdir().unwrap();
        let worker = root.path().join("hook-app@1.0.0");
        std::fs::create_dir_all(&worker).unwrap();
        std::fs::write(
            worker.join("manifest.yaml"),
            "name: hook-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
        )
        .unwrap();
        std::fs::write(
            worker.join("index.ts"),
            "export default () => new Response('ok');",
        )
        .unwrap();
        let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let temp_path = temp.path().to_path_buf();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *TEST_BUILD_PAUSE_END.lock().unwrap() = Some(BuildPause {
            started: started_tx,
            release: release_rx,
        });

        let export =
            tokio::spawn(
                async move { export_state(&index, None, Some(temp_path.as_path())).await },
            );
        match started_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(()) => {}
            Err(_) => {
                *TEST_BUILD_PAUSE_END.lock().unwrap() = None;
                panic!("a montagem não chegou à pausa de fim de build");
            }
        }
        // O ZIP já foi criado e ainda pertence à montagem pausada.
        assert_eq!(
            zip_count(temp.path()),
            1,
            "o ZIP deveria existir com a tarefa pausada no fim da montagem"
        );

        // Cancela a requisição de verdade (abort + conclusão cancelada)
        // ANTES de soltar a pausa: o future externo é derrubado; a montagem
        // bloqueante segue (não é cancelável) e, ao terminar, devolve o
        // resultado que ninguém consome.
        export.abort();
        let err = export.await.unwrap_err();
        assert!(
            err.is_cancelled(),
            "a task deveria terminar cancelada, obtive: {err:?}"
        );
        // Com a montagem ainda pausada, o dono (`TempPath`) segue na
        // closure: o abort não removeu nada.
        assert_eq!(
            zip_count(temp.path()),
            1,
            "o cancelamento não deveria remover o ZIP com a montagem pausada"
        );
        release_tx.send(()).unwrap();

        // A closure termina: o resultado não consumido cai e o owner apaga
        // o ZIP.
        let deadline = Instant::now() + Duration::from_secs(10);
        while zip_count(temp.path()) > 0 {
            assert!(
                Instant::now() < deadline,
                "o ZIP sobreviveu ao cancelamento"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(zip_count(temp.path()), 0);
    }

    /// A posse RAII do `StateExport` cobre o caminho em que o arquivo existe
    /// mas o stream nunca assume (erro no `File::open` ou cancelamento):
    /// drop sem `into_owned` remove o ZIP.
    #[test]
    fn dropped_export_removes_zip_without_stream() {
        let temp = tempfile::tempdir().unwrap();
        let file = tempfile::Builder::new()
            .prefix("edger-state-")
            .suffix(".zip")
            .tempfile_in(temp.path())
            .unwrap();
        let owned = file.into_temp_path();
        let path = (*owned).to_path_buf();
        let export = StateExport {
            path: path.clone(),
            filename: "edger-state-test.zip".into(),
            owned: Some(owned),
        };
        drop(export);
        assert!(!path.exists(), "o owner deveria ter removido o ZIP");
    }

    /// O `into_owned` transfere a posse sem remover o arquivo: o drop do
    /// resultado já transferido não apaga o ZIP (sem dupla remoção) e a
    /// limpeza passa a ser do body.
    #[test]
    fn into_owned_disowns_and_keeps_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = tempfile::Builder::new()
            .prefix("edger-state-")
            .suffix(".zip")
            .tempfile_in(temp.path())
            .unwrap();
        let owned = file.into_temp_path();
        let path = (*owned).to_path_buf();
        let export = StateExport {
            path: path.clone(),
            filename: "edger-state-test.zip".into(),
            owned: Some(owned),
        };
        let (transferred, filename) = export.into_owned();
        assert_eq!(transferred, path);
        assert_eq!(filename, "edger-state-test.zip");
        // O `into_owned` consumiu o shell: nada mais tem a posse RAII, e o
        // arquivo segue no disco (a limpeza é do consumer daqui).
        assert!(path.exists(), "o handoff não deve apagar o ZIP");
        // E a limpeza fica com o consumer (aqui, o teste).
        std::fs::remove_file(&path).unwrap();
    }

    /// Contador de ZIPs de export num diretório (o prefixo do builder é
    /// `edger-state-`).
    fn zip_count(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with("edger-state-") && name.ends_with(".zip")
            })
            .count()
    }
}
