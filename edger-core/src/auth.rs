//! Auth provider trait and helpers.

use anyhow::Result;

use crate::extension::Extension;
use crate::principal::{principal_can_access_namespace, ApiKeyPrincipal};

/// Header map as owned pairs (pure; orchestrator converts from hyper).
pub type HeaderPairs = [(String, String)];

/// Auth extension contract (Turso/SQLite store lives in edger-ext-auth later).
pub trait AuthProvider: Extension {
    fn authenticate(&self, headers: &[(String, String)]) -> Result<Option<ApiKeyPrincipal>>;

    fn can_access_namespace(&self, principal: &ApiKeyPrincipal, namespace: &str) -> bool {
        principal_can_access_namespace(principal, namespace)
    }
}
