//! EDG-54 (15c): parser de configuração + carregador do serviço SQLD
//! gerenciado. Sem rede real: servidores TCP de fixture só detectam (ou
//! registram) requests. Nada aqui muta `std::env`; as variáveis passam por
//! um lookup. O que depende de owner/chown roda só como root (WSL); o
//! subprocesso UID/fsGroup roda sob `setpriv` sem capabilities.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use edger_orchestrator::app_secrets::AppSecretVault;
use edger_orchestrator::sqld_admin::SqldAdminError;
use edger_orchestrator::sqld_backup::SqldBackupError;
use edger_orchestrator::sqld_managed_config::{
    load_sqld_managed, parse_sqld_managed_settings, SqldManagedError, SqldManagedRuntime,
    SqldManagedSettings, SqldVaultMasterEvidence, ENV_ADMIN_KEY_FILE, ENV_ADMIN_URL,
    ENV_BACKUP_DIR, ENV_MANAGED_BUDGET_BYTES, ENV_NAMES, ENV_REGISTRY_DB, ENV_SIGNER_KEY_FILE,
    ENV_USER_URL,
};
use edger_orchestrator::sqld_provision::SqldOwner;
use edger_orchestrator::sqld_registry::{SqldProvisionRegistry, SqldRegistryError};
use zeroize::Zeroizing;

const BUDGET: u64 = 64 * 1024 * 1024;
const ADMIN_MARKER: &str = "ADMINSECRETMARKER-7f3a";
const SIGNER_BYTES: [u8; 32] = *b"MASTER-MARKER-0123456789-ABCDEFG";
const VAULT_BYTES: [u8; 32] = [0x11; 32];

// ───────────────────────── fixture de rede ─────────────────────────

/// Servidor TCP que só conta/registra requests. Responde 401 sem
/// `Authorization` e 404 com ele (o bastante para o probe do `ensure`).
struct Probe {
    port: u16,
    hits: Arc<AtomicUsize>,
    heads: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Probe {
    fn start() -> Self {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let heads = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (h, hd, st) = (hits.clone(), heads.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !st.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        h.fetch_add(1, Ordering::SeqCst);
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 1024];
                        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let head = String::from_utf8_lossy(&buf).into_owned();
                        let authed = head
                            .lines()
                            .any(|l| l.to_ascii_lowercase().starts_with("authorization:"));
                        hd.lock().unwrap().push(head);
                        let status = if authed {
                            "404 Not Found"
                        } else {
                            "401 Unauthorized"
                        };
                        let _ = stream.write_all(
                            format!(
                                "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            )
                            .as_bytes(),
                        );
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Self {
            port,
            hits,
            heads,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ───────────────────────── fixture de arquivos ─────────────────────────

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[cfg(unix)]
fn write_file(path: &Path, bytes: &[u8], mode: u32) {
    // O fixture só reescreve arquivos fake de que é dono: prepara o modo de
    // escrita explicitamente (sem depender de CAP_DAC_OVERRIDE) e depois
    // restaura o modo-alvo.
    if path.exists() {
        set_mode(path, 0o600);
    }
    std::fs::write(path, bytes).unwrap();
    set_mode(path, mode);
}

#[cfg(unix)]
fn private_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    set_mode(path, 0o700);
}

#[cfg(unix)]
fn is_root() -> bool {
    rustix::process::geteuid().is_root()
}

#[cfg(unix)]
struct Fx {
    dir: tempfile::TempDir,
    admin: Probe,
    user: Probe,
    vault: Arc<AppSecretVault>,
    vault_master: [u8; 32],
    admin_key: PathBuf,
    signer_key: PathBuf,
    vault_key: PathBuf,
    ledger: PathBuf,
    backups: PathBuf,
    work: PathBuf,
    budget: String,
}

#[cfg(unix)]
impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Caminho canônico (o /tmp do WSL não é symlink, mas não dependa).
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let keys = base.join("keys");
        private_dir(&keys);
        let work = base.join("work");
        std::fs::create_dir_all(work.join(".edger")).unwrap();
        let ledger_dir = base.join("ledger");
        private_dir(&ledger_dir);
        let backups = base.join("backups");
        private_dir(&backups);
        let admin_key = keys.join("admin.key");
        write_file(&admin_key, format!("{ADMIN_MARKER}\n").as_bytes(), 0o600);
        let signer_key = keys.join("signer.key");
        write_file(&signer_key, &SIGNER_BYTES, 0o600);
        let vault_key = keys.join("vault.key");
        write_file(&vault_key, &VAULT_BYTES, 0o600);
        let vault = Arc::new(
            AppSecretVault::open(keys.join("vault.db"), Zeroizing::new(VAULT_BYTES)).unwrap(),
        );
        Self {
            dir,
            admin: Probe::start(),
            user: Probe::start(),
            vault,
            vault_master: VAULT_BYTES,
            admin_key,
            signer_key,
            vault_key,
            ledger: ledger_dir.join("registry.db"),
            backups,
            work,
            budget: BUDGET.to_string(),
        }
    }

    fn vars(&self) -> HashMap<&'static str, OsString> {
        let mut m = HashMap::new();
        m.insert(ENV_ADMIN_URL, OsString::from(self.admin.url()));
        m.insert(ENV_ADMIN_KEY_FILE, self.admin_key.clone().into_os_string());
        m.insert(ENV_USER_URL, OsString::from(self.user.url()));
        m.insert(ENV_REGISTRY_DB, self.ledger.clone().into_os_string());
        m.insert(
            ENV_SIGNER_KEY_FILE,
            self.signer_key.clone().into_os_string(),
        );
        m.insert(ENV_MANAGED_BUDGET_BYTES, OsString::from(&self.budget));
        m.insert(ENV_BACKUP_DIR, self.backups.clone().into_os_string());
        m
    }

    fn settings(&self) -> SqldManagedSettings {
        parse_map(&self.vars()).unwrap()
    }

    fn evidence(&self) -> SqldVaultMasterEvidence {
        SqldVaultMasterEvidence::new(Zeroizing::new(self.vault_master), self.vault_key.clone())
    }

    async fn load_with(
        &self,
        settings: SqldManagedSettings,
    ) -> Result<SqldManagedRuntime, SqldManagedError> {
        tokio::time::timeout(
            Duration::from_secs(30),
            load_sqld_managed(
                settings,
                self.vault.clone(),
                self.evidence(),
                vec![self.work.clone()],
            ),
        )
        .await
        .expect("load must not hang")
    }

    async fn load(&self) -> Result<SqldManagedRuntime, SqldManagedError> {
        self.load_with(self.settings()).await
    }

    /// Nenhum request chegou e o ledger não foi criado.
    fn assert_untouched(&self) {
        assert_eq!(self.admin.hits(), 0, "admin probed");
        assert_eq!(self.user.hits(), 0, "user probed");
        assert!(!self.ledger.exists(), "ledger created on a failure");
    }
}

fn parse_map(m: &HashMap<&'static str, OsString>) -> Result<SqldManagedSettings, SqldManagedError> {
    parse_sqld_managed_settings(|name| m.get(name).cloned())
}

fn abs(name: &str) -> OsString {
    std::env::temp_dir().join(name).into_os_string()
}

fn valid_vars() -> HashMap<&'static str, OsString> {
    let mut m = HashMap::new();
    m.insert(ENV_ADMIN_URL, OsString::from("http://127.0.0.1:18080"));
    m.insert(ENV_ADMIN_KEY_FILE, abs("SECRET-PATH-MARKER-admin"));
    m.insert(ENV_USER_URL, OsString::from("http://127.0.0.1:18081"));
    m.insert(ENV_REGISTRY_DB, abs("ledger/registry.db"));
    m.insert(ENV_SIGNER_KEY_FILE, abs("SECRET-PATH-MARKER-signer"));
    m.insert(ENV_MANAGED_BUDGET_BYTES, OsString::from(BUDGET.to_string()));
    m.insert(ENV_BACKUP_DIR, abs("backups"));
    m
}

// ───────────────────────── parser ─────────────────────────

#[test]
fn seven_names_are_exactly_the_operator_inputs() {
    assert_eq!(
        ENV_NAMES,
        [
            "EDGER_SQLD_ADMIN_URL",
            "EDGER_SQLD_ADMIN_KEY_FILE",
            "EDGER_SQLD_USER_URL",
            "EDGER_SQLD_REGISTRY_DB",
            "EDGER_SQLD_SIGNER_KEY_FILE",
            "EDGER_SQLD_MANAGED_BUDGET_BYTES",
            "EDGER_SQLD_BACKUP_DIR",
        ]
    );
}

#[test]
fn nothing_present_is_off_and_only_the_seven_names_are_looked_up() {
    let mut seen = Vec::new();
    let got = parse_sqld_managed_settings(|name| {
        seen.push(name.to_owned());
        None
    });
    assert_eq!(got, Ok(SqldManagedSettings::Off));
    assert_eq!(seen, ENV_NAMES.map(str::to_owned));
}

#[test]
fn full_valid_set_is_configured_and_debug_hides_values() {
    let got = parse_map(&valid_vars()).unwrap();
    assert!(matches!(got, SqldManagedSettings::Configured(_)));
    let dbg = format!("{got:?}");
    assert!(
        !dbg.contains("SECRET-PATH-MARKER") && !dbg.contains("127.0.0.1"),
        "{dbg}"
    );
}

#[test]
fn every_partial_combination_of_one_present_or_one_missing_is_rejected() {
    for name in ENV_NAMES {
        let mut only = HashMap::new();
        only.insert(name, valid_vars()[name].clone());
        assert_eq!(
            parse_map(&only),
            Err(SqldManagedError::Incomplete),
            "only {name}"
        );
        let mut all_but = valid_vars();
        all_but.remove(name);
        assert_eq!(
            parse_map(&all_but),
            Err(SqldManagedError::Incomplete),
            "missing {name}"
        );
    }
}

#[test]
fn present_but_empty_or_blank_never_becomes_off_or_a_default() {
    for name in ENV_NAMES {
        for blank in ["", "   ", "\t"] {
            let mut m = valid_vars();
            m.insert(name, OsString::from(blank));
            assert_eq!(
                parse_map(&m),
                Err(SqldManagedError::EmptyValue),
                "{name}={blank:?}"
            );
        }
        let mut alone = HashMap::new();
        alone.insert(name, OsString::new());
        assert_eq!(
            parse_map(&alone),
            Err(SqldManagedError::EmptyValue),
            "{name} alone"
        );
    }
}

#[cfg(unix)]
#[test]
fn non_unicode_fails_closed_for_every_input_even_alone() {
    use std::os::unix::ffi::OsStringExt;
    for name in ENV_NAMES {
        let bad = OsString::from_vec(vec![b'/', 0xff, 0xfe]);
        let mut m = valid_vars();
        m.insert(name, bad.clone());
        assert_eq!(parse_map(&m), Err(SqldManagedError::NotUnicode), "{name}");
        let mut alone = HashMap::new();
        alone.insert(name, bad);
        assert_eq!(
            parse_map(&alone),
            Err(SqldManagedError::NotUnicode),
            "{name} alone"
        );
    }
}

#[test]
fn invalid_urls_budget_and_paths_are_rejected_without_a_default() {
    let cases: Vec<(&str, &str, SqldManagedError)> = vec![
        (ENV_ADMIN_URL, "not a url", SqldManagedError::InvalidUrl),
        (
            ENV_ADMIN_URL,
            "http://a\n.example",
            SqldManagedError::InvalidUrl,
        ),
        (
            ENV_ADMIN_URL,
            " http://127.0.0.1:18080",
            SqldManagedError::InvalidUrl,
        ),
        (
            ENV_USER_URL,
            "127.0.0.1:18081",
            SqldManagedError::InvalidUrl,
        ),
        (
            ENV_USER_URL,
            "http://127.0.0.1:18080/",
            SqldManagedError::UrlsNotDistinct,
        ),
        (
            ENV_USER_URL,
            "http://127.0.0.1:18080",
            SqldManagedError::UrlsNotDistinct,
        ),
        (
            ENV_MANAGED_BUDGET_BYTES,
            "-1",
            SqldManagedError::InvalidBudget,
        ),
        (
            ENV_MANAGED_BUDGET_BYTES,
            "+67108864",
            SqldManagedError::InvalidBudget,
        ),
        (
            ENV_MANAGED_BUDGET_BYTES,
            "0x4000000",
            SqldManagedError::InvalidBudget,
        ),
        (
            ENV_MANAGED_BUDGET_BYTES,
            "64MiB",
            SqldManagedError::InvalidBudget,
        ),
        (
            ENV_MANAGED_BUDGET_BYTES,
            "6 4",
            SqldManagedError::InvalidBudget,
        ),
        (
            ENV_MANAGED_BUDGET_BYTES,
            "99999999999999999999999",
            SqldManagedError::InvalidBudget,
        ),
        (
            ENV_ADMIN_KEY_FILE,
            "relative/admin.key",
            SqldManagedError::InvalidPath,
        ),
        (
            ENV_REGISTRY_DB,
            "registry.db",
            SqldManagedError::InvalidPath,
        ),
        (
            ENV_SIGNER_KEY_FILE,
            "./signer.key",
            SqldManagedError::InvalidPath,
        ),
        (ENV_BACKUP_DIR, "backups", SqldManagedError::InvalidPath),
    ];
    for (name, value, want) in cases {
        let mut m = valid_vars();
        m.insert(name, OsString::from(value));
        assert_eq!(parse_map(&m), Err(want), "{name}={value}");
    }
}

#[test]
fn origins_are_compared_after_normalisation() {
    let mut m = valid_vars();
    m.insert(ENV_ADMIN_URL, OsString::from("http://Example.COM:80"));
    m.insert(ENV_USER_URL, OsString::from("http://example.com/"));
    assert_eq!(parse_map(&m), Err(SqldManagedError::UrlsNotDistinct));
    m.insert(ENV_USER_URL, OsString::from("http://example.com:81/"));
    assert!(parse_map(&m).is_ok());
}

// ───────────────────────── carregador (unix) ─────────────────────────

#[tokio::test]
async fn off_returns_before_any_access_even_with_an_unavailable_vault() {
    let vault = Arc::new(AppSecretVault::unavailable());
    let evidence =
        SqldVaultMasterEvidence::new(Zeroizing::new([1u8; 32]), PathBuf::from("/nonexistent/x"));
    let got = load_sqld_managed(SqldManagedSettings::Off, vault, evidence, Vec::new())
        .await
        .unwrap();
    assert!(matches!(got, SqldManagedRuntime::Off));
}

#[cfg(unix)]
#[tokio::test]
async fn builds_the_real_service_from_temp_inputs_with_no_remote_traffic() {
    let fx = Fx::new();
    let runtime = fx.load().await.unwrap();
    let SqldManagedRuntime::Enabled(enabled) = runtime else {
        panic!("expected enabled")
    };
    assert_eq!(enabled.data_origin(), format!("{}/", fx.user.url()));
    // Metadata vazia: serviço real sobre o ledger recém-criado.
    let owner = SqldOwner::new("acme/app", None);
    let meta = enabled.service().metadata(&owner).await.unwrap();
    assert!(meta.is_none());
    assert!(fx.ledger.exists());
    assert_eq!(mode_of(&fx.ledger), 0o600);
    assert_eq!(fx.admin.hits(), 0, "constructor probed admin");
    assert_eq!(fx.user.hits(), 0, "constructor probed data");
    let dbg = format!("{enabled:?}");
    assert!(
        !dbg.contains(ADMIN_MARKER) && !dbg.contains("MASTER-MARKER") && !dbg.contains("127.0.0.1")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unavailable_vault_is_static_error_before_files_or_ledger() {
    let mut fx = Fx::new();
    fx.vault = Arc::new(AppSecretVault::unavailable());
    // Mesmo com arquivos ilegíveis: o erro é o do cofre, nada é aberto.
    std::fs::remove_file(&fx.admin_key).unwrap();
    let err = fx.load().await.unwrap_err();
    assert_eq!(err, SqldManagedError::VaultUnavailable);
    fx.assert_untouched();
}

#[cfg(unix)]
#[tokio::test]
async fn raw_and_base64_signer_are_equivalent_on_the_same_ledger() {
    use base64::Engine as _;
    let fx = Fx::new();
    fx.load().await.unwrap(); // raw: cria o ledger
    let b64 = base64::engine::general_purpose::STANDARD.encode(SIGNER_BYTES);
    write_file(&fx.signer_key, format!("\n  {b64} \r\n").as_bytes(), 0o600);
    // Mesma master por outro caminho: o ledger abre (Integrity se diferisse).
    fx.load().await.unwrap();
    // E diretamente com os mesmos 32 bytes.
    SqldProvisionRegistry::open(&fx.ledger, Zeroizing::new(SIGNER_BYTES), BUDGET).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn raw32_keeps_every_byte_including_lf_and_other_master_is_refused_by_ledger() {
    let fx = Fx::new();
    let mut raw = SIGNER_BYTES;
    raw[7] = b'\n';
    raw[31] = b'\n';
    write_file(&fx.signer_key, &raw, 0o600);
    fx.load().await.unwrap();
    SqldProvisionRegistry::open(&fx.ledger, Zeroizing::new(raw), BUDGET).unwrap();
    // Controle negativo: LF aparado (31 bytes + base64 inválido) é recusado e
    // os bytes sem o LF não abrem o ledger.
    let mut trimmed = raw;
    trimmed[31] = b'X';
    assert_eq!(
        SqldProvisionRegistry::open(&fx.ledger, Zeroizing::new(trimmed), BUDGET).err(),
        Some(SqldRegistryError::Integrity)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn signer_equal_to_vault_master_is_refused_before_the_ledger_in_both_encodings() {
    use base64::Engine as _;
    let fx = Fx::new();
    write_file(&fx.signer_key, &VAULT_BYTES, 0o600);
    assert_eq!(
        fx.load().await.unwrap_err(),
        SqldManagedError::MasterEqualsVault
    );
    fx.assert_untouched();
    let b64 = base64::engine::general_purpose::STANDARD.encode(VAULT_BYTES);
    write_file(&fx.signer_key, format!("{b64}\n").as_bytes(), 0o600);
    assert_eq!(
        fx.load().await.unwrap_err(),
        SqldManagedError::MasterEqualsVault
    );
    fx.assert_untouched();
    // Controle: uma master que difere em UM byte passa.
    let mut other = VAULT_BYTES;
    other[31] ^= 1;
    write_file(&fx.signer_key, &other, 0o600);
    assert!(fx.load().await.is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn signer_content_errors_are_static_and_create_no_ledger() {
    use base64::Engine as _;
    let fx = Fx::new();
    let b = |n: usize| base64::engine::general_purpose::STANDARD.encode(vec![5u8; n]);
    let cases: Vec<Vec<u8>> = vec![
        Vec::new(),
        vec![b'a'; 31],
        vec![b'a'; 33],
        b(31).into_bytes(),
        b(33).into_bytes(),
        vec![b'A'; 257], // acima do cap de 256 antes do decode
        b"not base64 at all!!!".to_vec(),
    ];
    for body in cases {
        write_file(&fx.signer_key, &body, 0o600);
        assert_eq!(
            fx.load().await.unwrap_err(),
            SqldManagedError::SignerKeyInvalid,
            "len {}",
            body.len()
        );
        fx.assert_untouched();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn admin_key_is_sent_raw_after_a_single_outer_trim() {
    let fx = Fx::new();
    write_file(&fx.admin_key, b" \r\n  AdminKeyValue123 \n\t", 0o600);
    let SqldManagedRuntime::Enabled(enabled) = fx.load().await.unwrap() else {
        panic!()
    };
    assert_eq!(fx.admin.hits(), 0, "constructor must not call admin");
    // `ensure` é a ação explícita; aqui só observamos o header do probe.
    let owner = SqldOwner::new("acme/app", None);
    let _ = tokio::time::timeout(
        Duration::from_secs(20),
        enabled.service().ensure(&owner, 16 * 1024 * 1024),
    )
    .await;
    let heads = fx.admin.heads.lock().unwrap().clone();
    assert!(heads.len() >= 2, "probe anon + authed expected: {heads:?}");
    assert!(
        !heads[0].to_ascii_lowercase().contains("authorization"),
        "first probe is anonymous"
    );
    let authed: Vec<&String> = heads
        .iter()
        .filter(|h| h.to_ascii_lowercase().contains("authorization"))
        .collect();
    assert!(!authed.is_empty());
    for head in authed {
        let line = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
            .unwrap();
        assert_eq!(
            line.split_once(':').unwrap().1.trim(),
            "Basic AdminKeyValue123"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn admin_key_validation_cap_control_and_internal_space_refused() {
    let fx = Fx::new();
    let bad: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"  \n\t ".to_vec(),
        b"ab\x01cd".to_vec(),
        b"ab\tcd".to_vec(),
        b"ab\x7fcd".to_vec(),
        "ab\u{e9}cd".as_bytes().to_vec(),
        vec![b'a'; 4097],
        vec![b'a'; 5000],
    ];
    for body in bad {
        write_file(&fx.admin_key, &body, 0o600);
        assert_eq!(
            fx.load().await.unwrap_err(),
            SqldManagedError::AdminKeyInvalid,
            "len {}",
            body.len()
        );
        fx.assert_untouched();
    }
    // Exatamente no limite: aceito.
    write_file(&fx.admin_key, &vec![b'a'; 4096], 0o600);
    assert!(fx.load().await.is_ok());
    // Espaço interno nunca autentica no SQLD (2º token): recusado cedo, antes de
    // qualquer constructor, ledger ou chamada admin; o erro não ecoa a chave.
    for body in [&b" \n two words \t"[..], b"two  words", b"a b"] {
        let fx2 = Fx::new();
        write_file(&fx2.admin_key, body, 0o600);
        let err = fx2.load().await.unwrap_err();
        assert_eq!(err, SqldManagedError::AdminKeyInvalid, "{body:?}");
        assert_eq!(fx2.admin.hits(), 0);
        fx2.assert_untouched();
        assert!(!format!("{err} {err:?}").contains("words"));
    }
    // Controle: o mesmo arquivo sem espaço interno (com trim exterior) é aceito.
    let fx3 = Fx::new();
    write_file(&fx3.admin_key, b" \n two-words \t", 0o600);
    assert!(matches!(
        fx3.load().await.unwrap(),
        SqldManagedRuntime::Enabled(_)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn non_regular_files_are_refused_before_reading_and_never_hang() {
    let fx = Fx::new();
    let fifo = fx.admin_key.with_file_name("fifo.key");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());
    set_mode(&fifo, 0o600);
    for target in [
        fifo.clone(),
        PathBuf::from("/dev/null"),
        fx.admin_key.parent().unwrap().to_path_buf(),
    ] {
        let mut fx_vars = fx.vars();
        fx_vars.insert(ENV_ADMIN_KEY_FILE, target.clone().into_os_string());
        let settings = parse_map(&fx_vars).unwrap();
        // /dev/null: dentro das roots? não; é char device, recusado pelo fstat.
        assert_eq!(
            fx.load_with(settings).await.unwrap_err(),
            SqldManagedError::KeyFileUnsafe,
            "{target:?}"
        );
        fx.assert_untouched();
    }
    // Symlink do operador para FIFO: o alvo do fd é o checado.
    let link = fx.admin_key.with_file_name("fifo-link.key");
    std::os::unix::fs::symlink(&fifo, &link).unwrap();
    let mut v = fx.vars();
    v.insert(ENV_ADMIN_KEY_FILE, link.into_os_string());
    assert_eq!(
        fx.load_with(parse_map(&v).unwrap()).await.unwrap_err(),
        SqldManagedError::KeyFileUnsafe
    );
    // Arquivo ausente.
    let mut v = fx.vars();
    v.insert(
        ENV_ADMIN_KEY_FILE,
        fx.admin_key.with_file_name("missing").into_os_string(),
    );
    assert_eq!(
        fx.load_with(parse_map(&v).unwrap()).await.unwrap_err(),
        SqldManagedError::KeyFileUnreadable
    );
    fx.assert_untouched();
}

#[cfg(unix)]
#[tokio::test]
async fn mode_and_owner_matrix_on_real_files_for_every_key() {
    let fx = Fx::new();
    // 0400/0600 permitidos (controle).
    for mode in [0o400, 0o600] {
        for path in [&fx.admin_key, &fx.signer_key, &fx.vault_key] {
            set_mode(path, mode);
        }
        assert!(fx.load().await.is_ok(), "mode {mode:o}");
        let _ = std::fs::remove_file(&fx.ledger);
    }
    for bad in [
        0o644, 0o604, 0o602, 0o601, 0o620, 0o610, 0o500, 0o700, 0o4600,
    ] {
        for which in 0..3 {
            let paths = [&fx.admin_key, &fx.signer_key, &fx.vault_key];
            for p in paths {
                set_mode(p, 0o600);
            }
            set_mode(paths[which], bad);
            assert_eq!(
                fx.load().await.unwrap_err(),
                SqldManagedError::KeyFileUnsafe,
                "mode {bad:o} on file #{which}"
            );
            fx.assert_untouched();
        }
    }
    for p in [&fx.admin_key, &fx.signer_key, &fx.vault_key] {
        set_mode(p, 0o600);
    }
    if !is_root() {
        eprintln!("SKIP owner/group sub-cases: needs root to chown");
        return;
    }
    // Owner estranho (nem root nem euid), legível pelo processo via o grupo
    // dele: o kernel deixa abrir e a checagem fstat de owner é alcançada.
    let my_gid = rustix::process::getegid().as_raw();
    set_mode(&fx.admin_key, 0o640);
    std::os::unix::fs::chown(&fx.admin_key, Some(4242), Some(my_gid)).unwrap();
    assert!(
        std::fs::File::open(&fx.admin_key).is_ok(),
        "fixture must be readable through the group so the owner check runs"
    );
    assert_eq!(
        fx.load().await.unwrap_err(),
        SqldManagedError::KeyFileUnsafe,
        "foreign owner reached through the group"
    );
    // Mesmo owner estranho, 0600: sem DAC o kernel nega a abertura
    // (KeyFileUnreadable); com DAC o processo abre e o owner é recusado
    // (KeyFileUnsafe). O ramo é decidido pelo próprio kernel, não por flag.
    set_mode(&fx.admin_key, 0o600);
    let kernel_denies = matches!(
        std::fs::File::open(&fx.admin_key),
        Err(ref e) if e.kind() == std::io::ErrorKind::PermissionDenied
    );
    eprintln!("foreign-owner 0600: kernel_denies_open={kernel_denies}");
    assert_eq!(
        fx.load().await.unwrap_err(),
        if kernel_denies {
            SqldManagedError::KeyFileUnreadable
        } else {
            SqldManagedError::KeyFileUnsafe
        }
    );
    std::os::unix::fs::chown(&fx.admin_key, Some(0), Some(my_gid)).unwrap();
    set_mode(&fx.admin_key, 0o600);
    // group-read com gid que o processo não tem.
    std::os::unix::fs::chown(&fx.signer_key, None, Some(4343)).unwrap();
    set_mode(&fx.signer_key, 0o640);
    assert_eq!(
        fx.load().await.unwrap_err(),
        SqldManagedError::KeyFileUnsafe
    );
    fx.assert_untouched();
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_projection_is_followed_and_the_target_is_checked() {
    let fx = Fx::new();
    let proj = fx.dir.path().join("proj");
    std::fs::create_dir_all(proj.join("..2026")).unwrap();
    write_file(
        &proj.join("..2026/admin.key"),
        b"ProjectedAdminKey\n",
        0o400,
    );
    std::os::unix::fs::symlink("..2026", proj.join("..data")).unwrap();
    std::os::unix::fs::symlink("..data/admin.key", proj.join("admin.key")).unwrap();
    let mut v = fx.vars();
    v.insert(ENV_ADMIN_KEY_FILE, proj.join("admin.key").into_os_string());
    assert!(fx.load_with(parse_map(&v).unwrap()).await.is_ok());
    let _ = std::fs::remove_file(&fx.ledger);
    // Alvo inseguro por trás do mesmo symlink: recusado.
    set_mode(&proj.join("..2026/admin.key"), 0o644);
    assert_eq!(
        fx.load_with(parse_map(&v).unwrap()).await.unwrap_err(),
        SqldManagedError::KeyFileUnsafe
    );
    fx.assert_untouched();
}

#[cfg(unix)]
#[tokio::test]
async fn keys_inside_guest_readable_roots_are_refused_for_all_three() {
    let fx = Fx::new();
    let copy = |name: &str, from: &Path| {
        let to = fx.work.join(name);
        std::fs::copy(from, &to).unwrap();
        set_mode(&to, 0o600);
        to
    };
    let admin = copy("admin.key", &fx.admin_key);
    let signer = copy("signer.key", &fx.signer_key);
    let vkey = copy("vault.key", &fx.vault_key);
    for which in 0..3 {
        let mut v = fx.vars();
        let mut evidence_path = fx.vault_key.clone();
        match which {
            0 => drop(v.insert(ENV_ADMIN_KEY_FILE, admin.clone().into_os_string())),
            1 => drop(v.insert(ENV_SIGNER_KEY_FILE, signer.clone().into_os_string())),
            _ => evidence_path = vkey.clone(),
        }
        let r = load_sqld_managed(
            parse_map(&v).unwrap(),
            fx.vault.clone(),
            SqldVaultMasterEvidence::new(Zeroizing::new(VAULT_BYTES), evidence_path),
            vec![fx.work.clone()],
        )
        .await;
        assert_eq!(
            r.unwrap_err(),
            SqldManagedError::KeyInGuestRoot,
            "key #{which}"
        );
        fx.assert_untouched();
    }
    // Symlink FORA da root apontando PARA dentro dela (alvo canônico).
    let out_link = fx.dir.path().join("keys/out.key");
    std::os::unix::fs::symlink(&admin, &out_link).unwrap();
    let mut v = fx.vars();
    v.insert(ENV_ADMIN_KEY_FILE, out_link.into_os_string());
    assert_eq!(
        fx.load_with(parse_map(&v).unwrap()).await.unwrap_err(),
        SqldManagedError::KeyInGuestRoot
    );
    // Symlink DENTRO da root apontando para fora (guest alcança o link).
    let in_link = fx.work.join("link.key");
    std::os::unix::fs::symlink(&fx.admin_key, &in_link).unwrap();
    let mut v = fx.vars();
    v.insert(ENV_ADMIN_KEY_FILE, in_link.into_os_string());
    assert_eq!(
        fx.load_with(parse_map(&v).unwrap()).await.unwrap_err(),
        SqldManagedError::KeyInGuestRoot
    );
    // Prefixo textual parecido NÃO é "dentro": work2 não está em work.
    let sibling = fx.dir.path().join("work2");
    std::fs::create_dir_all(&sibling).unwrap();
    let to = sibling.join("admin.key");
    std::fs::copy(&fx.admin_key, &to).unwrap();
    set_mode(&to, 0o600);
    let mut v = fx.vars();
    v.insert(ENV_ADMIN_KEY_FILE, to.into_os_string());
    assert!(fx.load_with(parse_map(&v).unwrap()).await.is_ok());
    fx.assert_untouched_except_ledger();
}

#[cfg(unix)]
impl Fx {
    fn assert_untouched_except_ledger(&self) {
        assert_eq!(self.admin.hits(), 0);
        assert_eq!(self.user.hits(), 0);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn invalid_roots_fail_closed_and_state_may_live_under_the_scan_root() {
    let fx = Fx::new();
    for roots in [Vec::new(), vec![fx.dir.path().join("does-not-exist")]] {
        let r = load_sqld_managed(fx.settings(), fx.vault.clone(), fx.evidence(), roots).await;
        assert_eq!(r.unwrap_err(), SqldManagedError::InvalidRoots);
        fx.assert_untouched();
    }
    // Ledger e backups dentro de `<root>/.edger`: permitido (precisam persistir).
    let mut fx = Fx::new();
    let state = fx.work.join(".edger");
    fx.ledger = state.join("ledger/registry.db");
    private_dir(&state.join("ledger"));
    fx.backups = state.join("backups");
    private_dir(&fx.backups);
    assert!(fx.load().await.is_ok());
    assert!(fx.ledger.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn origins_budget_and_private_dirs_are_validated_before_the_ledger_is_created() {
    // URLs: o cliente admin e o store de backup são as autoridades.
    let cases: Vec<(&'static str, String, SqldManagedError)> = vec![
        (
            ENV_ADMIN_URL,
            "http://user:pw@127.0.0.1:18080".into(),
            SqldManagedError::Admin(SqldAdminError::InvalidOrigin),
        ),
        (
            ENV_ADMIN_URL,
            "http://127.0.0.1:18080/path".into(),
            SqldManagedError::Admin(SqldAdminError::InvalidOrigin),
        ),
        (
            ENV_ADMIN_URL,
            "http://127.0.0.1:18080/?q=1".into(),
            SqldManagedError::Admin(SqldAdminError::InvalidOrigin),
        ),
        (
            ENV_ADMIN_URL,
            "ftp://127.0.0.1:18080".into(),
            SqldManagedError::Admin(SqldAdminError::InvalidOrigin),
        ),
        (
            ENV_USER_URL,
            "http://127.0.0.1:9090".into(),
            SqldManagedError::Backup(SqldBackupError::InvalidOrigin),
        ),
        (
            ENV_USER_URL,
            "https://127.0.0.1:18081".into(),
            SqldManagedError::Backup(SqldBackupError::InvalidOrigin),
        ),
        (
            ENV_USER_URL,
            "http://u@127.0.0.1:18081".into(),
            SqldManagedError::Backup(SqldBackupError::InvalidOrigin),
        ),
        (
            ENV_USER_URL,
            "http://127.0.0.1:18081/x".into(),
            SqldManagedError::Backup(SqldBackupError::InvalidOrigin),
        ),
    ];
    for (name, value, want) in cases {
        let fx = Fx::new();
        let mut v = fx.vars();
        v.insert(name, OsString::from(&value));
        assert_eq!(
            fx.load_with(parse_map(&v).unwrap()).await.unwrap_err(),
            want,
            "{value}"
        );
        fx.assert_untouched();
    }
    // Orçamento: abaixo do mínimo e desalinhado (decisão do registry).
    for budget in ["4096", "67108865", "0"] {
        let mut fx = Fx::new();
        fx.budget = budget.into();
        assert_eq!(
            fx.load().await.unwrap_err(),
            SqldManagedError::Registry(SqldRegistryError::InvalidBudget),
            "{budget}"
        );
        fx.assert_untouched();
    }
    // Diretório de backup frouxo (não afrouxamos o validador existente).
    let fx = Fx::new();
    set_mode(&fx.backups, 0o755);
    assert!(matches!(
        fx.load().await.unwrap_err(),
        SqldManagedError::Backup(_)
    ));
    fx.assert_untouched();
    // Pai do ledger frouxo.
    let fx = Fx::new();
    set_mode(fx.ledger.parent().unwrap(), 0o755);
    assert!(matches!(
        fx.load().await.unwrap_err(),
        SqldManagedError::Registry(_)
    ));
    assert!(!fx.ledger.exists());
    assert_eq!(fx.admin.hits() + fx.user.hits(), 0);
    // Loader não cria árvores: pai do ledger ausente continua ausente.
    let mut fx = Fx::new();
    let missing = fx.dir.path().join("nope/ledger");
    fx.ledger = missing.join("registry.db");
    assert!(matches!(
        fx.load().await.unwrap_err(),
        SqldManagedError::Registry(_)
    ));
    assert!(!missing.exists());
    let fx = Fx::new();
    let gone = fx.dir.path().join("no-backups");
    let mut v = fx.vars();
    v.insert(ENV_BACKUP_DIR, gone.clone().into_os_string());
    assert!(matches!(
        fx.load_with(parse_map(&v).unwrap()).await.unwrap_err(),
        SqldManagedError::Backup(_)
    ));
    assert!(!gone.exists());
    fx.assert_untouched();
}

#[cfg(unix)]
#[tokio::test]
async fn errors_and_debug_never_print_credential_markers() {
    let fx = Fx::new();
    write_file(&fx.signer_key, &SIGNER_BYTES, 0o644); // força erro
    let err = fx.load().await.unwrap_err();
    let text = format!("{err} {err:?}");
    for marker in [ADMIN_MARKER, "MASTER-MARKER", "127.0.0.1", "keys/"] {
        assert!(!text.contains(marker), "{text}");
    }
    write_file(&fx.signer_key, &SIGNER_BYTES, 0o600);
    // Chave com controle interno: erro estático, sem eco da chave.
    write_file(
        &fx.admin_key,
        format!("{ADMIN_MARKER}\x01x").as_bytes(),
        0o600,
    );
    let err = fx.load().await.unwrap_err();
    let text = format!("{err} {err:?} {:?}", fx.evidence());
    for marker in [ADMIN_MARKER, "MASTER-MARKER", "127.0.0.1", "keys/"] {
        assert!(!text.contains(marker), "{text}");
    }
    // Espaço interno: recusado cedo, erro estático sem eco da chave.
    write_file(&fx.admin_key, format!("{ADMIN_MARKER} x").as_bytes(), 0o600);
    let err = fx.load().await.unwrap_err();
    assert_eq!(err, SqldManagedError::AdminKeyInvalid);
    let text = format!("{err} {err:?} {:?}", fx.evidence());
    for marker in [ADMIN_MARKER, "MASTER-MARKER", "127.0.0.1", "keys/"] {
        assert!(!text.contains(marker), "{text}");
    }
}

// ───────────────────────── subprocesso UID/fsGroup ─────────────────────────

const CHILD_UID: u32 = 41001;
const FS_GROUP: u32 = 41002;
const CHILD_ENV: &str = "SQLD_MANAGED_TEST_CHILD_DIR";

/// Pai (root no WSL): monta uma projeção de Secret do Kubernetes
/// (`..data` -> `..<ts>`, symlinks, root:fsGroup 0440) e roda o MESMO binário
/// de teste sob `setpriv` com UID não-root, grupo suplementar fsGroup e SEM
/// capabilities (sem DAC override/read-search): só o modo/owner/grupo reais
/// decidem. Sem root ou sem `setpriv` o teste se declara pulado.
#[cfg(unix)]
#[test]
fn fsgroup_projection_works_for_a_non_root_uid_under_setpriv() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return; // o filho roda `child_fsgroup_projection`
    }
    if !is_root() || !Path::new("/usr/bin/setpriv").exists() {
        eprintln!("SKIP fsgroup subprocess: needs root and /usr/bin/setpriv");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    set_mode(&base, 0o755);
    let chown = |p: &Path, uid: u32, gid: u32| {
        std::os::unix::fs::chown(p, Some(uid), Some(gid)).unwrap();
    };
    // Projeção: root:fsGroup 0440 com symlinks `..data`.
    let secret = base.join("secret");
    std::fs::create_dir_all(secret.join("..2026_01")).unwrap();
    for d in [&secret, &secret.join("..2026_01")] {
        set_mode(d, 0o755);
    }
    for (name, body) in [
        ("admin.key", b"ProjectedAdminKey\n".to_vec()),
        ("signer.key", SIGNER_BYTES.to_vec()),
    ] {
        let real = secret.join("..2026_01").join(name);
        write_file(&real, &body, 0o440);
        chown(&real, 0, FS_GROUP);
        std::os::unix::fs::symlink(format!("..data/{name}"), secret.join(name)).unwrap();
    }
    std::os::unix::fs::symlink("..2026_01", secret.join("..data")).unwrap();
    // Negativos reais: grupo que o processo não tem, group-write, others.
    for (name, mode, gid) in [
        ("rootonly.key", 0o440, 0),
        ("gw.key", 0o460, FS_GROUP),
        ("other.key", 0o444, FS_GROUP),
    ] {
        let p = secret.join(name);
        write_file(&p, b"x-admin-key", mode);
        chown(&p, 0, gid);
    }
    // Do filho (UID não-root): vault key 0600/0400, vault.db, ledger, backups.
    let own = base.join("own");
    std::fs::create_dir_all(&own).unwrap();
    for (name, mode) in [("vault.key", 0o600), ("admin0400.key", 0o400)] {
        let p = own.join(name);
        write_file(
            &p,
            if name == "vault.key" {
                &VAULT_BYTES
            } else {
                b"OwnAdminKey"
            },
            mode,
        );
        chown(&p, CHILD_UID, CHILD_UID);
    }
    // Só agora o diretório vira do filho (o pai, sem DAC, não escreveria mais).
    chown(&own, CHILD_UID, CHILD_UID);
    set_mode(&own, 0o700);
    for d in ["ledger", "backups", "work"] {
        let p = base.join(d);
        std::fs::create_dir_all(&p).unwrap();
        chown(&p, CHILD_UID, CHILD_UID);
        set_mode(&p, if d == "work" { 0o755 } else { 0o700 });
    }
    // Binário legível pelo filho (o target do cargo pode ficar em /root).
    let exe = base.join("testexe");
    let src = std::env::current_exe().unwrap();
    if std::fs::hard_link(&src, &exe).is_err() {
        std::fs::copy(&src, &exe).unwrap();
    }
    set_mode(&exe, 0o755);
    let out = std::process::Command::new("/usr/bin/setpriv")
        .arg(format!("--reuid={CHILD_UID}"))
        .arg(format!("--regid={CHILD_UID}"))
        .arg(format!("--groups={FS_GROUP}"))
        .arg("--inh-caps=-all")
        .arg("--bounding-set=-all")
        .arg("--no-new-privs")
        .arg(&exe)
        .args([
            "--exact",
            "child_fsgroup_projection",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &base)
        .env(CHILD_ENV, &base)
        .current_dir(&base)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("CHILD-OK"),
        "child failed: status={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status
    );
    for marker in [
        ADMIN_MARKER,
        "MASTER-MARKER",
        "ProjectedAdminKey",
        "OwnAdminKey",
    ] {
        assert!(
            !stdout.contains(marker) && !stderr.contains(marker),
            "marker {marker} leaked"
        );
    }
}

/// Corpo do filho: só age quando o pai o lançou sob `setpriv`.
#[cfg(unix)]
#[tokio::test]
async fn child_fsgroup_projection() {
    let Some(base) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        CHILD_UID,
        "not the child uid"
    );
    assert_eq!(rustix::process::getuid().as_raw(), CHILD_UID);
    let groups: Vec<u32> = rustix::process::getgroups()
        .unwrap()
        .iter()
        .map(|g| g.as_raw())
        .collect();
    assert_eq!(
        groups,
        vec![FS_GROUP],
        "supplementary group is the fsGroup only"
    );
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    for line in status.lines().filter(|l| {
        l.starts_with("CapEff:") || l.starts_with("CapPrm:") || l.starts_with("CapBnd:")
    }) {
        assert!(
            line.ends_with("0000000000000000"),
            "capabilities must be empty (no DAC override): {line}"
        );
    }
    // Controle do ambiente: o kernel realmente nega ao grupo errado.
    assert!(std::fs::read(base.join("secret/rootonly.key")).is_err());

    let admin = Probe::start();
    let user = Probe::start();
    let vars = |admin_key: PathBuf, signer: PathBuf| {
        let mut m: HashMap<&'static str, OsString> = HashMap::new();
        m.insert(ENV_ADMIN_URL, OsString::from(admin.url()));
        m.insert(ENV_ADMIN_KEY_FILE, admin_key.into_os_string());
        m.insert(ENV_USER_URL, OsString::from(user.url()));
        m.insert(
            ENV_REGISTRY_DB,
            base.join("ledger/registry.db").into_os_string(),
        );
        m.insert(ENV_SIGNER_KEY_FILE, signer.into_os_string());
        m.insert(ENV_MANAGED_BUDGET_BYTES, OsString::from(BUDGET.to_string()));
        m.insert(ENV_BACKUP_DIR, base.join("backups").into_os_string());
        m
    };
    let vault = Arc::new(
        AppSecretVault::open(base.join("own/vault.db"), Zeroizing::new(VAULT_BYTES)).unwrap(),
    );
    let load = |settings: SqldManagedSettings| {
        let vault = vault.clone();
        let evidence =
            SqldVaultMasterEvidence::new(Zeroizing::new(VAULT_BYTES), base.join("own/vault.key"));
        let roots = vec![base.join("work")];
        async move { load_sqld_managed(settings, vault, evidence, roots).await }
    };
    let secret = base.join("secret");
    // Negativos: nenhum cria o ledger.
    for bad in ["rootonly.key", "gw.key", "other.key"] {
        let s = parse_map(&vars(secret.join(bad), secret.join("signer.key"))).unwrap();
        let err = load(s).await.unwrap_err();
        // 0440 root:root: o kernel nega o open ao grupo do processo (sem
        // DAC override); os outros abrem e o modo/owner os recusa.
        let want = if bad == "rootonly.key" {
            SqldManagedError::KeyFileUnreadable
        } else {
            SqldManagedError::KeyFileUnsafe
        };
        assert_eq!(err, want, "{bad}");
        assert!(
            !base.join("ledger/registry.db").exists(),
            "{bad} created ledger"
        );
    }
    // Projeção root:fsGroup 0440 via symlinks (admin e signer).
    let s = parse_map(&vars(secret.join("admin.key"), secret.join("signer.key"))).unwrap();
    let got = load(s).await.expect("fsGroup projection must load");
    assert!(matches!(got, SqldManagedRuntime::Enabled(_)));
    assert!(base.join("ledger/registry.db").exists());
    // 0400 do próprio UID (admin) também passa.
    let s = parse_map(&vars(
        base.join("own/admin0400.key"),
        secret.join("signer.key"),
    ))
    .unwrap();
    assert!(load(s).await.is_ok());
    assert_eq!(admin.hits() + user.hits(), 0);
    println!("CHILD-OK");
}
