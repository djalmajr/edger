//! Auth provider trait and helpers.

use anyhow::Result;

use crate::extension::Extension;
use crate::principal::{principal_can_access_namespace, ApiKeyPrincipal};

/// Header map as owned pairs (pure; orchestrator converts from hyper).
pub type HeaderPairs = [(String, String)];

/// Extract API key from header pairs (`Authorization: Bearer` or `x-api-key`).
pub fn extract_api_key_from_pairs(headers: &[(String, String)]) -> Option<String> {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("authorization") {
            let prefix = "Bearer ";
            if let Some(token) = value.strip_prefix(prefix) {
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("x-api-key") && !value.is_empty() {
            return Some(value.clone());
        }
    }
    None
}

/// Auth extension contract (Turso/SQLite store lives in edger-ext-auth later).
pub trait AuthProvider: Extension {
    fn authenticate(&self, headers: &[(String, String)]) -> Result<Option<ApiKeyPrincipal>>;

    fn can_access_namespace(&self, principal: &ApiKeyPrincipal, namespace: &str) -> bool {
        principal_can_access_namespace(principal, namespace)
    }
}
