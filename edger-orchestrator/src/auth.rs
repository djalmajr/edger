//! Built-in control-plane auth gate.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use axum::http::HeaderMap;
use edger_core::{root_principal, ApiKeyPrincipal};

#[cfg(test)]
use crate::oidc::JwksSource;
use crate::oidc::{OidcConfig, OidcValidator};

const ROOT_API_KEY_ENV: &str = "ROOT_API_KEY";
const EDGER_ROOT_KEY_FILE_ENV: &str = "EDGER_ROOT_KEY_FILE";
const EDGER_OIDC_AUDIENCE_ENV: &str = "EDGER_OIDC_AUDIENCE";
const EDGER_OIDC_ISSUER_ENV: &str = "EDGER_OIDC_ISSUER";
const EDGER_OIDC_REQUIRED_ROLE_ENV: &str = "EDGER_OIDC_REQUIRED_ROLE";
const EDGER_OIDC_ROLES_CLAIM_ENV: &str = "EDGER_OIDC_ROLES_CLAIM";

/// Auth gate configuration. `EDGER_ROOT_KEY_FILE` takes precedence over `ROOT_API_KEY`.
#[derive(Clone, Debug, Default)]
pub struct ControlAuthConfig {
    pub oidc: Option<OidcConfig>,
    root_key_source: RootKeySource,
}

#[derive(Clone, Debug, Default)]
enum RootKeySource {
    #[default]
    Open,
    Env(String),
    File(PathBuf),
    Static(String),
}

#[derive(Clone, Debug, Default)]
struct FileRootKeyState {
    key: Option<String>,
    modified_at: Option<SystemTime>,
}

/// Built-in stateless gate for `/api/admin/*`.
#[derive(Clone)]
pub struct ControlAuth {
    pub config: ControlAuthConfig,
    file_state: Arc<RwLock<FileRootKeyState>>,
    oidc: Option<OidcValidator>,
}

impl ControlAuthConfig {
    pub fn from_env() -> Self {
        let root_key_file = non_empty_env(EDGER_ROOT_KEY_FILE_ENV).map(PathBuf::from);
        let root_key = non_empty_env(ROOT_API_KEY_ENV);
        let root_key_source = match (root_key_file, root_key) {
            (Some(path), _) => RootKeySource::File(path),
            (None, Some(key)) => RootKeySource::Env(key),
            (None, None) => RootKeySource::Open,
        };
        let oidc = oidc_config_from_env();
        Self {
            oidc,
            root_key_source,
        }
    }

    fn with_static_key(key: impl Into<String>) -> Self {
        Self {
            oidc: None,
            root_key_source: RootKeySource::Static(key.into()),
        }
    }
}

impl ControlAuth {
    pub fn new(config: ControlAuthConfig) -> Self {
        let oidc = config.oidc.clone().and_then(|oidc_config| {
            OidcValidator::with_http_source(oidc_config)
                .map_err(|err| tracing::warn!(error = %err, "could not initialize OIDC validator"))
                .ok()
        });
        Self {
            config,
            file_state: Arc::default(),
            oidc,
        }
    }

    pub fn from_env() -> Self {
        Self::new(ControlAuthConfig::from_env())
    }

    pub fn with_static_key(key: impl Into<String>) -> Self {
        Self::new(ControlAuthConfig::with_static_key(key))
    }

    #[cfg(test)]
    pub fn with_oidc_source(config: OidcConfig, source: Arc<dyn JwksSource>) -> Self {
        Self {
            config: ControlAuthConfig {
                oidc: Some(config.clone()),
                root_key_source: RootKeySource::Open,
            },
            file_state: Arc::default(),
            oidc: Some(OidcValidator::new(config, source)),
        }
    }

    pub async fn authenticate_headers(&self, headers: &HeaderMap) -> Option<ApiKeyPrincipal> {
        if let (Some(credential), Some(root_key)) =
            (extract_api_key(headers), self.current_root_key())
        {
            if credential == root_key {
                return Some(root_principal());
            }
        }

        let token = extract_bearer_token(headers)?;
        let validator = self.oidc.as_ref()?;
        match validator.validate_token(token).await {
            Ok(principal) => Some(principal),
            Err(err) => {
                tracing::debug!(error = %err, "OIDC bearer token rejected");
                None
            }
        }
    }

    pub fn is_open(&self) -> bool {
        matches!(self.config.root_key_source, RootKeySource::Open) && self.config.oidc.is_none()
    }

    pub fn root_key_for_internal_clients(&self) -> Option<String> {
        self.current_root_key()
    }

    fn current_root_key(&self) -> Option<String> {
        match &self.config.root_key_source {
            RootKeySource::Open => None,
            RootKeySource::Env(key) | RootKeySource::Static(key) => Some(key.clone()),
            RootKeySource::File(path) => self.current_file_root_key(path),
        }
    }

    fn current_file_root_key(&self, path: &Path) -> Option<String> {
        let modified_at = match std::fs::metadata(path).and_then(|metadata| metadata.modified()) {
            Ok(modified_at) => modified_at,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "could not stat EDGER_ROOT_KEY_FILE"
                );
                return None;
            }
        };

        {
            let state = self
                .file_state
                .read()
                .expect("control auth file state lock");
            if state.modified_at == Some(modified_at) {
                return state.key.clone();
            }
        }

        let key = match std::fs::read_to_string(path) {
            Ok(raw) => {
                let trimmed = raw.trim().to_string();
                if trimmed.is_empty() {
                    tracing::warn!(path = %path.display(), "EDGER_ROOT_KEY_FILE is empty");
                    None
                } else {
                    Some(trimmed)
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "could not read EDGER_ROOT_KEY_FILE"
                );
                None
            }
        };

        *self
            .file_state
            .write()
            .expect("control auth file state lock") = FileRootKeyState {
            key: key.clone(),
            modified_at: Some(modified_at),
        };
        key
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

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn oidc_config_from_env() -> Option<OidcConfig> {
    let issuer = non_empty_env(EDGER_OIDC_ISSUER_ENV)?;
    let audience = match non_empty_env(EDGER_OIDC_AUDIENCE_ENV) {
        Some(audience) => audience,
        None => {
            tracing::warn!(
                "EDGER_OIDC_ISSUER is configured but EDGER_OIDC_AUDIENCE is missing; OIDC disabled"
            );
            return None;
        }
    };
    Some(OidcConfig {
        audience,
        issuer,
        required_role: non_empty_env(EDGER_OIDC_REQUIRED_ROLE_ENV),
        roles_claim: non_empty_env(EDGER_OIDC_ROLES_CLAIM_ENV),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::io::Write;
    use std::time::Duration;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn static_root_key_returns_synthetic_principal() {
        let auth = ControlAuth::with_static_key("root-secret");
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer root-secret"),
        );
        let principal = auth.authenticate_headers(&headers).await.unwrap();
        assert!(principal.is_root);
        assert_eq!(principal.namespaces, vec!["*"]);
    }

    #[tokio::test]
    async fn static_root_key_rejects_missing_and_invalid_credentials() {
        let auth = ControlAuth::with_static_key("root-secret");
        assert!(auth.authenticate_headers(&HeaderMap::new()).await.is_none());

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("wrong"));
        assert!(auth.authenticate_headers(&headers).await.is_none());

        headers.insert("x-api-key", HeaderValue::from_static("root-secret"));
        assert!(auth.authenticate_headers(&headers).await.unwrap().is_root);
    }

    #[tokio::test]
    async fn open_mode_does_not_authenticate_headers() {
        let auth = ControlAuth::new(ControlAuthConfig::default());
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("anything"));

        assert!(auth.is_open());
        assert!(auth.authenticate_headers(&headers).await.is_none());
    }

    #[tokio::test]
    async fn file_root_key_hot_reloads_without_recreating_auth() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"k1\n").unwrap();
        file.flush().unwrap();
        let auth = ControlAuth::new(ControlAuthConfig {
            oidc: None,
            root_key_source: RootKeySource::File(file.path().to_path_buf()),
        });

        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("k1"));
        assert!(auth.authenticate_headers(&headers).await.unwrap().is_root);

        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(file.path(), "k2\n").unwrap();

        headers.insert("x-api-key", HeaderValue::from_static("k1"));
        assert!(auth.authenticate_headers(&headers).await.is_none());
        headers.insert("x-api-key", HeaderValue::from_static("k2"));
        assert!(auth.authenticate_headers(&headers).await.unwrap().is_root);
    }
}
