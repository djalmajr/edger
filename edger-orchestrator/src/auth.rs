//! Auth gate — delegates to `AuthProvider` extension (story 05.04 / 06.02).

use std::sync::Arc;

use axum::http::HeaderMap;
use edger_core::{
    principal_can_access_namespace, AdminApiKeyInfo, AdminCreateApiKeyRequest, ApiKeyPrincipal,
    AuthProvider, CoreError, PublicRoutesConfig,
};

/// Auth gate configuration (global `publicRoutes` only; keys resolved by `AuthProvider`).
#[derive(Clone, Debug, Default)]
pub struct AuthGateConfig {
    pub global_public_routes: PublicRoutesConfig,
}

/// Early auth gate wired into the request pipeline.
#[derive(Clone)]
pub struct AuthGate {
    pub config: AuthGateConfig,
    provider: Arc<dyn AuthProvider>,
}

impl AuthGate {
    pub fn new(config: AuthGateConfig, provider: Arc<dyn AuthProvider>) -> Self {
        Self { config, provider }
    }

    pub fn authenticate_headers(
        &self,
        headers: &HeaderMap,
    ) -> Result<Option<ApiKeyPrincipal>, CoreError> {
        let pairs = header_map_to_pairs(headers);
        self.provider
            .authenticate(&pairs)
            .map_err(|e| CoreError::new("AUTH_ERROR", e.to_string()))
    }

    pub fn list_api_keys(&self) -> Result<Vec<AdminApiKeyInfo>, CoreError> {
        self.provider
            .list_api_keys()
            .map_err(|e| CoreError::new("AUTH_ERROR", e.to_string()))
    }

    pub fn create_api_key(
        &self,
        raw_key: &str,
        request: &AdminCreateApiKeyRequest,
    ) -> Result<AdminApiKeyInfo, CoreError> {
        self.provider
            .create_api_key(raw_key, request)
            .map_err(|e| CoreError::new("AUTH_ERROR", e.to_string()))
    }

    pub fn revoke_api_key(&self, id: u64) -> Result<bool, CoreError> {
        self.provider
            .revoke_api_key(id)
            .map_err(|e| CoreError::new("AUTH_ERROR", e.to_string()))
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

fn header_map_to_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

/// Extract API key from `Authorization: Bearer` or `X-API-Key`.
pub fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    edger_core::extract_api_key_from_pairs(&header_map_to_pairs(headers))
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

    use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};

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
        let ext = Arc::new(AuthExtension::new(
            Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
            Some("root-secret".into()),
        ));
        let gate = AuthGate::new(AuthGateConfig::default(), ext);
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
