//! API key store trait (pure contract; implementations live in extensions).

use crate::principal::ApiKeyPrincipal;
use crate::CoreError;

/// Persistence contract for API keys (SQLite/Turso impls in `edger-ext-auth`).
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
