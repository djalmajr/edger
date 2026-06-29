//! Auth principal types (Buntime ApiKeyPrincipal port).

use serde::{Deserialize, Serialize};

/// API key principal resolved from auth headers.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyPrincipal {
    pub id: u64,
    pub name: String,
    pub key_prefix: String,
    pub role: String,
    pub permissions: Vec<String>,
    /// `"*"` or scoped namespaces like `"@acme"`.
    pub namespaces: Vec<String>,
    pub is_root: bool,
    pub expires_at: Option<u64>,
}

/// Pure namespace gate (orchestrator calls before dispatch).
pub fn principal_can_access_namespace(principal: &ApiKeyPrincipal, namespace: &str) -> bool {
    if principal.is_root {
        return true;
    }
    for ns in &principal.namespaces {
        if ns == "*" {
            return true;
        }
        if ns == namespace {
            return true;
        }
    }
    false
}

/// Synthetic root principal for bootstrap / internal calls.
pub fn root_principal() -> ApiKeyPrincipal {
    ApiKeyPrincipal {
        id: 0,
        name: "root".into(),
        key_prefix: "root".into(),
        role: "admin".into(),
        permissions: vec!["*".into()],
        namespaces: vec!["*".into()],
        is_root: true,
        expires_at: None,
    }
}
