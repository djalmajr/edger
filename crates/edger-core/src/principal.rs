//! Auth principal types (Buntime ApiKeyPrincipal port).

use serde::{Deserialize, Serialize};

/// API key principal resolved from auth headers.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyPrincipal {
    pub id: u64,
    pub name: String,
    pub key_prefix: String,
    pub role: String,
    pub permissions: Vec<String>,
    /// `"*"` or scoped namespaces like `"@acme"`.
    pub namespaces: Vec<String>,
    /// Escopo de RECURSO por worker: `"*"`, nome exato, ou glob de sufixo
    /// (`"p-abc*"`). Ortogonal a namespaces (boundary multi-tenant): uma key
    /// pode ver o tenant inteiro mas só tocar os workers do app dela. O
    /// default serde mantém principals antigos (OIDC, serializações) com
    /// acesso total, que era o comportamento antes do campo existir.
    #[serde(default = "star_vec")]
    pub workers: Vec<String>,
    pub is_root: bool,
    pub expires_at: Option<u64>,
}

fn star_vec() -> Vec<String> {
    vec!["*".into()]
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

/// Pure worker-scope gate: `"*"`, exact name, or suffix glob (`"p-abc*"`).
pub fn principal_can_access_worker(principal: &ApiKeyPrincipal, worker: &str) -> bool {
    if principal.is_root {
        return true;
    }
    principal.workers.iter().any(|entry| {
        entry == "*"
            || entry == worker
            || entry
                .strip_suffix('*')
                .is_some_and(|prefix| !prefix.is_empty() && worker.starts_with(prefix))
    })
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
        workers: vec!["*".into()],
        is_root: true,
        expires_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped(workers: Vec<&str>) -> ApiKeyPrincipal {
        ApiKeyPrincipal {
            id: 1,
            name: "scoped".into(),
            key_prefix: "egk_scoped".into(),
            role: "operator".into(),
            permissions: vec!["workers:read".into()],
            namespaces: vec!["*".into()],
            workers: workers.into_iter().map(str::to_string).collect(),
            is_root: false,
            expires_at: None,
        }
    }

    #[test]
    fn worker_scope_matches_star_exact_and_suffix_glob() {
        let key = scoped(vec!["hello", "p-abc*"]);
        assert!(principal_can_access_worker(&key, "hello"));
        assert!(principal_can_access_worker(&key, "p-abc-api"));
        assert!(!principal_can_access_worker(&key, "other"));
        // Glob de sufixo exige prefixo não-vazio: "*" solto é a via própria.
        let bare = scoped(vec!["*"]);
        assert!(principal_can_access_worker(&bare, "anything"));
    }

    #[test]
    fn workers_field_defaults_to_star_on_old_payloads() {
        let old = serde_json::json!({
            "id": 7, "name": "oidc", "keyPrefix": "oidc", "role": "operator",
            "permissions": ["workers:read"], "namespaces": ["@acme"],
            "isRoot": false, "expiresAt": null
        });
        let principal: ApiKeyPrincipal = serde_json::from_value(old).unwrap();
        assert_eq!(principal.workers, vec!["*".to_string()]);
    }
}
