//! API key persistence — SQLite primary store (story 05.04).

use std::path::Path;
use std::sync::Mutex;

use edger_core::{ApiKeyPrincipal, CoreError};
use rusqlite::{params, Connection};
use serde_json;
use sha2::{Digest, Sha256};

/// Store contract for API key lookup and provisioning (mockable in tests).
pub trait ApiKeyStore: Send + Sync {
    fn lookup_by_key(&self, raw_key: &str) -> Result<Option<ApiKeyPrincipal>, CoreError>;
    fn insert_key(
        &self,
        raw_key: &str,
        name: &str,
        role: &str,
        permissions: &[String],
        namespaces: &[String],
        expires_at: Option<u64>,
    ) -> Result<u64, CoreError>;
}

/// SQLite-backed API key store (`EDGER_AUTH_DB` path in production).
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
                role TEXT NOT NULL,
                permissions TEXT NOT NULL,
                namespaces TEXT NOT NULL,
                expires_at INTEGER,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
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
        raw_key.chars().take(8).collect()
    }
}

impl ApiKeyStore for SqliteApiKeyStore {
    fn lookup_by_key(&self, raw_key: &str) -> Result<Option<ApiKeyPrincipal>, CoreError> {
        let hash = Self::hash_key(raw_key);
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, key_prefix, role, permissions, namespaces, expires_at
                 FROM api_keys WHERE key_hash = ?1",
            )
            .map_err(db_err)?;

        let mut rows = stmt
            .query_map(params![hash], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            })
            .map_err(db_err)?;

        let Some(row) = rows.next() else {
            return Ok(None);
        };
        let (id, name, key_prefix, role, permissions_json, namespaces_json, expires_at) =
            row.map_err(db_err)?;
        let permissions: Vec<String> = serde_json::from_str(&permissions_json).map_err(json_err)?;
        let namespaces: Vec<String> = serde_json::from_str(&namespaces_json).map_err(json_err)?;

        if let Some(exp) = expires_at {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| CoreError::new("STORE_ERROR", e.to_string()))?
                .as_secs();
            if (exp as u64) < now {
                return Ok(None);
            }
        }

        Ok(Some(ApiKeyPrincipal {
            id,
            name,
            key_prefix,
            role,
            permissions,
            namespaces,
            is_root: false,
            expires_at: expires_at.map(|v| v as u64),
        }))
    }

    fn insert_key(
        &self,
        raw_key: &str,
        name: &str,
        role: &str,
        permissions: &[String],
        namespaces: &[String],
        expires_at: Option<u64>,
    ) -> Result<u64, CoreError> {
        let hash = Self::hash_key(raw_key);
        let prefix = Self::key_prefix(raw_key);
        let permissions_json = serde_json::to_string(permissions).map_err(json_err)?;
        let namespaces_json = serde_json::to_string(namespaces).map_err(json_err)?;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            "INSERT INTO api_keys (name, key_hash, key_prefix, role, permissions, namespaces, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                name,
                hash,
                prefix,
                role,
                permissions_json,
                namespaces_json,
                expires_at.map(|v| v as i64)
            ],
        )
        .map_err(db_err)?;
        Ok(conn.last_insert_rowid() as u64)
    }
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

    #[test]
    fn insert_and_lookup_roundtrip() {
        let store = SqliteApiKeyStore::in_memory().unwrap();
        store
            .insert_key(
                "btk_test_secret_key",
                "acme-editor",
                "editor",
                &["read".into()],
                &["@acme".into()],
                None,
            )
            .unwrap();
        let principal = store
            .lookup_by_key("btk_test_secret_key")
            .unwrap()
            .expect("principal");
        assert_eq!(principal.name, "acme-editor");
        assert_eq!(principal.namespaces, vec!["@acme"]);
    }

    #[test]
    fn wrong_key_returns_none() {
        let store = SqliteApiKeyStore::in_memory().unwrap();
        store
            .insert_key("real-key", "n", "editor", &[], &["*".into()], None)
            .unwrap();
        assert!(store.lookup_by_key("wrong-key").unwrap().is_none());
    }
}
