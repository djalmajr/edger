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
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine;
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use edger_core::{AppSecretResolver, CoreError};
use rusqlite::{params, Connection, OptionalExtension};
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
pub struct AppSecretVault {
    store: AppSecretsStore,
    key: Option<Zeroizing<[u8; KEY_LEN]>>,
}

impl AppSecretVault {
    /// Cofre disponível: store aberto em `path` + chave mestra em memória.
    pub fn open(path: impl AsRef<Path>, key: Zeroizing<[u8; KEY_LEN]>) -> Result<Self, CoreError> {
        let store = AppSecretsStore::open(path)?;
        Ok(Self {
            store,
            key: Some(key),
        })
    }

    /// Cofre indisponível: sem chave mestra (o store nem precisa abrir —
    /// nada pode ser lido nem gravado, e o boot segue servindo).
    pub fn unavailable() -> Self {
        Self {
            store: AppSecretsStore::in_memory().expect("in-memory sqlite opens"),
            key: None,
        }
    }

    /// O cofre tem chave mestra (rotas respondem; injeção é possível).
    pub fn is_available(&self) -> bool {
        self.key.is_some()
    }

    /// Caminho do arquivo de banco (None quando o store é em memória —
    /// ex.: cofre indisponível sem chave).
    pub fn db_path(&self) -> Option<&Path> {
        self.store.db_path()
    }

    /// Cópia consistente do banco (`VACUUM INTO`) para o estado export;
    /// a export do estado usa a MESMA semântica da `api-keys.db`.
    pub fn export_to(&self, target: &Path) -> Result<(), CoreError> {
        self.store.export_to(target)
    }

    /// Grava (ou sobrepõe) um segredo, cifrado com nonce novo. Teto por
    /// worker (F5): um NOME NOVO com `MAX_SECRETS_PER_WORKER` segredos já
    /// gravados falha com `SECRETS_LIMIT` (409 na API); sobrescrever um
    /// existente continua funcionando.
    pub fn set(&self, worker: &str, name: &str, value: &str) -> Result<(), CoreError> {
        let key = self.key.as_ref().ok_or_else(unavailable_err)?;
        let (nonce, ciphertext) = encrypt(key, worker, name, value)?;
        self.store.upsert_capped(
            worker,
            name,
            &nonce,
            &ciphertext,
            now_unix_seconds(),
            MAX_SECRETS_PER_WORKER,
        )
    }

    /// Lê e decifra um segredo. `None` = ausente OU falha fechada (chave
    /// errada/trocada): o erro SEM o valor vai para o log. O valor devolvido
    /// vive em `Zeroizing` (F4): é zeroizado no drop, salvo a recuperação
    /// explícita na fronteira com o resolvedor (`mem::take`).
    pub fn get(&self, worker: &str, name: &str) -> Option<Zeroizing<String>> {
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

    /// Remove um segredo. `true` se existia.
    pub fn delete(&self, worker: &str, name: &str) -> Result<bool, CoreError> {
        if !self.is_available() {
            return Err(unavailable_err());
        }
        self.store.delete(worker, name)
    }

    /// Lista nomes + datas (nunca o valor).
    pub fn list(&self, worker: &str) -> Result<Vec<AppSecretMeta>, CoreError> {
        if !self.is_available() {
            return Err(unavailable_err());
        }
        Ok(self
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
        &self.store
    }
}

/// `AppSecretResolver` para o backend de processo: o resolvedor injeta no
/// env DO processo do worker que declara o nome — e só dele.
#[async_trait]
impl AppSecretResolver for AppSecretVault {
    async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
        // A fronteira com o resolvedor exige um `String` comum (o valor vai
        // para o env do processo filho — a cópia é aceitável, e o core não
        // depende de `zeroize`). O `zeroize` 1.9.0 não tem `into_inner`:
        // recupera o valor sem copiar com `mem::take` (o wrapper fica
        // com um `String` vazio, zeroizado no drop).
        self.get(worker, name)
            .map(|mut value| std::mem::take(&mut *value))
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
}
