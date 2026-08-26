//! Contrato PURO do store de API keys — a implementação (SQLite) mora no
//! orchestrator. Ressuscitado do `edger-ext-auth` removido no Epic 17.A: a
//! objeção da época era persistência em K8s, e hoje o PVC de user workers
//! existe por padrão no chart.

use crate::admin::AdminApiKeyInfo;
use crate::{ApiKeyPrincipal, CoreError};

/// Parâmetros de inserção — o hash é responsabilidade da implementação; o
/// trait fala em `raw_key` para o contrato de hash morar num lugar só.
pub struct NewApiKey<'a> {
    pub raw_key: &'a str,
    pub name: &'a str,
    pub role: &'a str,
    pub permissions: &'a [String],
    pub namespaces: &'a [String],
    pub workers: &'a [String],
    pub expires_at: Option<u64>,
}

pub trait ApiKeyStore: Send + Sync {
    /// Principal vivo para a key crua — `None` cobre inexistente, revogada e
    /// expirada (o chamador não distingue de propósito: 401 é 401).
    fn lookup_by_key(&self, raw_key: &str) -> Result<Option<ApiKeyPrincipal>, CoreError>;
    fn list_keys(&self) -> Result<Vec<AdminApiKeyInfo>, CoreError>;
    fn get_key(&self, id: u64) -> Result<Option<AdminApiKeyInfo>, CoreError>;
    fn insert_key(&self, new_key: NewApiKey<'_>) -> Result<u64, CoreError>;
    /// Revogação TERMINAL (sem reativação). `false` = id inexistente.
    fn revoke_key(&self, id: u64) -> Result<bool, CoreError>;
    /// Remoção definitiva; exige revogação prévia (`KEY_NOT_REVOKED`).
    fn delete_key(&self, id: u64) -> Result<bool, CoreError>;
    fn touch_last_used(&self, id: u64) -> Result<(), CoreError>;
}
