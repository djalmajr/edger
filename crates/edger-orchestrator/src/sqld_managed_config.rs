//! EDG-54 (15c): configuração do host e carregamento das chaves do serviço
//! SQLD gerenciado. Só configuração, arquivos privados e CONSTRUÇÃO local do
//! [`SqldProvisionService`]; HTTP admin, factory e `bin` são outra fatia.
//!
//! # Entradas (exclusivas do operador, sem default)
//!
//! Sete variáveis, exatamente: [`ENV_ADMIN_URL`], [`ENV_ADMIN_KEY_FILE`],
//! [`ENV_USER_URL`], [`ENV_REGISTRY_DB`], [`ENV_SIGNER_KEY_FILE`],
//! [`ENV_MANAGED_BUDGET_BYTES`], [`ENV_BACKUP_DIR`]. Nenhuma presente =
//! [`SqldManagedSettings::Off`] (sem I/O). Qualquer presente exige todas
//! não-vazias e válidas: presença vazia/inválida/não-Unicode NUNCA vira Off.
//! [`parse_sqld_managed_settings`] recebe um lookup (testável sem mutar
//! `std::env`); [`sqld_managed_settings_from_env`] é só conveniência de boot.
//!
//! # Carregamento ([`load_sqld_managed`])
//!
//! Chamado uma vez no boot, nunca por request/worker. Fluxo: `Off` retorna
//! antes de qualquer acesso; fora de Unix, configurado = `Unsupported`
//! estático (nada é tocado); cofre indisponível = erro estático. O restante
//! (arquivos, SQLite, cripto) roda em `spawn_blocking` sob admissão limitada
//! (o `await` da admissão é cancelável; o permit é do closure e só cai ao
//! término físico, mesmo com o `await` cancelado). Ordem no closure:
//!
//! 1. canonicaliza as roots que guests leem (obrigatórias, existentes);
//! 2. valida a master do AppVault (path/metadata, sem reler) e as chaves;
//! 3. chaves: arquivo REGULAR validado pelo fd aberto (owner root/euid, sem
//!    acesso a others, sem group-write/exec nem owner-exec; group-read só se
//!    o gid é do processo), fora das roots, com leitura limitada;
//! 4. master do signer != master do vault (`subtle::ConstantTimeEq`);
//! 5. cliente admin, store de backup (diretório `0700` existente) e só então
//!    o ledger (`SqldProvisionRegistry::open`, que valida orçamento e
//!    permissões antes de criar o arquivo) e o serviço.
//!
//! Garantia limitada: tudo acima valida o estado visto NAQUELE momento; um
//! ator com o mesmo UID/root que troque arquivos depois está fora da
//! fronteira. Pais e configuração do operador são confiáveis. A construção
//! não faz probe/CREATE/HTTP; `ensure` segue sendo a ação root explícita.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use crate::app_secrets::AppSecretVault;
use crate::sqld_admin::SqldAdminError;
use crate::sqld_backup::SqldBackupError;
use crate::sqld_provision::{SqldProvisionError, SqldProvisionService};
use crate::sqld_registry::SqldRegistryError;

pub const ENV_ADMIN_URL: &str = "EDGER_SQLD_ADMIN_URL";
pub const ENV_ADMIN_KEY_FILE: &str = "EDGER_SQLD_ADMIN_KEY_FILE";
pub const ENV_USER_URL: &str = "EDGER_SQLD_USER_URL";
pub const ENV_REGISTRY_DB: &str = "EDGER_SQLD_REGISTRY_DB";
pub const ENV_SIGNER_KEY_FILE: &str = "EDGER_SQLD_SIGNER_KEY_FILE";
pub const ENV_MANAGED_BUDGET_BYTES: &str = "EDGER_SQLD_MANAGED_BUDGET_BYTES";
pub const ENV_BACKUP_DIR: &str = "EDGER_SQLD_BACKUP_DIR";

/// As sete entradas, na ordem de validação.
pub const ENV_NAMES: [&str; 7] = [
    ENV_ADMIN_URL,
    ENV_ADMIN_KEY_FILE,
    ENV_USER_URL,
    ENV_REGISTRY_DB,
    ENV_SIGNER_KEY_FILE,
    ENV_MANAGED_BUDGET_BYTES,
    ENV_BACKUP_DIR,
];

/// Limite de leitura da chave admin.
pub const MAX_ADMIN_KEY_BYTES: usize = 4096;
/// Limite de leitura da master do signer, antes de qualquer decode.
pub const MAX_MASTER_FILE_BYTES: usize = 256;
const MASTER_LEN: usize = 32;
/// Jobs de boot simultâneos (admissão do processo).
pub const BOOT_MAX_BLOCKING: usize = 2;
/// Espera máxima pela admissão.
pub const BOOT_ADMISSION_WAIT: Duration = Duration::from_secs(15);

/// Erros estáticos: nunca carregam valores, tamanhos secretos, conteúdo, URL,
/// caminho nem resposta remota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldManagedError {
    /// Algumas entradas presentes, outras ausentes.
    Incomplete,
    /// Entrada presente e vazia.
    EmptyValue,
    /// Entrada que não é Unicode.
    NotUnicode,
    InvalidUrl,
    /// Admin e dados na mesma origem normalizada.
    UrlsNotDistinct,
    InvalidBudget,
    /// Caminho de entrada não absoluto.
    InvalidPath,
    /// Fora de Unix com serviço configurado.
    Unsupported,
    /// Cofre sem chave mestra.
    VaultUnavailable,
    /// Roots de guests vazias, inexistentes ou não canonicalizáveis.
    InvalidRoots,
    /// Chave/master dentro de uma root legível pelos guests.
    KeyInGuestRoot,
    KeyFileUnreadable,
    /// Não regular, owner ou modo indevidos.
    KeyFileUnsafe,
    AdminKeyInvalid,
    SignerKeyInvalid,
    /// Master do signer igual à do AppVault.
    MasterEqualsVault,
    /// Admissão de boot esgotada.
    Busy,
    /// Job abortado/paniquado ou semáforo fechado.
    Internal,
    Admin(SqldAdminError),
    Backup(SqldBackupError),
    Registry(SqldRegistryError),
    Provision(SqldProvisionError),
}

impl fmt::Display for SqldManagedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Incomplete => "managed sqld configuration is incomplete",
            Self::EmptyValue => "a managed sqld setting is empty",
            Self::NotUnicode => "a managed sqld setting is not valid unicode",
            Self::InvalidUrl => "a managed sqld url is invalid",
            Self::UrlsNotDistinct => "managed sqld admin and data origins must differ",
            Self::InvalidBudget => "the managed sqld budget is invalid",
            Self::InvalidPath => "a managed sqld path must be absolute",
            Self::Unsupported => "the managed sqld service needs a unix host",
            Self::VaultUnavailable => "the app secrets vault is unavailable",
            Self::InvalidRoots => "the guest-readable roots are invalid",
            Self::KeyInGuestRoot => "a key file lies inside a guest-readable root",
            Self::KeyFileUnreadable => "a key file cannot be read",
            Self::KeyFileUnsafe => "a key file is not a private regular file",
            Self::AdminKeyInvalid => "the admin key is invalid",
            Self::SignerKeyInvalid => "the signer master key is invalid",
            Self::MasterEqualsVault => "the signer master equals the app secrets master",
            Self::Busy => "managed sqld boot admission is exhausted",
            Self::Internal => "managed sqld boot job failed",
            Self::Admin(_) => "the sqld admin client was refused",
            Self::Backup(_) => "the sqld backup store was refused",
            Self::Registry(_) => "the sqld registry was refused",
            Self::Provision(_) => "the sqld provision service was refused",
        })
    }
}

impl std::error::Error for SqldManagedError {}

/// Configuração validada sintaticamente (sem I/O). Campos privados; `Debug`
/// não mostra URLs nem caminhos.
#[derive(Clone, PartialEq, Eq)]
pub struct SqldManagedConfig {
    admin_url: String,
    admin_key_file: PathBuf,
    user_url: String,
    registry_db: PathBuf,
    signer_key_file: PathBuf,
    budget_bytes: u64,
    backup_dir: PathBuf,
}

impl fmt::Debug for SqldManagedConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldManagedConfig").finish_non_exhaustive()
    }
}

/// Resultado do parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqldManagedSettings {
    Off,
    Configured(SqldManagedConfig),
}

/// Parser por lookup (sem tocar `std::env`). O lookup é chamado uma vez por
/// nome de [`ENV_NAMES`], na ordem; `None` = ausente.
pub fn parse_sqld_managed_settings<F>(
    mut lookup: F,
) -> Result<SqldManagedSettings, SqldManagedError>
where
    F: FnMut(&str) -> Option<OsString>,
{
    let raw: Vec<Option<OsString>> = ENV_NAMES.iter().map(|name| lookup(name)).collect();
    if raw.iter().all(Option::is_none) {
        return Ok(SqldManagedSettings::Off);
    }
    let mut values: Vec<String> = Vec::with_capacity(ENV_NAMES.len());
    let mut missing = false;
    for entry in raw {
        match entry {
            None => missing = true,
            Some(os) => {
                let value = os.into_string().map_err(|_| SqldManagedError::NotUnicode)?;
                if value.trim().is_empty() {
                    return Err(SqldManagedError::EmptyValue);
                }
                values.push(value);
            }
        }
    }
    if missing {
        return Err(SqldManagedError::Incomplete);
    }
    let mut it = values.into_iter();
    let mut next = || it.next().expect("seven values checked above");
    let admin_url = next();
    let admin_key_file = next();
    let user_url = next();
    let registry_db = next();
    let signer_key_file = next();
    let budget = next();
    let backup_dir = next();

    let admin_origin = origin_of(&admin_url)?;
    let user_origin = origin_of(&user_url)?;
    if admin_origin == user_origin {
        return Err(SqldManagedError::UrlsNotDistinct);
    }
    let budget_bytes = parse_budget(&budget)?;
    let absolute = |s: String| {
        let path = PathBuf::from(s);
        if path.is_absolute() {
            Ok(path)
        } else {
            Err(SqldManagedError::InvalidPath)
        }
    };
    Ok(SqldManagedSettings::Configured(SqldManagedConfig {
        admin_url,
        admin_key_file: absolute(admin_key_file)?,
        user_url,
        registry_db: absolute(registry_db)?,
        signer_key_file: absolute(signer_key_file)?,
        budget_bytes,
        backup_dir: absolute(backup_dir)?,
    }))
}

/// Conveniência de boot: lê o ambiente do processo. Nunca chamar por
/// request/worker.
pub fn sqld_managed_settings_from_env() -> Result<SqldManagedSettings, SqldManagedError> {
    parse_sqld_managed_settings(|name| std::env::var_os(name))
}

/// Origem normalizada (esquema, host, porta efetiva). Sem espaço/controle:
/// `Url::parse` removeria silenciosamente tab/LF.
fn origin_of(value: &str) -> Result<String, SqldManagedError> {
    if value
        .bytes()
        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(SqldManagedError::InvalidUrl);
    }
    let url = url::Url::parse(value).map_err(|_| SqldManagedError::InvalidUrl)?;
    if !url.origin().is_tuple() {
        return Err(SqldManagedError::InvalidUrl);
    }
    Ok(url.origin().ascii_serialization())
}

/// Inteiro decimal explícito (sem sinal, espaço ou hex); mínimo/alinhamento
/// são do registry.
fn parse_budget(value: &str) -> Result<u64, SqldManagedError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SqldManagedError::InvalidBudget);
    }
    value
        .parse::<u64>()
        .map_err(|_| SqldManagedError::InvalidBudget)
}

/// Evidência da master EXATA já carregada para o `AppSecretVault` recebido:
/// os bytes (só para igualdade) e o caminho do arquivo de onde vieram.
/// Nenhuma releitura é usada para comparar.
pub struct SqldVaultMasterEvidence {
    key: Zeroizing<[u8; MASTER_LEN]>,
    path: PathBuf,
}

impl SqldVaultMasterEvidence {
    pub fn new(key: Zeroizing<[u8; MASTER_LEN]>, path: PathBuf) -> Self {
        Self { key, path }
    }
}

impl fmt::Debug for SqldVaultMasterEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldVaultMasterEvidence")
            .finish_non_exhaustive()
    }
}

/// Serviço montado + origem de dados validada para a futura fábrica. Não
/// guarda master nem chave; `Debug` não mostra nada.
#[derive(Clone)]
pub struct SqldManagedEnabled {
    service: Arc<SqldProvisionService>,
    data_origin: String,
}

impl SqldManagedEnabled {
    pub fn service(&self) -> &Arc<SqldProvisionService> {
        &self.service
    }

    /// Origem de dados normalizada (`http://host[:porta]/`).
    pub fn data_origin(&self) -> &str {
        &self.data_origin
    }
}

impl fmt::Debug for SqldManagedEnabled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldManagedEnabled").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum SqldManagedRuntime {
    Off,
    Enabled(SqldManagedEnabled),
}

fn boot_admission() -> Arc<Semaphore> {
    static GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(Semaphore::new(BOOT_MAX_BLOCKING)))
        .clone()
}

/// Admissão cancelável + job bloqueante. O permit é movido para o closure: só
/// cai quando o job termina fisicamente, inclusive se o chamador cancelar o
/// `await` depois da admissão.
async fn run_admitted<T, F>(
    gate: Arc<Semaphore>,
    wait: Duration,
    job: F,
) -> Result<T, SqldManagedError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = match tokio::time::timeout(wait, gate.acquire_owned()).await {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => return Err(SqldManagedError::Internal),
        Err(_) => return Err(SqldManagedError::Busy),
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job()
    })
    .await
    .map_err(|_| SqldManagedError::Internal)
}

/// Monta o serviço a partir das entradas do operador. `roots` são as roots
/// REAIS que guests podem ler (worker/core/cache). `Off` retorna sem acesso.
/// Sem probe/CREATE/HTTP e sem tarefa de fundo.
pub async fn load_sqld_managed(
    settings: SqldManagedSettings,
    vault: Arc<AppSecretVault>,
    vault_master: SqldVaultMasterEvidence,
    roots: Vec<PathBuf>,
) -> Result<SqldManagedRuntime, SqldManagedError> {
    let config = match settings {
        SqldManagedSettings::Off => return Ok(SqldManagedRuntime::Off),
        SqldManagedSettings::Configured(config) => config,
    };
    load_configured(config, vault, vault_master, roots).await
}

#[cfg(not(unix))]
async fn load_configured(
    _config: SqldManagedConfig,
    _vault: Arc<AppSecretVault>,
    _vault_master: SqldVaultMasterEvidence,
    _roots: Vec<PathBuf>,
) -> Result<SqldManagedRuntime, SqldManagedError> {
    Err(SqldManagedError::Unsupported)
}

#[cfg(unix)]
async fn load_configured(
    config: SqldManagedConfig,
    vault: Arc<AppSecretVault>,
    vault_master: SqldVaultMasterEvidence,
    roots: Vec<PathBuf>,
) -> Result<SqldManagedRuntime, SqldManagedError> {
    if !vault.is_available() {
        return Err(SqldManagedError::VaultUnavailable);
    }
    run_admitted(boot_admission(), BOOT_ADMISSION_WAIT, move || {
        unix::build(config, vault, vault_master, roots)
    })
    .await?
}

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::OwnedFd;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use base64::Engine as _;
    use rustix::fs::{fstat, open, FileType, Mode, OFlags};
    use subtle::ConstantTimeEq;
    use zeroize::Zeroizing;

    use super::{
        SqldManagedConfig, SqldManagedEnabled, SqldManagedError, SqldManagedRuntime,
        SqldVaultMasterEvidence, MASTER_LEN, MAX_ADMIN_KEY_BYTES, MAX_MASTER_FILE_BYTES,
    };
    use crate::app_secrets::AppSecretVault;
    use crate::sqld_admin::SqldAdminClient;
    use crate::sqld_backup::SqldBackupStore;
    use crate::sqld_provision::{SqldProvisionService, SqldSystemClock};
    use crate::sqld_registry::SqldProvisionRegistry;

    pub(super) fn build(
        config: SqldManagedConfig,
        vault: Arc<AppSecretVault>,
        vault_master: SqldVaultMasterEvidence,
        roots: Vec<PathBuf>,
    ) -> Result<SqldManagedRuntime, SqldManagedError> {
        let roots = canonical_roots(&roots)?;

        // Master do AppVault: só path/metadata; os bytes são da evidência.
        drop(open_private_file(&vault_master.path, &roots)?);

        let admin_key = {
            let mut file = open_private_file(&config.admin_key_file, &roots)?;
            let raw = read_capped(&mut file, MAX_ADMIN_KEY_BYTES)
                .map_err(|e| e.or(SqldManagedError::AdminKeyInvalid))?;
            parse_admin_key(&raw)?
        };
        let signer = {
            let mut file = open_private_file(&config.signer_key_file, &roots)?;
            let raw = read_capped(&mut file, MAX_MASTER_FILE_BYTES)
                .map_err(|e| e.or(SqldManagedError::SignerKeyInvalid))?;
            parse_master(&raw)?
        };
        let equal: bool = signer[..].ct_eq(&vault_master.key[..]).into();
        if equal {
            return Err(SqldManagedError::MasterEqualsVault);
        }
        drop(vault_master);

        // Construtores reais, sem HTTP. O ledger só nasce depois de tudo.
        let admin =
            SqldAdminClient::new(&config.admin_url, admin_key).map_err(SqldManagedError::Admin)?;
        let backup = SqldBackupStore::new(&config.user_url, config.backup_dir.clone())
            .map_err(SqldManagedError::Backup)?;
        let registry =
            SqldProvisionRegistry::open(&config.registry_db, signer, config.budget_bytes)
                .map_err(SqldManagedError::Registry)?;
        let service = SqldProvisionService::new(
            Arc::new(registry),
            vault,
            admin,
            backup,
            &config.user_url,
            Arc::new(SqldSystemClock),
        )
        .map_err(SqldManagedError::Provision)?;
        let data_origin = url::Url::parse(&config.user_url)
            .map_err(|_| SqldManagedError::InvalidUrl)?
            .to_string();
        Ok(SqldManagedRuntime::Enabled(SqldManagedEnabled {
            service: Arc::new(service),
            data_origin,
        }))
    }

    fn canonical_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>, SqldManagedError> {
        if roots.is_empty() {
            return Err(SqldManagedError::InvalidRoots);
        }
        roots
            .iter()
            .map(|root| std::fs::canonicalize(root).map_err(|_| SqldManagedError::InvalidRoots))
            .collect()
    }

    /// Recusa o arquivo se ele (alvo final) ou o lugar onde o operador o pôs
    /// (pai canônico + nome) estiver dentro de uma root de guest.
    fn outside_roots(path: &Path, roots: &[PathBuf]) -> Result<(), SqldManagedError> {
        let unreadable = |_| SqldManagedError::KeyFileUnreadable;
        let target = std::fs::canonicalize(path).map_err(unreadable)?;
        let placed = match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => std::fs::canonicalize(parent)
                .map_err(unreadable)?
                .join(name),
            _ => return Err(SqldManagedError::KeyFileUnreadable),
        };
        if roots
            .iter()
            .any(|root| target.starts_with(root) || placed.starts_with(root))
        {
            return Err(SqldManagedError::KeyInGuestRoot);
        }
        Ok(())
    }

    /// Abre sem bloquear (FIFO/device não travam antes do `fstat`), segue
    /// symlink do operador e valida o fd aberto (alvo). Só então devolve.
    fn open_private_file(path: &Path, roots: &[PathBuf]) -> Result<File, SqldManagedError> {
        if !path.is_absolute() {
            return Err(SqldManagedError::InvalidPath);
        }
        outside_roots(path, roots)?;
        let fd: OwnedFd = open(
            path,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| SqldManagedError::KeyFileUnreadable)?;
        let st = fstat(&fd).map_err(|_| SqldManagedError::KeyFileUnreadable)?;
        let regular = FileType::from_raw_mode(st.st_mode as _) == FileType::RegularFile;
        let groups: Vec<u32> = rustix::process::getgroups()
            .map_err(|_| SqldManagedError::KeyFileUnreadable)?
            .iter()
            .map(|gid| gid.as_raw())
            .collect();
        let safe = regular
            && mode_acceptable(
                st.st_uid,
                st.st_gid,
                st.st_mode as u32,
                rustix::process::geteuid().as_raw(),
                rustix::process::getegid().as_raw(),
                &groups,
            );
        if !safe {
            return Err(SqldManagedError::KeyFileUnsafe);
        }
        Ok(File::from(fd))
    }

    /// Regra pura de owner/modo (testável sem privilégio).
    pub(super) fn mode_acceptable(
        uid: u32,
        gid: u32,
        mode: u32,
        euid: u32,
        egid: u32,
        groups: &[u32],
    ) -> bool {
        if uid != 0 && uid != euid {
            return false;
        }
        // setuid/setgid/sticky, qualquer bit de others, group write/exec e
        // owner exec.
        if mode & 0o7000 != 0 || mode & 0o007 != 0 || mode & 0o030 != 0 || mode & 0o100 != 0 {
            return false;
        }
        if mode & 0o040 != 0 && gid != egid && !groups.contains(&gid) {
            return false;
        }
        true
    }

    #[derive(Debug)]
    enum ReadFailure {
        Io,
        TooLarge,
    }

    impl ReadFailure {
        /// `TooLarge` vira o erro de conteúdo do chamador; I/O é ilegível.
        fn or(self, too_large: SqldManagedError) -> SqldManagedError {
            match self {
                Self::Io => SqldManagedError::KeyFileUnreadable,
                Self::TooLarge => too_large,
            }
        }
    }

    /// Lê no máximo `cap` bytes (laço limitado a `cap + 1`, sem `read_to_end`
    /// e sem realocar): mais que `cap` é recusa. O buffer é zeroizado.
    fn read_capped(file: &mut File, cap: usize) -> Result<Zeroizing<Vec<u8>>, ReadFailure> {
        let mut buf = Zeroizing::new(vec![0u8; cap + 1]);
        let mut len = 0;
        while len < buf.len() {
            match file.read(&mut buf[len..]) {
                Ok(0) => break,
                Ok(n) => len += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(ReadFailure::Io),
            }
        }
        if len > cap {
            return Err(ReadFailure::TooLarge);
        }
        buf.truncate(len);
        Ok(buf)
    }

    /// Trim ASCII exterior UMA vez; não vazio e ASCII visível 0x21..0x7e. O SQLD
    /// compara só o 2º token do header (`split_whitespace`), então espaço
    /// interno nunca autentica e é recusado aqui, antes de qualquer constructor.
    fn parse_admin_key(raw: &[u8]) -> Result<Zeroizing<String>, SqldManagedError> {
        let trimmed = raw.trim_ascii();
        if trimmed.is_empty() || !trimmed.iter().all(|b| (0x21..=0x7e).contains(b)) {
            return Err(SqldManagedError::AdminKeyInvalid);
        }
        let text = std::str::from_utf8(trimmed).map_err(|_| SqldManagedError::AdminKeyInvalid)?;
        Ok(Zeroizing::new(text.to_owned()))
    }

    /// Raw32 conserva TODOS os bytes (inclusive LF); senão base64 padrão de
    /// exatos 32 bytes, sem brancos (mesma regra do loader do cofre).
    fn parse_master(raw: &[u8]) -> Result<Zeroizing<[u8; MASTER_LEN]>, SqldManagedError> {
        let invalid = SqldManagedError::SignerKeyInvalid;
        if raw.len() == MASTER_LEN {
            let mut out = Zeroizing::new([0u8; MASTER_LEN]);
            out.copy_from_slice(raw);
            return Ok(out);
        }
        let compact: Zeroizing<Vec<u8>> = Zeroizing::new(
            raw.iter()
                .filter(|b| !b.is_ascii_whitespace())
                .copied()
                .collect(),
        );
        let decoded = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(&compact[..])
                .map_err(|_| invalid)?,
        );
        if decoded.len() != MASTER_LEN {
            return Err(invalid);
        }
        let mut out = Zeroizing::new([0u8; MASTER_LEN]);
        out.copy_from_slice(&decoded);
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const ME: u32 = 1000;
        const MY_GID: u32 = 1000;
        const FS_GROUP: u32 = 2000;

        fn ok(uid: u32, gid: u32, mode: u32, groups: &[u32]) -> bool {
            mode_acceptable(uid, gid, mode, ME, MY_GID, groups)
        }

        #[test]
        fn owner_and_mode_matrix() {
            assert!(ok(ME, MY_GID, 0o400, &[]));
            assert!(ok(ME, MY_GID, 0o600, &[]));
            // root:fsGroup 0440 com o grupo suplementar do processo.
            assert!(ok(0, FS_GROUP, 0o440, &[FS_GROUP]));
            assert!(ok(0, MY_GID, 0o440, &[]), "egid counts");
            assert!(!ok(0, FS_GROUP, 0o440, &[7]), "group not ours");
            assert!(!ok(4242, MY_GID, 0o600, &[]), "foreign owner");
            assert!(!ok(ME, MY_GID, 0o644, &[]), "others read");
            assert!(!ok(ME, MY_GID, 0o604, &[]), "others read only");
            assert!(!ok(ME, MY_GID, 0o602, &[]), "others write");
            assert!(!ok(ME, MY_GID, 0o601, &[]), "others exec");
            assert!(!ok(ME, MY_GID, 0o620, &[]), "group write");
            assert!(!ok(ME, MY_GID, 0o610, &[]), "group exec");
            assert!(!ok(ME, MY_GID, 0o500, &[]), "owner exec");
            assert!(!ok(ME, MY_GID, 0o4600, &[]), "setuid");
            assert!(!ok(ME, MY_GID, 0o1600, &[]), "sticky");
        }

        #[test]
        fn admin_key_trim_once_and_visible_ascii() {
            let key = parse_admin_key(b" \r\n k-e-y\n\t ").unwrap();
            assert_eq!(key.as_str(), "k-e-y");
            assert!(parse_admin_key(b"k e-y").is_err(), "interior space");
            assert!(parse_admin_key(b" \n k  e-y \t").is_err(), "interior space");
            assert!(parse_admin_key(b" \n").is_err());
            assert!(parse_admin_key(b"a\x01b").is_err());
            assert!(parse_admin_key(b"a\tb").is_err());
            assert!(parse_admin_key("a\u{e9}b".as_bytes()).is_err());
            assert!(parse_admin_key(b"a\x7fb").is_err());
        }

        #[test]
        fn master_raw_keeps_every_byte_and_base64_fallback() {
            let mut raw = [7u8; 32];
            raw[10] = b'\n';
            raw[31] = b'\n';
            assert_eq!(&parse_master(&raw).unwrap()[..], &raw[..]);
            let b64 = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
            let spaced = format!("  {}\n", b64);
            assert_eq!(
                &parse_master(spaced.as_bytes()).unwrap()[..],
                &[9u8; 32][..]
            );
            let short = base64::engine::general_purpose::STANDARD.encode([9u8; 31]);
            assert!(parse_master(short.as_bytes()).is_err());
            let long = base64::engine::general_purpose::STANDARD.encode([9u8; 33]);
            assert!(parse_master(long.as_bytes()).is_err());
            assert!(parse_master(&[7u8; 31]).is_err());
        }

        #[test]
        fn read_capped_is_bounded_without_metadata() {
            use std::io::Write;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("f");
            std::fs::File::create(&path)
                .unwrap()
                .write_all(&[b'a'; 11])
                .unwrap();
            assert_eq!(
                read_capped(&mut File::open(&path).unwrap(), 11)
                    .unwrap()
                    .len(),
                11
            );
            assert!(read_capped(&mut File::open(&path).unwrap(), 10).is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    #[tokio::test]
    async fn admission_wait_is_cancelable_and_busy_on_timeout() {
        let gate = Arc::new(Semaphore::new(0));
        let err = run_admitted(gate.clone(), Duration::from_millis(30), || 1u8)
            .await
            .unwrap_err();
        assert_eq!(err, SqldManagedError::Busy);
        // Cancelar o await de admissão não deixa nada ocupado.
        let waiting = tokio::spawn(run_admitted(gate.clone(), Duration::from_secs(30), || 1u8));
        tokio::task::yield_now().await;
        waiting.abort();
        let _ = waiting.await;
        gate.add_permits(1);
        assert_eq!(
            gate.available_permits(),
            1,
            "no stray waiter took the permit"
        );
    }

    #[tokio::test]
    async fn permit_is_held_until_the_job_physically_ends_even_if_awaiter_is_cancelled() {
        let gate = Arc::new(Semaphore::new(1));
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let task = tokio::spawn(run_admitted(
            gate.clone(),
            Duration::from_secs(5),
            move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                flag.store(true, Ordering::SeqCst);
            },
        ));
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        task.abort();
        let _ = task.await;
        assert_eq!(
            gate.available_permits(),
            0,
            "job still running: permit kept"
        );
        assert!(!finished.load(Ordering::SeqCst));
        release_tx.send(()).unwrap();
        let permit = tokio::time::timeout(Duration::from_secs(10), gate.clone().acquire_owned())
            .await
            .expect("permit returns once the job ends")
            .unwrap();
        assert!(finished.load(Ordering::SeqCst));
        drop(permit);
    }
}
