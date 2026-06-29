//! Auth gate — API key resolution, public route bypass, namespace check (story 05.04).

use std::sync::Arc;

use axum::http::HeaderMap;
use edger_core::{
    principal_can_access_namespace, root_principal, ApiKeyPrincipal, CoreError, PublicRoutesConfig,
};

use crate::store::ApiKeyStore;

/// Auth gate configuration (env: `ROOT_API_KEY`, global `publicRoutes`).
#[derive(Clone, Debug, Default)]
pub struct AuthGateConfig {
    pub root_api_key: Option<String>,
    pub global_public_routes: PublicRoutesConfig,
}

/// Early auth gate wired into the request pipeline.
#[derive(Clone)]
pub struct AuthGate {
    pub config: AuthGateConfig,
    store: Arc<dyn ApiKeyStore>,
}

impl AuthGate {
    pub fn new(config: AuthGateConfig, store: Arc<dyn ApiKeyStore>) -> Self {
        Self { config, store }
    }

    pub fn from_env(store: Arc<dyn ApiKeyStore>) -> Self {
        let root_api_key = std::env::var("ROOT_API_KEY").ok().filter(|s| !s.is_empty());
        Self::new(
            AuthGateConfig {
                root_api_key,
                ..Default::default()
            },
            store,
        )
    }

    pub fn authenticate_headers(
        &self,
        headers: &HeaderMap,
    ) -> Result<Option<ApiKeyPrincipal>, CoreError> {
        let Some(raw_key) = extract_api_key(headers) else {
            return Ok(None);
        };
        self.authenticate_raw_key(&raw_key)
    }

    pub fn authenticate_raw_key(
        &self,
        raw_key: &str,
    ) -> Result<Option<ApiKeyPrincipal>, CoreError> {
        if self
            .config
            .root_api_key
            .as_ref()
            .is_some_and(|root| root == raw_key)
        {
            return Ok(Some(root_principal()));
        }
        self.store.lookup_by_key(raw_key)
    }

    /// Run gate: public bypass, authenticate, namespace check.
    pub fn authorize(
        &self,
        path: &str,
        headers: &HeaderMap,
        worker_public_routes: Option<&PublicRoutesConfig>,
        worker_namespace: Option<&str>,
    ) -> Result<Option<ApiKeyPrincipal>, CoreError> {
        if is_public_route(path, &self.config.global_public_routes)
            || worker_public_routes.is_some_and(|routes| is_public_route(path, routes))
        {
            return Ok(None);
        }

        let principal = self
            .authenticate_headers(headers)?
            .ok_or_else(|| CoreError::new("UNAUTHORIZED", "missing or invalid API key"))?;

        if let Some(namespace) = worker_namespace.filter(|ns| !ns.is_empty()) {
            if !principal_can_access_namespace(&principal, namespace) {
                return Err(CoreError::new(
                    "FORBIDDEN",
                    format!("namespace access denied for {namespace}"),
                ));
            }
        }

        Ok(Some(principal))
    }
}

/// Extract API key from `Authorization: Bearer` or `X-API-Key`.
pub fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let prefix = "Bearer ";
        if let Some(token) = value.strip_prefix(prefix) {
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

/// Check whether a path matches configured public routes (Buntime `publicRoutes`).
pub fn is_public_route(path: &str, config: &PublicRoutesConfig) -> bool {
    for route in &config.routes {
        if config.exact {
            if path == route {
                return true;
            }
        } else if path == route || path.starts_with(&format!("{route}/")) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::sync::Arc;

    use crate::store::SqliteApiKeyStore;

    #[test]
    fn public_route_exact_and_prefix() {
        let exact = PublicRoutesConfig {
            routes: vec!["/login".into()],
            exact: true,
        };
        assert!(is_public_route("/login", &exact));
        assert!(!is_public_route("/login/oauth", &exact));

        let prefix = PublicRoutesConfig {
            routes: vec!["/health".into()],
            exact: false,
        };
        assert!(is_public_route("/health", &prefix));
        assert!(is_public_route("/health/live", &prefix));
    }

    #[test]
    fn root_key_returns_synthetic_principal() {
        let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
        let gate = AuthGate::new(
            AuthGateConfig {
                root_api_key: Some("root-secret".into()),
                ..Default::default()
            },
            store,
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer root-secret"),
        );
        let principal = gate.authenticate_headers(&headers).unwrap().unwrap();
        assert!(principal.is_root);
        assert_eq!(principal.namespaces, vec!["*"]);
    }
}
