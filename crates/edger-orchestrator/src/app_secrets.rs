//! Cofre de segredos de app (EDG-38).
//!
//! SQLite próprio, ao lado das API keys (`EDGER_APP_SECRETS_DB`, default
//! `<primeiro diretório de workers>/.edger/app-secrets.db`), com a mesma
//! forma do `api_keys.rs`: busy timeout, `VACUUM INTO` para cópia
//! consistente e erro `STORE_ERROR`.
//!
//! Cifra em repouso: ChaCha20-Poly1305 (nonce aleatório por gravação) com
//! AAD = `worker || 0x00 || name` — um ciphertext copiado para outro par
//! (worker, nome) não decifra. Chave mestra de 32 bytes lida de
//! `EDGER_SECRETS_KEY_FILE` (base64 ou 32 bytes crus), zeroizada em
//! memória. Sem a chave o cofre fica INDISPONÍVEL: as rotas respondem 503
//! `SECRETS_UNAVAILABLE` e nada é injetado. Uma chave que não decifra um
//! registro falha fechado: o segredo não é injetado e o erro (SEMPRE sem o
//! valor) vai para o log.
//!
//! O valor de um segredo nunca aparece em resposta, log ou evento
//! operacional — apenas o nome e os metadados.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine;
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use edger_core::{AppSecretResolver, CoreError};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use zeroize::Zeroizing;

/// Master key length in bytes (ChaCha20 key size).
const KEY_LEN: usize = 32;
/// ChaCha20-Poly1305 nonce size in bytes.
const NONCE_LEN: usize = 12;
/// Maximum secret value in bytes (16 KiB), enforced by the Admin API.
pub const MAX_SECRET_VALUE_BYTES: usize = 16 * 1024;
/// Per-worker storage cap (F5): a NEW secret name is rejected with
/// `SECRETS_LIMIT` once the worker holds this many (overwrite never hits it).
pub const MAX_SECRETS_PER_WORKER: usize = 64;
const SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Carrega a chave mestra do arquivo (`EDGER_SECRETS_KEY_FILE`). Caminho
/// cru: o arquivo INTEIRO, aceito só quando tem exatamente 32 bytes — sem
/// NENHUMA limpeza (todo byte é material de chave; F2). Caminho base64:
/// espaços em branco (quebras de linha incluídas) são removidos e o
/// resultado deve decodificar para exatamente 32 bytes. Qualquer outra
/// forma é erro — falha fechada no boot.
pub fn load_master_key_file(path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>, CoreError> {
    let raw = std::fs::read(path).map_err(|err| {
        CoreError::new(
            "SECRETS_KEY_INVALID",
            format!("cannot read the app secrets key file: {err}"),
        )
    })?;
    // Caminho cru: sem limpeza (F2) — exatamente 32 bytes, nada mais.
    if raw.len() == KEY_LEN {
        return Ok(Zeroizing::new(
            raw.try_into().expect("checked length: exactly 32 bytes"),
        ));
    }
    // Caminho base64: só aqui se removem os brancos, e o decodificado deve
    // ter exatamente 32 bytes.
    let trimmed: Vec<u8> = raw
        .iter()
        .filter(|byte| !byte.is_ascii_whitespace())
        .copied()
        .collect();
    match base64::engine::general_purpose::STANDARD.decode(&trimmed) {
        Ok(decoded) if decoded.len() == KEY_LEN => Ok(Zeroizing::new(
            decoded
                .try_into()
                .expect("checked length: exactly 32 bytes"),
        )),
        _ => Err(CoreError::new(
            "SECRETS_KEY_INVALID",
            "the app secrets key file must hold base64-encoded 32 bytes or exactly 32 raw bytes",
        )),
    }
}

/// Metadados de um segredo (a listagem NUNCA devolve o valor).
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppSecretMeta {
    pub name: String,
    /// Unix seconds (mesma forma do `createdAt` das API keys).
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

/// Store SQLite dos segredos de app. Mantém os bytes cifrados (nonce +
/// ciphertext); a cifra vive no [`AppSecretVault`].
/// Ponto cifrado do banco: (nonce, ciphertext).
type EncryptedSecret = (Vec<u8>, Vec<u8>);

pub struct AppSecretsStore {
    conn: Mutex<Connection>,
    /// Caminho do arquivo de banco (None em memória).
    path: Option<PathBuf>,
}

impl AppSecretsStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let mut conn = Connection::open(path).map_err(db_err)?;
        conn.busy_timeout(Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))
            .map_err(db_err)?;
        Self::init_schema(&mut conn)?;
        // (F6) o banco é criptografado, mas não pode estar legível por
        // outros usuários do host: 0600 no arquivo (e sidecars, se houver).
        restrict_vault_permissions(path);
        Ok(Self {
            conn: Mutex::new(conn),
            path: Some(path.to_path_buf()),
        })
    }

    pub fn in_memory() -> Result<Self, CoreError> {
        let mut conn = Connection::open_in_memory().map_err(db_err)?;
        Self::init_schema(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: None,
        })
    }

    /// Caminho do arquivo de banco, se o store persiste em arquivo.
    pub fn db_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Cópia consistente do banco (padrão `api_keys::export_to`): `VACUUM
    /// INTO` sob a conexão própria; o destino deve ainda não existir.
    pub fn export_to(&self, target: &Path) -> Result<(), CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            "VACUUM INTO ?1",
            params![target.to_string_lossy().into_owned()],
        )
        .map_err(db_err)?;
        Ok(())
    }

    fn init_schema(conn: &mut Connection) -> Result<(), CoreError> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS app_secrets (
                worker TEXT NOT NULL,
                name TEXT NOT NULL,
                nonce BLOB NOT NULL,
                ciphertext BLOB NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (worker, name)
            );
            "#,
        )
        .map_err(db_err)
    }

    /// Upsert com teto (F5): um NOME NOVO é recusado quando o worker já tem
    /// `max` segredos (`SECRETS_LIMIT`); sobrescrever um existente nunca
    /// esbarra no teto. A verificação do count e a escrita acontecem no MESMO
    /// lock da conexão — sem janela TOCTOU.
    fn upsert_capped(
        &self,
        worker: &str,
        name: &str,
        nonce: &[u8; NONCE_LEN],
        ciphertext: &[u8],
        now: i64,
        max: usize,
    ) -> Result<(), CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM app_secrets WHERE worker = ?1",
                params![worker],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM app_secrets WHERE worker = ?1 AND name = ?2)",
                params![worker, name],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        if !exists && count >= max as i64 {
            return Err(CoreError::new(
                "SECRETS_LIMIT",
                format!(
                    "worker {worker} already has the maximum of {max} app secrets; delete one first"
                ),
            ));
        }
        Self::execute_upsert(&conn, worker, name, nonce, ciphertext, now)
    }

    fn execute_upsert(
        conn: &Connection,
        worker: &str,
        name: &str,
        nonce: &[u8; NONCE_LEN],
        ciphertext: &[u8],
        now: i64,
    ) -> Result<(), CoreError> {
        conn.execute(
            "INSERT INTO app_secrets (worker, name, nonce, ciphertext, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
             ON CONFLICT (worker, name) DO UPDATE SET \
                 nonce = excluded.nonce, \
                 ciphertext = excluded.ciphertext, \
                 updated_at = excluded.updated_at",
            params![worker, name, nonce as &[u8], ciphertext as &[u8], now],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// Compare-and-set atômico: UMA transação SQLite `IMMEDIATE` sob o mutex
    /// da conexão cobre leitura, decifragem/comparação, contagem do teto e
    /// gravação + commit. O lock de escrita do SQLite é tomado no `BEGIN`,
    /// então outro handle (ou processo) no mesmo arquivo não escreve entre a
    /// comparação e o commit: ele espera (até o busy timeout) ou falha com
    /// `STORE_ERROR`, nunca intercala. `Ok(false)` = conflito e NADA mudou
    /// (a transação é descartada sem commit). Qualquer erro também faz
    /// rollback. `None` exige ausência; `Some` exige igualdade exata do
    /// plaintext atual (decifrado aqui; um row que não decifra é erro, sem
    /// overwrite). Com `expected = None` e row presente o resultado é
    /// `false` sem decifrar: a ausência já foi refutada.
    #[allow(clippy::too_many_arguments)]
    fn compare_and_set(
        &self,
        key: &Zeroizing<[u8; KEY_LEN]>,
        worker: &str,
        name: &str,
        expected: Option<&str>,
        nonce: &[u8; NONCE_LEN],
        ciphertext: &[u8],
        now: i64,
        max: usize,
    ) -> Result<bool, CoreError> {
        let mut conn = self.conn.lock().map_err(|_| lock_err())?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let current: Option<EncryptedSecret> = tx
            .query_row(
                "SELECT nonce, ciphertext FROM app_secrets WHERE worker = ?1 AND name = ?2",
                params![worker, name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        match (&current, expected) {
            (Some(_), None) | (None, Some(_)) => return Ok(false),
            (Some((stored_nonce, stored_ct)), Some(want)) => {
                let plain = decrypt(key, worker, name, stored_nonce, stored_ct)?;
                if plain.as_str() != want {
                    return Ok(false);
                }
            }
            (None, None) => {
                let count: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM app_secrets WHERE worker = ?1",
                        params![worker],
                        |row| row.get(0),
                    )
                    .map_err(db_err)?;
                if count >= max as i64 {
                    return Err(CoreError::new(
                        "SECRETS_LIMIT",
                        format!(
                            "worker {worker} already has the maximum of {max} app secrets; delete one first"
                        ),
                    ));
                }
            }
        }
        #[cfg(test)]
        run_cas_window_hook();
        Self::execute_upsert(&tx, worker, name, nonce, ciphertext, now)?;
        tx.commit().map_err(db_err)?;
        Ok(true)
    }

    fn read(&self, worker: &str, name: &str) -> Result<Option<EncryptedSecret>, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row: Option<EncryptedSecret> = conn
            .query_row(
                "SELECT nonce, ciphertext FROM app_secrets WHERE worker = ?1 AND name = ?2",
                params![worker, name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        Ok(row)
    }

    fn delete(&self, worker: &str, name: &str) -> Result<bool, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let removed = conn
            .execute(
                "DELETE FROM app_secrets WHERE worker = ?1 AND name = ?2",
                params![worker, name],
            )
            .map_err(db_err)?;
        Ok(removed > 0)
    }

    fn list(&self, worker: &str) -> Result<Vec<(String, i64, i64)>, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                "SELECT name, created_at, updated_at FROM app_secrets \
                 WHERE worker = ?1 ORDER BY name",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![worker], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (name, created_at, updated_at) = row.map_err(db_err)?;
            out.push((name, created_at, updated_at));
        }
        Ok(out)
    }
}

/// Cofre: store + chave mestra em memória (zeroizada no drop).
///
/// `key: None` = cofre INDISPONÍVEL (sem `EDGER_SECRETS_KEY_FILE`, ou o
/// arquivo inválido/ilegível no boot): leitura e escrita respondem falha
/// fechada, e o resolvedor injeta nada.
///
/// (EDG-52 06b) O material do cofre vive em `Arc<AppSecretVaultInner>`: o
/// resolvedor assíncrono clona o `Arc` para o job do pool de bloqueio SEM
/// copiar a chave mestra — o `Zeroizing` da chave só é liberado quando o
/// ÚLTIMO trabalho em voo (e o cofre) terminam. Sem `Clone`/`Debug`
/// públicos novos: o cofre continua derivando nenhuma das duas.
pub struct AppSecretVault {
    inner: Arc<AppSecretVaultInner>,
}

/// (EDG-52 06b) Posse compartilhada do cofre: o store serializado, a chave
/// mestra zeroizada e o slot de admissão do lookup offloadado. O `Arc`
/// leva tudo ao closure de `spawn_blocking` sem copiar material da chave.
struct AppSecretVaultInner {
    store: AppSecretsStore,
    key: Option<Zeroizing<[u8; KEY_LEN]>>,
    /// (EDG-52 06b) Slot de admissão POR COFRE, capacidade 1: a conexão já é
    /// serial atrás de um único `Mutex<Connection>`, então no máximo um
    /// lookup bloqueante fica em voo por cofre — sem fila de jobs bloqueantes
    /// sem limite. O await de admissão é cancelável (um caller derrubado ali
    /// não cria job), e o permit só é liberado quando o lookup/cifra
    /// realmente termina — o job em voo nunca é preemptado: o SQLite em voo
    /// roda até o fim e o permit sai com o job.
    lookup_slot: Arc<tokio::sync::Semaphore>,
}

impl AppSecretVaultInner {
    /// Lê e decifra um segredo. `None` = ausente OU falha fechada (chave
    /// errada/trocada): o erro SEM o valor vai para o log. O valor devolvido
    /// vive em `Zeroizing` (F4): é zeroizado no drop, salvo a recuperação
    /// explícita na fronteira com o resolvedor (`mem::take`).
    fn get(&self, worker: &str, name: &str) -> Option<Zeroizing<String>> {
        let key = self.key.as_ref()?;
        match self.store.read(worker, name) {
            Ok(Some((nonce, ciphertext))) => {
                match decrypt(key, worker, name, &nonce, &ciphertext) {
                    Ok(value) => Some(value),
                    Err(err) => {
                        tracing::error!(
                            worker = %worker,
                            secret = %name,
                            "app secret failed to decrypt (wrong or rotated master key?): {err} — failing closed"
                        );
                        None
                    }
                }
            }
            Ok(None) => None,
            Err(err) => {
                tracing::error!(worker = %worker, secret = %name, "app secret read failed: {err}");
                None
            }
        }
    }
}

impl AppSecretVault {
    /// Cofre disponível: store aberto em `path` + chave mestra em memória.
    pub fn open(path: impl AsRef<Path>, key: Zeroizing<[u8; KEY_LEN]>) -> Result<Self, CoreError> {
        Ok(Self::with_parts(AppSecretsStore::open(path)?, Some(key)))
    }

    /// Cofre indisponível: sem chave mestra (o store nem precisa abrir —
    /// nada pode ser lido nem gravado, e o boot segue servindo).
    pub fn unavailable() -> Self {
        Self::with_parts(
            AppSecretsStore::in_memory().expect("in-memory sqlite opens"),
            None,
        )
    }

    fn with_parts(store: AppSecretsStore, key: Option<Zeroizing<[u8; KEY_LEN]>>) -> Self {
        Self {
            inner: Arc::new(AppSecretVaultInner {
                store,
                key,
                lookup_slot: Arc::new(tokio::sync::Semaphore::new(1)),
            }),
        }
    }

    /// O cofre tem chave mestra (rotas respondem; injeção é possível).
    pub fn is_available(&self) -> bool {
        self.inner.key.is_some()
    }

    /// Caminho do arquivo de banco (None quando o store é em memória —
    /// ex.: cofre indisponível sem chave).
    pub fn db_path(&self) -> Option<&Path> {
        self.inner.store.db_path()
    }

    /// Cópia consistente do banco (`VACUUM INTO`) para o estado export;
    /// a export do estado usa a MESMA semântica da `api-keys.db`.
    pub fn export_to(&self, target: &Path) -> Result<(), CoreError> {
        self.inner.store.export_to(target)
    }

    /// Grava (ou sobrepõe) um segredo, cifrado com nonce novo. Teto por
    /// worker (F5): um NOME NOVO com `MAX_SECRETS_PER_WORKER` segredos já
    /// gravados falha com `SECRETS_LIMIT` (409 na API); sobrescrever um
    /// existente continua funcionando.
    pub fn set(&self, worker: &str, name: &str, value: &str) -> Result<(), CoreError> {
        let key = self.inner.key.as_ref().ok_or_else(unavailable_err)?;
        let (nonce, ciphertext) = encrypt(key, worker, name, value)?;
        self.inner.store.upsert_capped(
            worker,
            name,
            &nonce,
            &ciphertext,
            now_unix_seconds(),
            MAX_SECRETS_PER_WORKER,
        )
    }

    /// Compare-and-set atômico (host interno, SÍNCRONO): grava `value` só se
    /// o estado atual bater com `expected` (`None` = ausente; `Some` =
    /// plaintext idêntico; `Some("")` é um valor real). `true` = aplicado;
    /// `false` = conflito e nada mudou. Não adquire `lookup_slot` nem chama
    /// `resolve_secret`: o chamador offloada e mantém a admissão. Mesma cifra,
    /// AAD, nonce aleatório e teto por worker de `set`; `created_at` é
    /// preservado na substituição. `value` e `expected` têm no máximo
    /// `MAX_SECRET_VALUE_BYTES`. Cofre indisponível, row que não decifra ou
    /// falha de banco → erro SEM o plaintext e sem overwrite. Um `set`
    /// manual concorrente continua permitido e é serializado pelo SQLite.
    pub fn compare_and_set(
        &self,
        worker: &str,
        name: &str,
        expected: Option<&str>,
        value: &str,
    ) -> Result<bool, CoreError> {
        let key = self.inner.key.as_ref().ok_or_else(unavailable_err)?;
        if value.len() > MAX_SECRET_VALUE_BYTES
            || expected.is_some_and(|expected| expected.len() > MAX_SECRET_VALUE_BYTES)
        {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                format!("secret value must have at most {MAX_SECRET_VALUE_BYTES} bytes (16 KiB)"),
            ));
        }
        let (nonce, ciphertext) = encrypt(key, worker, name, value)?;
        self.inner.store.compare_and_set(
            key,
            worker,
            name,
            expected,
            &nonce,
            &ciphertext,
            now_unix_seconds(),
            MAX_SECRETS_PER_WORKER,
        )
    }

    /// Lê e decifra um segredo — a forma SINCRONA (APIs existentes):
    /// bloqueia a thread do chamador no mutex do store. O resolvedor
    /// assíncrono (`resolve_secret`) offloada a mesma operação.
    pub fn get(&self, worker: &str, name: &str) -> Option<Zeroizing<String>> {
        self.inner.get(worker, name)
    }

    /// Remove um segredo. `true` se existia.
    pub fn delete(&self, worker: &str, name: &str) -> Result<bool, CoreError> {
        if !self.is_available() {
            return Err(unavailable_err());
        }
        self.inner.store.delete(worker, name)
    }

    /// Lista nomes + datas (nunca o valor).
    pub fn list(&self, worker: &str) -> Result<Vec<AppSecretMeta>, CoreError> {
        if !self.is_available() {
            return Err(unavailable_err());
        }
        Ok(self
            .inner
            .store
            .list(worker)?
            .into_iter()
            .map(|(name, created_at, updated_at)| AppSecretMeta {
                name,
                created_at,
                updated_at,
            })
            .collect())
    }

    /// Store subjacente (caminho do banco, export `VACUUM INTO`).
    pub fn store(&self) -> &AppSecretsStore {
        &self.inner.store
    }
}

#[cfg(test)]
thread_local! {
    /// Hook de teste (por thread) executado DENTRO da transação do CAS,
    /// depois da comparação e antes da escrita.
    static CAS_WINDOW_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn run_cas_window_hook() {
    if let Some(hook) = CAS_WINDOW_HOOK.with(|slot| slot.borrow_mut().take()) {
        hook();
    }
}

/// (EDG-52 06b) Instrumentação privada de teste: conta os jobs bloqueantes
/// offloadados criados pelo `resolve_secret` (processo inteiro — os testes
/// de offload se serializam em `OFFLOAD_TEST_LOCK`) e força um job a
/// paniquear para provar o fail closed do join. Nunca existe no build de
/// produção.
#[cfg(test)]
static JOBS_SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// (EDG-52 06b) Força o PRÓXIMO job offloadado a paniquear (só em teste).
#[cfg(test)]
static FORCED_JOB_PANIC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
fn jobs_spawned_for_test() -> &'static std::sync::atomic::AtomicUsize {
    &JOBS_SPAWNED
}

#[cfg(test)]
fn reset_jobs_spawned_for_test() {
    JOBS_SPAWNED.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// `AppSecretResolver` para os dois backends (processo e WASM): cada nome
/// declarado é injetado só no env DO worker — no backend de processo, no
/// spawn; no backend WASM, no Store fresco de cada execução.
#[async_trait]
impl AppSecretResolver for AppSecretVault {
    async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
        // (EDG-52 06b) Offload: o lookup SQLite + a decifragem ChaCha20 nunca
        // rodam numa thread Tokio. Chave mestra ausente (cofre indisponível)
        // responde `None` SEM NENHUM I/O — antes da admissão.
        self.inner.key.as_ref()?;
        // Slot por cofre (capacidade 1 — a conexão já é serial). O await de
        // admissão é cancelável: um caller derrubado aqui libera nada e não
        // cria job bloqueante nenhum.
        let permit = match Arc::clone(&self.inner.lookup_slot).acquire_owned().await {
            Ok(permit) => permit,
            // O slot nunca é fechado; fail closed mesmo assim.
            Err(_) => return None,
        };
        #[cfg(test)]
        jobs_spawned_for_test().fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let inner = Arc::clone(&self.inner);
        let worker = worker.to_owned();
        let name = name.to_owned();
        // Cópias APENAS dos nomes para o log do join falho (nunca o valor);
        // os owned vão para o closure.
        let worker_log = worker.clone();
        let name_log = name.clone();
        // O permit MOVE para o job: é liberado só quando o lookup/cifra
        // realmente termina (ou o job paniqueia) — inclusive no cancel
        // do chamador deste await. O job em voo nunca é preemptado: o
        // SQLite em voo roda até o fim e o permit sai com o drop do job.
        let job = tokio::task::spawn_blocking(move || {
            let _slot = permit;
            #[cfg(test)]
            if FORCED_JOB_PANIC.swap(false, std::sync::atomic::Ordering::SeqCst) {
                panic!("forced app secret lookup job failure (test only)");
            }
            inner
                .get(&worker, &name)
                .map(|mut value| std::mem::take(&mut *value))
        });
        match job.await {
            Ok(value) => value,
            // Fail closed: um `JoinError` pode carregar o payload do panic
            // (que pode conter o segredo), então ele nunca é formatado,
            // logado nem devolvido — só a mensagem estática com os nomes.
            Err(_) => {
                tracing::error!(
                    worker = %worker_log,
                    secret = %name_log,
                    "app secret lookup task failed; failing closed without the value"
                );
                None
            }
        }
    }
}

fn unavailable_err() -> CoreError {
    CoreError::new(
        "SECRETS_UNAVAILABLE",
        "the app secrets vault is unavailable (no EDGER_SECRETS_KEY_FILE configured)",
    )
}

/// AAD = `worker || 0x00 || name` (o 0x00 impede ambiguidade entre pares).
fn aad(worker: &str, name: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(worker.len() + 1 + name.len());
    aad.extend_from_slice(worker.as_bytes());
    aad.push(0);
    aad.extend_from_slice(name.as_bytes());
    aad
}

/// (EDG-38 emenda 1 / F4, emenda 2 / R1) Cria o cipher a partir da chave
/// mestra zeroizada. `new_from_slice` converte por referência
/// (`<\&Key>::try_from`) — NENHUMA cópia owned da chave fica no stack; a
/// cópia que o cipher guarda é zeroizada no drop pela feature `zeroize`
/// do `chacha20poly1305`.
fn new_cipher(key: &Zeroizing<[u8; KEY_LEN]>) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new_from_slice(key.as_ref()).expect("master key: exactly 32 bytes")
}

/// Cifra com nonce aleatório; devolve `(nonce, ciphertext)`.
/// O valor em memória é zeroizado ao sair do escopo.
fn encrypt(
    key: &Zeroizing<[u8; KEY_LEN]>,
    worker: &str,
    name: &str,
    value: &str,
) -> Result<([u8; NONCE_LEN], Vec<u8>), CoreError> {
    let cipher = new_cipher(key);
    let nonce = Nonce::generate(); // MUST be unique per message (getrandom)
    let value_bytes = Zeroizing::new(value.as_bytes().to_vec());
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &value_bytes,
                aad: &aad(worker, name),
            },
        )
        .map_err(|err| CoreError::new("SECRETS_ENCRYPT_FAILED", err.to_string()))?;
    Ok((
        nonce
            .to_vec()
            .try_into()
            .expect("aead nonce size: 12 bytes"),
        ciphertext,
    ))
}

/// Decifra `ciphertext` (sem o prefixo de nonce). O valor decifrado vive em
/// `Zeroizing` (zeroizado no fim do escopo, salvo a recuperação explícita na
/// fronteira com o resolvedor).
fn decrypt(
    key: &Zeroizing<[u8; KEY_LEN]>,
    worker: &str,
    name: &str,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<String>, CoreError> {
    if nonce.len() != NONCE_LEN {
        return Err(CoreError::new(
            "SECRETS_DECRYPT_FAILED",
            format!(
                "stored nonce has {} bytes (expected {NONCE_LEN})",
                nonce.len()
            ),
        ));
    }
    let cipher = new_cipher(key);
    let nonce = Nonce::try_from(nonce).expect("nonce length checked above");
    let value_bytes = Zeroizing::new(
        cipher
            .decrypt(&nonce, Payload { msg: ciphertext, aad: &aad(worker, name) })
            .map_err(|_| {
                CoreError::new(
                    "SECRETS_DECRYPT_FAILED",
                    "app secret does not decrypt under the configured master key (AAD or key mismatch)",
                )
            })?,
    );
    std::str::from_utf8(&value_bytes)
        .map(|value| Zeroizing::new(value.to_string()))
        .map_err(|_| {
            CoreError::new(
                "SECRETS_DECRYPT_FAILED",
                "app secret decrypted to non-UTF-8 content",
            )
        })
}

fn db_err(err: rusqlite::Error) -> CoreError {
    CoreError::new("STORE_ERROR", err.to_string())
}

fn lock_err() -> CoreError {
    CoreError::new("STORE_ERROR", "app secrets store lock poisoned")
}

/// (F6) `app-secrets.db` com 0600 (e os sidecars `-journal`/`-wal`/`-shm`
/// do SQLite, se já existirem): o conteúdo é cifrado, mas o arquivo não
/// pode ser legível por outros usuários do host. Falha de permissão é
/// tolerada (o boot não pode morrer por isso).
#[cfg(unix)]
fn restrict_vault_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::Permissions::from_mode(0o600);
    let _ = std::fs::set_permissions(path, mode.clone());
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut file_name = path
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_default();
        file_name.push(suffix);
        let sidecar = path.with_file_name(file_name);
        if sidecar.exists() {
            let _ = std::fs::set_permissions(&sidecar, mode.clone());
        }
    }
}

/// Fora de Unix as permissões não são modeladas assim: no-op.
#[cfg(not(unix))]
fn restrict_vault_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn key(bytes: u8) -> Zeroizing<[u8; KEY_LEN]> {
        Zeroizing::new([bytes; KEY_LEN])
    }

    fn temp_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app-secrets.db");
        (dir, path)
    }

    /// (F4) `get` agora devolve `Zeroizing<String>`; os testes comparam com
    /// o valor em texto claro recuperando sem copiar (o `zeroize` 1.9.0
    /// não tem `into_inner`: `mem::take` move o valor para fora e o
    /// wrapper fica com um `String` vazio, zeroizado no drop).
    fn get_str(vault: &AppSecretVault, worker: &str, name: &str) -> Option<String> {
        vault
            .get(worker, name)
            .map(|mut value| std::mem::take(&mut *value))
    }

    #[test]
    fn cipher_roundtrips_a_value() {
        let (_dir, path) = temp_db();
        let vault = AppSecretVault::open(&path, key(1)).unwrap();
        vault
            .set("pinar", "BYOK_MASTER_KEY", "value-abc-123")
            .unwrap();
        assert_eq!(
            get_str(&vault, "pinar", "BYOK_MASTER_KEY").as_deref(),
            Some("value-abc-123")
        );
        // Rotação: novo PUT substitui, nonce novo, `created_at` preservado.
        let (created_before, updated_before) = vault
            .list("pinar")
            .unwrap()
            .pop()
            .map(|meta| (meta.created_at, meta.updated_at))
            .unwrap();
        vault
            .set("pinar", "BYOK_MASTER_KEY", "rotated-999")
            .unwrap();
        let meta = vault.list("pinar").unwrap().pop().unwrap();
        assert_eq!(meta.name, "BYOK_MASTER_KEY");
        assert_eq!(
            meta.created_at, created_before,
            "created_at is preserved on overwrite"
        );
        assert!(meta.updated_at >= updated_before);
        assert_eq!(
            get_str(&vault, "pinar", "BYOK_MASTER_KEY").as_deref(),
            Some("rotated-999")
        );
    }

    #[test]
    fn a_swapped_ciphertext_does_not_decrypt_across_names_or_workers() {
        let (_dir, path) = temp_db();
        let vault = AppSecretVault::open(&path, key(1)).unwrap();
        vault.set("pinar", "SECRET_A", "value-a").unwrap();
        vault.set("pinar", "SECRET_B", "value-b").unwrap();
        vault
            .set("other-worker", "SECRET_A", "value-other")
            .unwrap();

        // Copia o ciphertext (pinar, SECRET_A) para os outros pares: o AAD
        // (worker || 0x00 || name) deve recusar cada um deles.
        let (nonce, ciphertext) = vault.store().read("pinar", "SECRET_A").unwrap().unwrap();
        let nonce: [u8; NONCE_LEN] = nonce.try_into().unwrap();
        let store = vault.store();
        store
            .upsert_capped(
                "pinar",
                "SECRET_B",
                &nonce,
                &ciphertext,
                123,
                MAX_SECRETS_PER_WORKER,
            )
            .unwrap();
        store
            .upsert_capped(
                "other-worker",
                "SECRET_A",
                &nonce,
                &ciphertext,
                123,
                MAX_SECRETS_PER_WORKER,
            )
            .unwrap();

        // Falha fechada: não decifra (AAD mismatch) — e o valor original de
        // cada par continua o que era antes da cópia, para os pares que
        // ainda têm o próprio ciphertext. Aqui os dois pares foram
        // SOBRESCRITOS, então ambos devem falhar.
        assert_eq!(
            get_str(&vault, "pinar", "SECRET_B"),
            None,
            "name swap must not decrypt"
        );
        assert_eq!(
            get_str(&vault, "other-worker", "SECRET_A"),
            None,
            "worker swap must not decrypt"
        );
    }

    #[test]
    fn a_wrong_key_fails_closed_without_the_value() {
        let (_dir, path) = temp_db();
        let writer = AppSecretVault::open(&path, key(1)).unwrap();
        writer
            .set("pinar", "BYOK_MASTER_KEY", "secret-value-zz9X")
            .unwrap();

        // Chave errada/trocada: lê o registro, tenta decifrar, falha
        // fechado — `None`, nunca o valor.
        let reader = AppSecretVault::open(&path, key(2)).unwrap();
        assert_eq!(get_str(&reader, "pinar", "BYOK_MASTER_KEY"), None);
        // Escrita com a chave nova funciona (novo nonce/ciphertext) e o
        // registro volta a decifrar com ela.
        reader
            .set("pinar", "BYOK_MASTER_KEY", "after-rotation")
            .unwrap();
        assert_eq!(
            get_str(&reader, "pinar", "BYOK_MASTER_KEY").as_deref(),
            Some("after-rotation")
        );
        // E a chave antiga deixa de decifrar o registro novo.
        assert_eq!(get_str(&writer, "pinar", "BYOK_MASTER_KEY"), None);
    }

    #[test]
    fn the_master_key_file_accepts_base64_and_raw_and_rejects_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let raw32 = [7u8; KEY_LEN];
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw32);

        let p = dir.path().join("b64.key");
        std::fs::write(&p, format!("{b64}\n")).unwrap();
        assert_eq!(&load_master_key_file(&p).unwrap()[..], &raw32[..]);

        // Base64 com quebras de linha (estilo PEM) é tolerado.
        let wrapped = format!("{}\n{}\n", &b64[..22], &b64[22..]);
        let p = dir.path().join("wrapped.key");
        std::fs::write(&p, &wrapped).unwrap();
        assert_eq!(&load_master_key_file(&p).unwrap()[..], &raw32[..]);

        let p = dir.path().join("raw.key");
        std::fs::write(&p, raw32).unwrap();
        assert_eq!(&load_master_key_file(&p).unwrap()[..], &raw32[..]);

        // (F2) crua com bytes de controle/espaço EM MEIO aos 32 bytes: é o
        // arquivo inteiro, SEM LIMPEZA — o resultado é igual aos bytes
        // originais (um 0x0A no meio NÃO é quebra de linha a remover).
        let mut raw_with_noise = [7u8; KEY_LEN];
        raw_with_noise[5] = 0x0A; // '\n' dentro dos 32 bytes
        raw_with_noise[17] = 0x20; // espaço dentro dos 32 bytes
        let p = dir.path().join("raw-noise.key");
        std::fs::write(&p, raw_with_noise).unwrap();
        assert_eq!(
            &load_master_key_file(&p).unwrap()[..],
            &raw_with_noise[..],
            "a raw key is the whole file, uncleaned"
        );

        // (F2) base64 com quebra de linha FINAL: aceita.
        let p = dir.path().join("b64-trailing-newline.key");
        std::fs::write(&p, format!("{b64}\n")).unwrap();
        assert_eq!(&load_master_key_file(&p).unwrap()[..], &raw32[..]);

        // (F2) 33 bytes crus: recusados (nem base64 de 32 nem 32 crus).
        let p = dir.path().join("long.key");
        std::fs::write(&p, [9u8; 33]).unwrap();
        assert!(load_master_key_file(&p).is_err());

        // 31 bytes crus: nem base64 de 32 nem 32 crus.
        let p = dir.path().join("short.key");
        std::fs::write(&p, [9u8; 31]).unwrap();
        assert!(load_master_key_file(&p).is_err());

        // Base64 que decodifica para 31 bytes.
        let p = dir.path().join("short-b64.key");
        std::fs::write(
            &p,
            base64::engine::general_purpose::STANDARD.encode([9u8; 31]),
        )
        .unwrap();
        assert!(load_master_key_file(&p).is_err());

        // (F2) base64 que decodifica para 33 bytes.
        let p = dir.path().join("long-b64.key");
        std::fs::write(
            &p,
            base64::engine::general_purpose::STANDARD.encode([9u8; 33]),
        )
        .unwrap();
        assert!(load_master_key_file(&p).is_err());

        // Arquivo ausente.
        assert!(load_master_key_file(&dir.path().join("missing.key")).is_err());
    }

    #[test]
    fn an_unavailable_vault_fails_closed_on_every_operation() {
        let vault = AppSecretVault::unavailable();
        assert!(!vault.is_available());
        assert!(vault.set("w", "S", "v").is_err());
        assert!(vault.get("w", "S").is_none());
        assert!(vault.delete("w", "S").is_err());
        assert!(vault.list("w").is_err());
        let err = vault.set("w", "S", "v").unwrap_err();
        assert_eq!(err.code, "SECRETS_UNAVAILABLE");
    }

    #[test]
    fn list_returns_names_and_dates_never_the_value_and_delete_removes() {
        let (_dir, path) = temp_db();
        let vault = AppSecretVault::open(&path, key(1)).unwrap();
        vault.set("pinar", "ZZ_LAST", "v3").unwrap();
        vault.set("pinar", "AA_FIRST", "v1").unwrap();
        vault.set("pinar", "MM_MID", "v2").unwrap();

        let metas = vault.list("pinar").unwrap();
        assert_eq!(
            metas
                .iter()
                .map(|meta| meta.name.as_str())
                .collect::<Vec<_>>(),
            vec!["AA_FIRST", "MM_MID", "ZZ_LAST"],
            "the list is sorted by name"
        );
        for meta in &metas {
            assert!(meta.created_at > 0);
            assert!(meta.updated_at >= meta.created_at);
        }
        // Outro worker não vê nada do `pinar`.
        assert!(vault.list("other").unwrap().is_empty());

        assert!(vault.delete("pinar", "MM_MID").unwrap());
        assert!(
            !vault.delete("pinar", "MM_MID").unwrap(),
            "a second delete finds nothing"
        );
        assert_eq!(vault.list("pinar").unwrap().len(), 2);
        assert!(get_str(&vault, "pinar", "MM_MID").is_none());
    }

    #[test]
    fn the_db_path_is_reported_for_persisted_stores() {
        let (_dir, path) = temp_db();
        let vault = AppSecretVault::open(&path, key(1)).unwrap();
        assert_eq!(vault.store().db_path(), Some(path.as_path()));
        let memory = AppSecretVault::unavailable();
        assert_eq!(memory.store().db_path(), None);
    }

    #[test]
    fn export_to_produces_a_consistent_copy() {
        let (_dir, path) = temp_db();
        let vault = AppSecretVault::open(&path, key(1)).unwrap();
        vault.set("pinar", "K", "v").unwrap();
        let copy = _dir.path().join("copy.db");
        vault.store().export_to(&copy).unwrap();
        let reopened = AppSecretVault::open(&copy, key(1)).unwrap();
        assert_eq!(get_str(&reopened, "pinar", "K").as_deref(), Some("v"));
    }

    /// (F5) teto por worker: o 65º NOME NOVO falha com `SECRETS_LIMIT`;
    /// sobrescrever um existente continua funcionando.
    #[test]
    fn the_per_worker_cap_rejects_the_65th_new_name_but_allows_overwrite() {
        let (_dir, path) = temp_db();
        let vault = AppSecretVault::open(&path, key(1)).unwrap();
        for i in 0..64 {
            let name = format!("K{i:03}");
            vault.set("pinar", &name, &format!("v{i}")).unwrap();
        }
        assert_eq!(vault.list("pinar").unwrap().len(), 64);
        // O 65º nome novo: 409 na API; aqui o erro `SECRETS_LIMIT`.
        let err = vault.set("pinar", "K64", "v64").unwrap_err();
        assert_eq!(err.code, "SECRETS_LIMIT");
        // Sobrescrever um existente continua funcionando (não esbarra no teto).
        vault.set("pinar", "K000", "v000-again").unwrap();
        assert_eq!(
            get_str(&vault, "pinar", "K000").as_deref(),
            Some("v000-again")
        );
        assert_eq!(vault.list("pinar").unwrap().len(), 64);
    }

    // ---- compare_and_set (EDG-54 14v) ----

    /// Linha crua (nonce, ciphertext, created_at, updated_at) lida por outra conexão.
    type RawRow = (Vec<u8>, Vec<u8>, i64, i64);

    fn raw_row(path: &Path, worker: &str, name: &str) -> Option<RawRow> {
        let conn = Connection::open(path).unwrap();
        conn.query_row(
            "SELECT nonce, ciphertext, created_at, updated_at FROM app_secrets \
             WHERE worker = ?1 AND name = ?2",
            params![worker, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .unwrap()
    }

    fn raw_exec(path: &Path, sql: &str) {
        Connection::open(path).unwrap().execute_batch(sql).unwrap();
    }

    fn short_busy(vault: &AppSecretVault) {
        vault
            .store()
            .conn
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(50))
            .unwrap();
    }

    #[test]
    fn cas_insert_replace_mismatch_empty_and_isolation() {
        let (_dir, path) = temp_db();
        let v = AppSecretVault::open(&path, key(1)).unwrap();
        // ausente + expected None: insere.
        assert!(v.compare_and_set("w", "T", None, "a").unwrap());
        assert_eq!(get_str(&v, "w", "T").as_deref(), Some("a"));
        // repetição com None: recusada, preserva o valor (manual).
        let first = raw_row(&path, "w", "T").unwrap();
        assert!(!v.compare_and_set("w", "T", None, "b").unwrap());
        assert_eq!(raw_row(&path, "w", "T").unwrap(), first);
        // mismatch: recusado e intacto.
        assert!(!v.compare_and_set("w", "T", Some("zzz"), "b").unwrap());
        assert_eq!(raw_row(&path, "w", "T").unwrap(), first);
        // ausente + Some: false e nada criado.
        assert!(!v.compare_and_set("w", "OTHER", Some("a"), "b").unwrap());
        assert!(raw_row(&path, "w", "OTHER").is_none());
        // substituição com expected correto: nonce/ciphertext novos.
        assert!(v.compare_and_set("w", "T", Some("a"), "b").unwrap());
        assert_eq!(get_str(&v, "w", "T").as_deref(), Some("b"));
        let after = raw_row(&path, "w", "T").unwrap();
        assert_ne!(after.0, first.0, "nonce must be fresh");
        assert_ne!(after.1, first.1);
        assert_eq!(after.2, first.2, "created_at preserved");
        // vazio é valor real, não ausência.
        assert!(v.compare_and_set("w", "E", None, "").unwrap());
        assert_eq!(get_str(&v, "w", "E").as_deref(), Some(""));
        assert!(!v.compare_and_set("w", "E", None, "x").unwrap());
        assert!(!v.compare_and_set("w", "E", Some("x"), "y").unwrap());
        assert!(v.compare_and_set("w", "E", Some(""), "y").unwrap());
        // worker/name distintos isolados.
        assert!(v.compare_and_set("w2", "T", None, "iso").unwrap());
        assert!(v.compare_and_set("w", "T2", None, "iso2").unwrap());
        assert_eq!(get_str(&v, "w", "T").as_deref(), Some("b"));
        assert!(!v.compare_and_set("w2", "T", Some("b"), "x").unwrap());
        assert_eq!(get_str(&v, "w2", "T").as_deref(), Some("iso"));
    }

    #[test]
    fn cas_conflict_changes_nothing_and_success_keeps_created_at() {
        let (_dir, path) = temp_db();
        let v = AppSecretVault::open(&path, key(1)).unwrap();
        v.set("w", "T", "a").unwrap();
        // envelhece o row para qualquer mudança de timestamp aparecer.
        raw_exec(
            &path,
            "UPDATE app_secrets SET created_at = 10, updated_at = 20",
        );
        let before = raw_row(&path, "w", "T");
        assert!(!v.compare_and_set("w", "T", Some("nope"), "b").unwrap());
        assert!(!v.compare_and_set("w", "T", None, "b").unwrap());
        assert_eq!(raw_row(&path, "w", "T"), before);
        assert!(v.compare_and_set("w", "T", Some("a"), "b").unwrap());
        let after = raw_row(&path, "w", "T").unwrap();
        assert_eq!(after.2, 10, "created_at preserved");
        assert!(after.3 > 20, "updated_at refreshed");
    }

    #[test]
    fn cas_enforces_the_cap_on_insert_but_not_on_replace() {
        let (_dir, path) = temp_db();
        let v = AppSecretVault::open(&path, key(1)).unwrap();
        for i in 0..64 {
            assert!(v
                .compare_and_set("w", &format!("K{i:03}"), None, "v")
                .unwrap());
        }
        let err = v.compare_and_set("w", "K064", None, "v").unwrap_err();
        assert_eq!(err.code, "SECRETS_LIMIT");
        assert!(raw_row(&path, "w", "K064").is_none());
        assert_eq!(v.list("w").unwrap().len(), 64);
        assert!(v.compare_and_set("w", "K000", Some("v"), "new").unwrap());
        assert_eq!(get_str(&v, "w", "K000").as_deref(), Some("new"));
        assert_eq!(v.list("w").unwrap().len(), 64);
        // outro worker não sofre o teto.
        assert!(v.compare_and_set("w2", "K064", None, "v").unwrap());
    }

    #[test]
    fn cas_size_cap_applies_to_value_and_expected_without_touching_state() {
        let (_dir, path) = temp_db();
        let v = AppSecretVault::open(&path, key(1)).unwrap();
        v.set("w", "T", "a").unwrap();
        let before = raw_row(&path, "w", "T");
        let big = "x".repeat(MAX_SECRET_VALUE_BYTES + 1);
        let ok = "x".repeat(MAX_SECRET_VALUE_BYTES);
        let err = v.compare_and_set("w", "T", Some("a"), &big).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        let err = v.compare_and_set("w", "T", Some(&big), "b").unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        assert_eq!(raw_row(&path, "w", "T"), before);
        assert!(v.compare_and_set("w", "T", Some("a"), &ok).unwrap());
    }

    #[test]
    fn cas_fails_closed_when_unavailable_wrong_master_or_corrupt_row() {
        let u = AppSecretVault::unavailable();
        let err = u
            .compare_and_set("w", "T", None, "secret-plain")
            .unwrap_err();
        assert_eq!(err.code, "SECRETS_UNAVAILABLE");
        assert!(!err.message.contains("secret-plain"));

        let (_dir, path) = temp_db();
        let v = AppSecretVault::open(&path, key(1)).unwrap();
        v.set("w", "T", "old-plain").unwrap();
        let before = raw_row(&path, "w", "T");
        // master errada: não decifra, sem overwrite, sem plaintext no erro.
        let wrong = AppSecretVault::open(&path, key(2)).unwrap();
        let err = wrong
            .compare_and_set("w", "T", Some("old-plain"), "new-plain")
            .unwrap_err();
        assert_eq!(err.code, "SECRETS_DECRYPT_FAILED");
        assert!(!err.message.contains("old-plain") && !err.message.contains("new-plain"));
        assert_eq!(raw_row(&path, "w", "T"), before);
        // ciphertext corrompido.
        raw_exec(&path, "UPDATE app_secrets SET ciphertext = zeroblob(8)");
        let before = raw_row(&path, "w", "T");
        let err = v
            .compare_and_set("w", "T", Some("old-plain"), "new-plain")
            .unwrap_err();
        assert_eq!(err.code, "SECRETS_DECRYPT_FAILED");
        assert!(!err.message.contains("new-plain"));
        assert_eq!(raw_row(&path, "w", "T"), before);
        // nonce com tamanho inválido.
        raw_exec(&path, "UPDATE app_secrets SET nonce = zeroblob(3)");
        let before = raw_row(&path, "w", "T");
        assert!(v.compare_and_set("w", "T", Some("old-plain"), "n").is_err());
        assert_eq!(raw_row(&path, "w", "T"), before);
    }

    #[test]
    fn cas_rolls_back_when_the_write_fails_inside_the_transaction() {
        let (_dir, path) = temp_db();
        let v = AppSecretVault::open(&path, key(1)).unwrap();
        v.set("w", "T", "a").unwrap();
        let before = raw_row(&path, "w", "T");
        raw_exec(
            &path,
            "CREATE TRIGGER no_update BEFORE UPDATE ON app_secrets \
             BEGIN SELECT RAISE(ABORT, 'blocked'); END; \
             CREATE TRIGGER no_insert BEFORE INSERT ON app_secrets \
             BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
        );
        let err = v.compare_and_set("w", "T", Some("a"), "b").unwrap_err();
        assert_eq!(err.code, "STORE_ERROR");
        assert!(v.compare_and_set("w", "N", None, "b").is_err());
        assert_eq!(raw_row(&path, "w", "T"), before);
        assert!(raw_row(&path, "w", "N").is_none());
        // a conexão não ficou com transação aberta: remove os triggers e repete.
        raw_exec(&path, "DROP TRIGGER no_update; DROP TRIGGER no_insert;");
        assert!(v.compare_and_set("w", "T", Some("a"), "b").unwrap());
    }

    /// Interleaving DENTRO da janela: com a comparação feita e a escrita
    /// ainda pendente, outro handle (set manual e outro CAS) não consegue
    /// escrever (lock IMMEDIATE tomado no BEGIN) — recebe erro de banco
    /// ocupado em vez de intercalar.
    #[test]
    fn cas_holds_the_write_lock_between_compare_and_commit() {
        let (_dir, path) = temp_db();
        let a = AppSecretVault::open(&path, key(1)).unwrap();
        let b = Arc::new(AppSecretVault::open(&path, key(1)).unwrap());
        short_busy(&b);
        a.set("w", "T", "a").unwrap();
        let inside: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (b2, inside2) = (b.clone(), inside.clone());
        CAS_WINDOW_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let mut log = inside2.lock().unwrap();
                log.push(match b2.set("w", "T", "manual") {
                    Ok(()) => "set:ok".into(),
                    Err(e) => format!("set:{}", e.code),
                });
                log.push(match b2.compare_and_set("w", "T", Some("a"), "rival") {
                    Ok(won) => format!("cas:{won}"),
                    Err(e) => format!("cas:{}", e.code),
                });
            }));
        });
        assert!(a.compare_and_set("w", "T", Some("a"), "managed").unwrap());
        assert_eq!(
            *inside.lock().unwrap(),
            vec!["set:STORE_ERROR".to_string(), "cas:STORE_ERROR".to_string()]
        );
        assert_eq!(get_str(&a, "w", "T").as_deref(), Some("managed"));
        // o mesmo CAS rival, depois do commit, vê o estado real: false.
        assert!(!b.compare_and_set("w", "T", Some("a"), "rival").unwrap());
        // um PUT manual posterior continua autorizado.
        b.set("w", "T", "manual").unwrap();
        assert_eq!(get_str(&a, "w", "T").as_deref(), Some("manual"));
    }

    /// Manual set entre a leitura e o CAS (handles distintos): o CAS vê o
    /// estado real, devolve false e preserva o manual.
    #[test]
    fn cas_after_a_manual_set_between_read_and_cas_returns_false_and_keeps_manual() {
        let (_dir, path) = temp_db();
        let a = AppSecretVault::open(&path, key(1)).unwrap();
        let b = AppSecretVault::open(&path, key(1)).unwrap();
        a.set("w", "T", "old").unwrap();
        let read = get_str(&a, "w", "T").unwrap();
        b.set("w", "T", "manual").unwrap();
        let before = raw_row(&path, "w", "T");
        assert!(!a.compare_and_set("w", "T", Some(&read), "managed").unwrap());
        assert_eq!(raw_row(&path, "w", "T"), before);
        assert_eq!(get_str(&a, "w", "T").as_deref(), Some("manual"));
    }

    /// Dois handles independentes (threads, mesmo arquivo, master igual)
    /// disputam a mesma expectativa liberados por barrier: exatamente um
    /// ganha. Threads do mesmo processo — não é prova multiprocesso.
    #[test]
    fn two_independent_handles_race_the_same_expectation_exactly_one_wins() {
        for round in 0..20 {
            let (_dir, path) = temp_db();
            let seed = AppSecretVault::open(&path, key(1)).unwrap();
            // rounds pares disputam a inserção (None), ímpares a substituição.
            let expected = if round % 2 == 0 {
                None
            } else {
                seed.set("w", "T", "base").unwrap();
                Some("base")
            };
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let handles: Vec<_> = (0..2)
                .map(|i| {
                    let path = path.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        let vault = AppSecretVault::open(&path, key(1)).unwrap();
                        barrier.wait();
                        let won = vault
                            .compare_and_set("w", "T", expected, &format!("writer{i}"))
                            .unwrap();
                        (i, won)
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let winners: Vec<_> = results.iter().filter(|(_, won)| *won).collect();
            assert_eq!(winners.len(), 1, "round {round}: {results:?}");
            assert_eq!(
                get_str(&seed, "w", "T").unwrap(),
                format!("writer{}", winners[0].0)
            );
        }
    }

    /// (F6) o arquivo do cofre é criado com 0600 (Unix): nem legível por
    /// outros usuários do host, mesmo cifrado.
    #[cfg(unix)]
    #[test]
    fn the_vault_db_file_is_created_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = temp_db();
        let _vault = AppSecretVault::open(&path, key(1)).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "app-secrets.db must be 0600, got {mode:o}"
        );
    }

    /// (EDG-52 06b) Serializa os testes de offload no contador de jobs de
    /// processo inteiro (os testes de unidade correm em threads paralelas).
    static OFFLOAD_TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    async fn offload_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
        OFFLOAD_TEST_LOCK.lock().await
    }

    /// (EDG-52 06b, TDD seed) While the REAL connection is fenced by a test
    /// thread, the resolution must keep the Tokio `current_thread` runtime
    /// responsive: the offloaded lookup waits on the fenced
    /// `Mutex<Connection>` on the blocking pool, so a concurrent timer keeps
    /// firing (the runtime heartbeat stays alive). The synchronous
    /// implementation Red-fails this: the lookup blocks the runtime thread on
    /// the same mutex and the timer cannot tick during the fenced window.
    #[tokio::test(flavor = "current_thread")]
    async fn the_resolver_keeps_the_tokio_thread_responsive_on_a_fenced_real_connection() {
        let _guard = offload_test_lock().await;
        let (_dir, path) = temp_db();
        let vault = Arc::new(AppSecretVault::open(&path, key(1)).unwrap());
        vault.set("w", "APP_SECRET", "canary-value-1").unwrap();

        // A test thread fences the REAL connection via an RAII guard; the
        // hold is BOUNDED on every path (release send, sender drop on panic,
        // or the 10 s deadline), so the guard is always released and the
        // thread always joins without an infinite wait.
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let fenced = Arc::clone(&vault);
        let holder = std::thread::spawn(move || {
            let fence = fenced.store().conn.lock().unwrap();
            held_tx.send(()).ok();
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
            drop(fence);
        });
        tokio::task::spawn_blocking(move || {
            held_rx.recv_timeout(std::time::Duration::from_secs(10))
        })
        .await
        .unwrap()
        .expect("the holder thread must hold the real connection");

        let ticks = Arc::new(AtomicUsize::new(0));
        {
            let counter = Arc::clone(&ticks);
            let heartbeat = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
                for _ in 0..40 {
                    interval.tick().await;
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
            tokio::task::yield_now().await;
            let baseline = ticks.load(Ordering::SeqCst);
            {
                let lookup = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    vault.resolve_secret("w", "APP_SECRET"),
                );
                tokio::pin!(lookup);
                tokio::select! {
                    res = lookup.as_mut() => {
                        // Awaiting answer inside the fenced window: the
                        // runtime thread was NOT blocked on the fence.
                        release_tx.send(()).ok();
                        let value = res
                            .expect("the lookup must answer once the fence releases")
                            .unwrap();
                        let during = ticks.load(Ordering::SeqCst) - baseline;
                        assert_eq!(value.as_str(), "canary-value-1");
                        assert!(
                            during >= 2,
                            "the Tokio current_thread runtime must keep ticking while the lookup waits on the fenced connection (during={during})"
                        );
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(800)) => {
                        // The runtime stayed responsive during the fenced
                        // window: release the connection and take the result.
                        release_tx.send(()).ok();
                        let value = lookup
                            .await
                            .expect("the lookup must finish once the fence releases")
                            .unwrap();
                        let during = ticks.load(Ordering::SeqCst) - baseline;
                        assert_eq!(value.as_str(), "canary-value-1");
                        assert!(
                            during >= 2,
                            "the Tokio current_thread runtime must keep ticking while the lookup waits on the fenced connection (during={during})"
                        );
                    }
                }
            }
            let _ = heartbeat.await;
        }
        release_tx.send(()).ok();
        holder.join().expect("the holder thread must join");
    }

    /// (EDG-52 06b) The in-flight offloaded lookup holds the per-vault slot
    /// until it really finishes — NOT when the caller cancels — a second
    /// caller is blocked at admission (a cancellable awaiter) and spawns no
    /// new job, and once the connection is released the slot is back and the
    /// vault resolves again. The holder thread is released/joined on every
    /// path with a bounded hold (release send, sender drop on panic, 10 s
    /// deadline) — no infinite wait.
    #[tokio::test(flavor = "current_thread")]
    async fn the_in_flight_lookup_holds_the_slot_until_the_real_release_and_admission_is_cancellable(
    ) {
        let _guard = offload_test_lock().await;
        reset_jobs_spawned_for_test();

        let (_dir, path) = temp_db();
        let vault = Arc::new(AppSecretVault::open(&path, key(1)).unwrap());
        vault.set("w", "APP_SECRET", "canary-value-1").unwrap();

        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let fenced = Arc::clone(&vault);
        let holder = std::thread::spawn(move || {
            let fence = fenced.store().conn.lock().unwrap();
            held_tx.send(()).ok();
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
            drop(fence);
        });
        tokio::task::spawn_blocking(move || {
            held_rx.recv_timeout(std::time::Duration::from_secs(10))
        })
        .await
        .unwrap()
        .expect("the holder thread must hold the real connection");

        // The in-flight lookup and its caller cancel live in this scope:
        // ending the scope drops the caller while the blocking job keeps
        // running.
        {
            let lookup = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                vault.resolve_secret("w", "APP_SECRET"),
            );
            tokio::pin!(lookup);
            tokio::select! {
                _res = lookup.as_mut() => {
                    panic!("the lookup must still be in flight while the connection is fenced")
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(600)) => {}
            }
            // The offload spawned exactly one blocking job, holding the
            // per-vault slot (capacity 1).
            assert_eq!(
                jobs_spawned_for_test().load(Ordering::SeqCst),
                1,
                "the offload must have spawned exactly one blocking job"
            );
            assert!(
                vault.inner.lookup_slot.try_acquire().is_err(),
                "the slot must be held by the in-flight lookup"
            );
            // A second caller is blocked at ADMISSION: its timeout returns
            // (cancellable awaiter) and it spawns no new job.
            let second = tokio::time::timeout(
                std::time::Duration::from_millis(300),
                vault.resolve_secret("w", "APP_SECRET"),
            )
            .await;
            assert!(
                second.is_err(),
                "the second caller must time out at admission while the slot is held"
            );
            assert_eq!(
                jobs_spawned_for_test().load(Ordering::SeqCst),
                1,
                "a caller cancelled at admission must not spawn a new blocking job"
            );
        } // <- caller cancel: the pinned lookup future is dropped here.

        // The slot stays held by the in-flight job: the caller cancel does
        // not release it (the job is never preempted; the lookup keeps
        // waiting on the fenced connection).
        assert!(
            vault.inner.lookup_slot.try_acquire().is_err(),
            "the slot must stay held after the caller cancel, until the lookup really finishes"
        );

        // Release the fence: the in-flight job finishes (real SQLite read +
        // decrypt; its result is dropped with the cancelled caller) and
        // frees the slot.
        release_tx.send(()).ok();
        let freed = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match vault.inner.lookup_slot.try_acquire() {
                Ok(permit) => {
                    // Slot probe: hand the permit back.
                    drop(permit);
                    break;
                }
                Err(_) if std::time::Instant::now() < freed => {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
                Err(_) => {
                    panic!("the slot must free once the in-flight lookup finishes");
                }
            }
        }

        // The slot is usable again: a fresh lookup is admitted and resolves
        // the value from the real vault; only the fresh job was spawned.
        let value = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            vault.resolve_secret("w", "APP_SECRET"),
        )
        .await
        .expect("the fresh lookup must be admitted and finish")
        .unwrap();
        assert_eq!(value.as_str(), "canary-value-1");
        assert_eq!(
            jobs_spawned_for_test().load(Ordering::SeqCst),
            2,
            "only the in-flight and the fresh lookup may have spawned a job"
        );

        holder.join().expect("the holder thread must join");
    }

    /// (EDG-52 06b) A missing master key answers `None` WITHOUT ANY I/O:
    /// no blocking job is spawned (no SQLite access on the blocking pool)
    /// and the admission slot is never touched.
    #[tokio::test(flavor = "current_thread")]
    async fn an_unavailable_vault_resolves_to_none_without_io_and_without_a_job() {
        let _guard = offload_test_lock().await;
        reset_jobs_spawned_for_test();

        let vault = AppSecretVault::unavailable();
        let value = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            vault.resolve_secret("w", "APP_SECRET"),
        )
        .await
        .expect("an unavailable vault must answer at once");
        assert!(value.is_none(), "an unavailable vault fails closed to None");
        assert_eq!(
            jobs_spawned_for_test().load(Ordering::SeqCst),
            0,
            "no blocking job (no I/O) for a missing master key"
        );
        // No admission either: the vault's slot is untouched.
        let permit = vault
            .inner
            .lookup_slot
            .try_acquire()
            .expect("nothing may have admitted against the slot");
        drop(permit);
    }

    /// (EDG-52 06b) A panicked offloaded job fails closed: the caller gets
    /// `None` (the panic payload never surfaces — only the static message
    /// with the names is logged), and the job drop released the slot, so the
    /// next lookup is admitted and resolves the real value.
    #[tokio::test(flavor = "current_thread")]
    async fn a_panicked_lookup_job_fails_closed_and_frees_the_slot() {
        let _guard = offload_test_lock().await;
        reset_jobs_spawned_for_test();

        let (_dir, path) = temp_db();
        let vault = AppSecretVault::open(&path, key(1)).unwrap();
        vault.set("w", "APP_SECRET", "canary-value-1").unwrap();
        FORCED_JOB_PANIC.store(true, Ordering::SeqCst);

        let value = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            vault.resolve_secret("w", "APP_SECRET"),
        )
        .await
        .expect("a panicked job must not hang the join");
        assert!(
            value.is_none(),
            "a panicked lookup must fail closed without the value"
        );

        // The panicked job's drop released the slot: the next lookup is
        // admitted and resolves the real value from the real vault.
        let value = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            vault.resolve_secret("w", "APP_SECRET"),
        )
        .await
        .expect("the slot must be free after the panicked job")
        .unwrap();
        assert_eq!(value.as_str(), "canary-value-1");
        assert_eq!(
            jobs_spawned_for_test().load(Ordering::SeqCst),
            2,
            "one job per lookup, panic included"
        );
    }
}
