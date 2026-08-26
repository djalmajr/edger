//! API keys persistentes do control plane: SQLite no PVC + service com cache.
//!
//! Ressuscita o `SqliteApiKeyStore` do `edger-ext-auth` removido no Epic 17.A
//! (a objeção era persistência em K8s; hoje o PVC de user workers é padrão) e
//! o moderniza: escopo por worker, `last_used_at`, e revogação TERMINAL
//! (`revoked_at`, sem reativação — padrão tenancit) com delete exigindo
//! revogação prévia. O hash é o mesmo contrato da época
//! (`sha256("edger-auth-v1:" || raw)`), hex.
//!
//! Runtime no padrão apigate: lookup por hash servido de um cache em memória
//! com TTL, limpo inteiro em qualquer mutação; `last_used` com throttle para
//! o hot path não virar write amplification.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use edger_core::{
    validate_key_grant, AdminApiKeyCreatedResponse, AdminApiKeyInfo, ApiKeyPrincipal, ApiKeyStore,
    CoreError, CreateApiKeyRequest, NewApiKey,
};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Prefixo discriminante (padrão `lwc_` do Studio): é ele que decide se um
/// Bearer toca o SQLite — JWTs de OIDC nunca pagam o lookup.
pub const API_KEY_PREFIX: &str = "egk_";

const CACHE_TTL: Duration = Duration::from_secs(60);
const TOUCH_THROTTLE: Duration = Duration::from_secs(60);

pub struct SqliteApiKeyStore {
    conn: Mutex<Connection>,
}

impl SqliteApiKeyStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let conn = Connection::open(path).map_err(db_err)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn in_memory() -> Result<Self, CoreError> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), CoreError> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS api_keys (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                key_hash TEXT NOT NULL UNIQUE,
                key_prefix TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'operator',
                permissions TEXT NOT NULL,
                namespaces TEXT NOT NULL,
                workers TEXT NOT NULL DEFAULT '["*"]',
                expires_at INTEGER,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                last_used_at INTEGER,
                revoked_at INTEGER
            );
            "#,
        )
        .map_err(db_err)
    }

    fn hash_key(raw_key: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"edger-auth-v1:");
        hasher.update(raw_key.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn key_prefix(raw_key: &str) -> String {
        raw_key.chars().take(12).collect()
    }
}

type KeyRow = (
    u64,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<i64>,
    i64,
    Option<i64>,
    Option<i64>,
);

const KEY_COLUMNS: &str = "id, name, key_prefix, role, permissions, namespaces, workers, \
     expires_at, created_at, last_used_at, revoked_at";

fn map_key_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<KeyRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn info_from_row(row: KeyRow) -> Result<AdminApiKeyInfo, CoreError> {
    let (
        id,
        name,
        key_prefix,
        role,
        permissions_json,
        namespaces_json,
        workers_json,
        expires_at,
        created_at,
        last_used_at,
        revoked_at,
    ) = row;
    Ok(AdminApiKeyInfo {
        id,
        name,
        key_prefix,
        role,
        permissions: serde_json::from_str(&permissions_json).map_err(json_err)?,
        namespaces: serde_json::from_str(&namespaces_json).map_err(json_err)?,
        workers: serde_json::from_str(&workers_json).map_err(json_err)?,
        created_at: created_at as u64,
        last_used_at: last_used_at.map(|v| v as u64),
        expires_at: expires_at.map(|v| v as u64),
        revoked_at: revoked_at.map(|v| v as u64),
    })
}

impl ApiKeyStore for SqliteApiKeyStore {
    fn lookup_by_key(&self, raw_key: &str) -> Result<Option<ApiKeyPrincipal>, CoreError> {
        let hash = Self::hash_key(raw_key);
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {KEY_COLUMNS} FROM api_keys WHERE key_hash = ?1 AND revoked_at IS NULL"
            ))
            .map_err(db_err)?;
        let mut rows = stmt.query_map(params![hash], map_key_row).map_err(db_err)?;
        let Some(row) = rows.next() else {
            return Ok(None);
        };
        let info = info_from_row(row.map_err(db_err)?)?;
        if let Some(exp) = info.expires_at {
            if exp < now_epoch()? {
                return Ok(None);
            }
        }
        Ok(Some(ApiKeyPrincipal {
            id: info.id,
            name: info.name,
            key_prefix: info.key_prefix,
            role: info.role,
            permissions: info.permissions,
            namespaces: info.namespaces,
            workers: info.workers,
            is_root: false,
            expires_at: info.expires_at,
        }))
    }

    fn list_keys(&self) -> Result<Vec<AdminApiKeyInfo>, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {KEY_COLUMNS} FROM api_keys ORDER BY id ASC"
            ))
            .map_err(db_err)?;
        let rows = stmt.query_map([], map_key_row).map_err(db_err)?;
        let mut keys = Vec::new();
        for row in rows {
            keys.push(info_from_row(row.map_err(db_err)?)?);
        }
        Ok(keys)
    }

    fn get_key(&self, id: u64) -> Result<Option<AdminApiKeyInfo>, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(&format!("SELECT {KEY_COLUMNS} FROM api_keys WHERE id = ?1"))
            .map_err(db_err)?;
        let mut rows = stmt.query_map(params![id], map_key_row).map_err(db_err)?;
        match rows.next() {
            Some(row) => Ok(Some(info_from_row(row.map_err(db_err)?)?)),
            None => Ok(None),
        }
    }

    fn insert_key(&self, new_key: NewApiKey<'_>) -> Result<u64, CoreError> {
        let hash = Self::hash_key(new_key.raw_key);
        let prefix = Self::key_prefix(new_key.raw_key);
        let permissions_json = serde_json::to_string(new_key.permissions).map_err(json_err)?;
        let namespaces_json = serde_json::to_string(new_key.namespaces).map_err(json_err)?;
        let workers_json = serde_json::to_string(new_key.workers).map_err(json_err)?;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            "INSERT INTO api_keys (name, key_hash, key_prefix, role, permissions, namespaces, workers, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                new_key.name,
                hash,
                prefix,
                new_key.role,
                permissions_json,
                namespaces_json,
                workers_json,
                new_key.expires_at.map(|v| v as i64)
            ],
        )
        .map_err(db_err)?;
        Ok(conn.last_insert_rowid() as u64)
    }

    fn revoke_key(&self, id: u64) -> Result<bool, CoreError> {
        let now = now_epoch()? as i64;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        // Idempotente: revogar de novo não reescreve o timestamp original.
        let changed = conn
            .execute(
                "UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![id, now],
            )
            .map_err(db_err)?;
        if changed > 0 {
            return Ok(true);
        }
        let mut stmt = conn
            .prepare("SELECT 1 FROM api_keys WHERE id = ?1")
            .map_err(db_err)?;
        stmt.exists(params![id]).map_err(db_err)
    }

    fn delete_key(&self, id: u64) -> Result<bool, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare("SELECT revoked_at FROM api_keys WHERE id = ?1")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![id], |row| row.get::<_, Option<i64>>(0))
            .map_err(db_err)?;
        let Some(revoked_at) = rows.next() else {
            return Ok(false);
        };
        if revoked_at.map_err(db_err)?.is_none() {
            return Err(CoreError::new(
                "KEY_NOT_REVOKED",
                "revoke the key before deleting it",
            ));
        }
        conn.execute("DELETE FROM api_keys WHERE id = ?1", params![id])
            .map_err(db_err)?;
        Ok(true)
    }

    fn touch_last_used(&self, id: u64) -> Result<(), CoreError> {
        let now = now_epoch()? as i64;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            "UPDATE api_keys SET last_used_at = ?2 WHERE id = ?1",
            params![id, now],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

/// O serviço que o resto do orchestrator enxerga: autenticação com cache e o
/// ciclo de gestão com a anti-escalada aplicada ANTES do insert.
pub struct ApiKeyService {
    store: SqliteApiKeyStore,
    cache: RwLock<HashMap<String, (ApiKeyPrincipal, Instant)>>,
    last_touch: RwLock<HashMap<u64, Instant>>,
}

impl ApiKeyService {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        Ok(Self::from_store(SqliteApiKeyStore::open(path)?))
    }

    pub fn in_memory() -> Result<Self, CoreError> {
        Ok(Self::from_store(SqliteApiKeyStore::in_memory()?))
    }

    fn from_store(store: SqliteApiKeyStore) -> Self {
        Self {
            store,
            cache: RwLock::new(HashMap::new()),
            last_touch: RwLock::new(HashMap::new()),
        }
    }

    /// Principal vivo para a credencial, ou None (inexistente/revogada/
    /// expirada — indistinguíveis de propósito: 401 é 401).
    pub fn authenticate(&self, raw_key: &str) -> Option<ApiKeyPrincipal> {
        let cache_key = SqliteApiKeyStore::hash_key(raw_key);
        if let Ok(cache) = self.cache.read() {
            if let Some((principal, at)) = cache.get(&cache_key) {
                if at.elapsed() < CACHE_TTL {
                    let principal = principal.clone();
                    drop(cache);
                    self.touch(principal.id);
                    return Some(principal);
                }
            }
        }
        let principal = match self.store.lookup_by_key(raw_key) {
            Ok(principal) => principal?,
            Err(err) => {
                tracing::warn!(code = %err.code, "api key lookup failed: {}", err.message);
                return None;
            }
        };
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(cache_key, (principal.clone(), Instant::now()));
        }
        self.touch(principal.id);
        Some(principal)
    }

    fn touch(&self, id: u64) {
        if let Ok(mut touched) = self.last_touch.write() {
            match touched.get(&id) {
                Some(at) if at.elapsed() < TOUCH_THROTTLE => return,
                _ => {
                    touched.insert(id, Instant::now());
                }
            }
        }
        if let Err(err) = self.store.touch_last_used(id) {
            tracing::warn!(code = %err.code, "api key touch failed: {}", err.message);
        }
    }

    fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.write() {
            cache.clear();
        }
    }

    pub fn create(
        &self,
        creator: &ApiKeyPrincipal,
        request: CreateApiKeyRequest,
    ) -> Result<AdminApiKeyCreatedResponse, CoreError> {
        let name = request.name.trim();
        if name.is_empty() || name.len() > 80 {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                "name must be 1-80 characters",
            ));
        }
        validate_key_grant(
            creator,
            &request.permissions,
            &request.namespaces,
            &request.workers,
        )?;
        if let Some(expires_at) = request.expires_at {
            if expires_at <= now_epoch()? {
                return Err(CoreError::new(
                    "VALIDATION_ERROR",
                    "expiresAt must be in the future",
                ));
            }
        }
        let raw_key = generate_api_key();
        let role = request.role.as_deref().unwrap_or("operator");
        let id = self.store.insert_key(NewApiKey {
            raw_key: &raw_key,
            name,
            role,
            permissions: &request.permissions,
            namespaces: &request.namespaces,
            workers: &request.workers,
            expires_at: request.expires_at,
        })?;
        let key = self
            .store
            .get_key(id)?
            .ok_or_else(|| CoreError::new("STORE_ERROR", "inserted key vanished"))?;
        Ok(AdminApiKeyCreatedResponse { key, raw_key })
    }

    pub fn list(&self) -> Result<Vec<AdminApiKeyInfo>, CoreError> {
        self.store.list_keys()
    }

    /// `false` = id inexistente. Revogação é terminal e derruba o cache.
    pub fn revoke(&self, id: u64) -> Result<bool, CoreError> {
        let found = self.store.revoke_key(id)?;
        self.clear_cache();
        Ok(found)
    }

    /// `false` = id inexistente; key viva devolve `KEY_NOT_REVOKED`.
    pub fn delete(&self, id: u64) -> Result<bool, CoreError> {
        let found = self.store.delete_key(id)?;
        self.clear_cache();
        Ok(found)
    }
}

/// `egk_` + 64 hex (~244 bits de dois UUIDv4). O prefixo de exibição são os
/// 12 primeiros chars — `egk_` + 8 hex.
fn generate_api_key() -> String {
    format!(
        "{API_KEY_PREFIX}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn now_epoch() -> Result<u64, CoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|err| CoreError::new("STORE_ERROR", err.to_string()))
}

fn db_err(err: rusqlite::Error) -> CoreError {
    CoreError::new("STORE_ERROR", err.to_string())
}

fn json_err(err: serde_json::Error) -> CoreError {
    CoreError::new("STORE_ERROR", err.to_string())
}

fn lock_err() -> CoreError {
    CoreError::new("STORE_ERROR", "sqlite connection lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use edger_core::root_principal;

    fn request(name: &str, permissions: &[&str]) -> CreateApiKeyRequest {
        CreateApiKeyRequest {
            name: name.into(),
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
            namespaces: vec!["*".into()],
            workers: vec!["*".into()],
            expires_at: None,
            role: None,
        }
    }

    #[test]
    fn create_authenticate_roundtrip_with_prefix_and_scopes() {
        let service = ApiKeyService::in_memory().unwrap();
        let mut req = request("studio", &["workers:read", "workers:invoke"]);
        req.workers = vec!["p-abc*".into()];
        let created = service.create(&root_principal(), req).unwrap();

        assert!(created.raw_key.starts_with(API_KEY_PREFIX));
        assert_eq!(created.key.key_prefix.len(), 12);
        assert!(created.raw_key.starts_with(&created.key.key_prefix));

        let principal = service.authenticate(&created.raw_key).unwrap();
        assert_eq!(principal.name, "studio");
        assert!(!principal.is_root);
        assert_eq!(principal.workers, vec!["p-abc*".to_string()]);
        assert!(service.authenticate("egk_nope").is_none());
    }

    #[test]
    fn hash_contract_is_stable() {
        // O contrato herdado do edger-ext-auth: mudar o sal/formato invalida
        // TODAS as keys persistidas de uma instância.
        assert_eq!(
            SqliteApiKeyStore::hash_key("abc"),
            "75fe4f6b75102946867aa82c39d36ab6153adde7bc57d2c9866d409e21174307"
        );
    }

    #[test]
    fn revoke_is_terminal_and_delete_requires_it() {
        let service = ApiKeyService::in_memory().unwrap();
        let created = service
            .create(&root_principal(), request("ci", &["workers:read"]))
            .unwrap();
        let id = created.key.id;

        // Key viva não deleta.
        let err = service.delete(id).unwrap_err();
        assert_eq!(err.code, "KEY_NOT_REVOKED");

        assert!(service.revoke(id).unwrap());
        // Revogada não autentica mais (cache foi limpo junto).
        assert!(service.authenticate(&created.raw_key).is_none());
        // Revoke de novo é idempotente e continua achando o id.
        assert!(service.revoke(id).unwrap());
        assert!(service.delete(id).unwrap());
        assert!(!service.revoke(id).unwrap());
        assert!(!service.delete(id).unwrap());
    }

    #[test]
    fn expired_key_does_not_authenticate() {
        let service = ApiKeyService::in_memory().unwrap();
        let mut req = request("curta", &["workers:read"]);
        req.expires_at = Some(now_epoch().unwrap() + 1000);
        let created = service.create(&root_principal(), req).unwrap();
        // Expira "por fora" para não depender de relógio no teste.
        service
            .store
            .conn
            .lock()
            .unwrap()
            .execute("UPDATE api_keys SET expires_at = 1", [])
            .unwrap();
        service.clear_cache();
        assert!(service.authenticate(&created.raw_key).is_none());
    }

    #[test]
    fn create_enforces_anti_escalation_via_grant() {
        let service = ApiKeyService::in_memory().unwrap();
        let creator = service
            .create(
                &root_principal(),
                request("gerente", &["keys:manage", "workers:read"]),
            )
            .unwrap();
        let manager = service.authenticate(&creator.raw_key).unwrap();

        let err = service
            .create(&manager, request("escalada", &["workers:install"]))
            .unwrap_err();
        assert_eq!(err.code, "KEY_GRANT_DENIED");

        let ok = service
            .create(&manager, request("leitura", &["workers:read"]))
            .unwrap();
        assert!(ok.raw_key.starts_with(API_KEY_PREFIX));
    }

    #[test]
    fn expires_at_in_the_past_is_rejected() {
        let service = ApiKeyService::in_memory().unwrap();
        let mut req = request("velha", &["workers:read"]);
        req.expires_at = Some(1);
        let err = service.create(&root_principal(), req).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
    }
}
