//! Built-in control-plane auth gate.

use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use axum::http::HeaderMap;
use edger_core::{root_principal, ApiKeyPrincipal};

#[cfg(test)]
use crate::oidc::JwksSource;
use crate::oidc::{OidcConfig, OidcValidator};

const ROOT_API_KEY_ENV: &str = "ROOT_API_KEY";
const EDGER_ROOT_KEY_FILE_ENV: &str = "EDGER_ROOT_KEY_FILE";
const EDGER_OIDC_ADMIN_ROLE_ENV: &str = "EDGER_OIDC_ADMIN_ROLE";
const EDGER_OIDC_AUDIENCE_ENV: &str = "EDGER_OIDC_AUDIENCE";
const EDGER_OIDC_ISSUER_ENV: &str = "EDGER_OIDC_ISSUER";
const EDGER_OIDC_NAMESPACES_CLAIM_ENV: &str = "EDGER_OIDC_NAMESPACES_CLAIM";
const EDGER_OIDC_REQUIRED_ROLE_ENV: &str = "EDGER_OIDC_REQUIRED_ROLE";
const EDGER_OIDC_ROLES_CLAIM_ENV: &str = "EDGER_OIDC_ROLES_CLAIM";
const DEFAULT_OIDC_NAMESPACES_CLAIM: &str = "namespaces";

/// Auth gate configuration. `EDGER_ROOT_KEY_FILE` takes precedence over `ROOT_API_KEY`.
#[derive(Clone, Debug, Default)]
pub struct ControlAuthConfig {
    pub oidc: Option<OidcConfig>,
    root_key_source: RootKeySource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlAuthConfigError(String);

impl Display for ControlAuthConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ControlAuthConfigError {}

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
    /// Store de api-keys persistentes (`egk_`). `None` = instância sem
    /// store (open mode, ou boot sem EDGER_API_KEYS_DB utilizável).
    keys: Option<Arc<crate::api_keys::ApiKeyService>>,
    /// Console por senha: root user + sessões `ses-` persistentes
    /// (`EDGER_ROOT_PASSWORD_FILE` / `EDGER_API_KEYS_DB`). `None` = instância
    /// sem a feature (open mode fresco, ou boot sem store utilizável).
    console: Option<Arc<crate::console_auth::ConsoleAuthService>>,
    /// Se o store de console tem usuário root (computado na ligação; a
    /// semente só acontece no boot, então o flag é estável no processo).
    console_seeded: Arc<AtomicBool>,
}

impl ControlAuthConfig {
    pub fn from_env() -> Result<Self, ControlAuthConfigError> {
        let root_key_file = non_empty_env(EDGER_ROOT_KEY_FILE_ENV).map(PathBuf::from);
        let root_key = non_empty_env(ROOT_API_KEY_ENV);
        let root_key_source = match (root_key_file, root_key) {
            (Some(path), _) => RootKeySource::File(path),
            (None, Some(key)) => RootKeySource::Env(key),
            (None, None) => RootKeySource::Open,
        };
        let oidc = oidc_config_from_env()?;
        Ok(Self {
            oidc,
            root_key_source,
        })
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
            keys: None,
            console: None,
            console_seeded: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Liga o store de api-keys — chamado no boot, depois do from_env.
    pub fn with_key_service(mut self, keys: Arc<crate::api_keys::ApiKeyService>) -> Self {
        self.keys = Some(keys);
        self
    }

    pub fn key_service(&self) -> Option<&Arc<crate::api_keys::ApiKeyService>> {
        self.keys.as_ref()
    }

    /// Liga o store de console (root user + sessões `ses-`) — chamado no
    /// boot depois do store de keys, no MESMO caminho de banco.
    pub fn with_console_service(
        mut self,
        console: Arc<crate::console_auth::ConsoleAuthService>,
    ) -> Self {
        let seeded = match console.has_root_user() {
            Ok(seeded) => seeded,
            Err(err) => {
                // Falha fechada: sem saber se há root no store, o gate de
                // open mode continua FECHADO.
                tracing::warn!(
                    code = %err.code,
                    "console root user check failed, gate stays closed: {}", err.message
                );
                true
            }
        };
        self.console_seeded.store(seeded, Ordering::SeqCst);
        self.console = Some(console);
        self
    }

    pub fn console_service(&self) -> Option<&Arc<crate::console_auth::ConsoleAuthService>> {
        self.console.as_ref()
    }

    pub fn from_env() -> Result<Self, ControlAuthConfigError> {
        ControlAuthConfig::from_env().map(Self::new)
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
            keys: None,
            console: None,
            console_seeded: Arc::new(AtomicBool::new(false)),
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

        // Keys persistentes: o prefixo discriminante decide — um egk_ que
        // não autentica NÃO cai no OIDC (não é JWT), falha aqui mesmo.
        // Sessões da console (ses-…) têm o mesmo comportamento: prefixo
        // disjunto de egk_ e de JWT, e sem store a sessão é negada na hora.
        if let Some(credential) = extract_api_key(headers) {
            if credential.starts_with(crate::api_keys::API_KEY_PREFIX) {
                let keys = self.keys.as_ref()?;
                return keys.authenticate(&credential);
            }
            if credential.starts_with(crate::console_auth::SESSION_PREFIX) {
                return self
                    .console
                    .as_ref()
                    .and_then(|console| console.authenticate_session(&credential));
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

    /// Gate de open mode: sem root key, sem OIDC e sem root user no store de
    /// console. A senha semeada FECHA o gate mesmo sem as demais credenciais
    /// (open mode só existe quando não há NENHUMA credencial).
    pub fn is_open(&self) -> bool {
        self.is_open_without_console() && !self.console_seeded.load(Ordering::SeqCst)
    }

    /// Gate de credenciais estáticas (root key/OIDC) — usado no boot ANTES do
    /// store de console estar ligado.
    pub fn is_open_without_console(&self) -> bool {
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

fn oidc_config_from_env() -> Result<Option<OidcConfig>, ControlAuthConfigError> {
    let pair = validate_oidc_pair(
        non_empty_env(EDGER_OIDC_ISSUER_ENV),
        non_empty_env(EDGER_OIDC_AUDIENCE_ENV),
    )?;
    Ok(pair.map(|(issuer, audience)| OidcConfig {
        admin_role: non_empty_env(EDGER_OIDC_ADMIN_ROLE_ENV),
        audience,
        issuer,
        namespaces_claim: non_empty_env(EDGER_OIDC_NAMESPACES_CLAIM_ENV)
            .unwrap_or_else(|| DEFAULT_OIDC_NAMESPACES_CLAIM.into()),
        required_role: non_empty_env(EDGER_OIDC_REQUIRED_ROLE_ENV),
        roles_claim: non_empty_env(EDGER_OIDC_ROLES_CLAIM_ENV),
    }))
}

fn validate_oidc_pair(
    issuer: Option<String>,
    audience: Option<String>,
) -> Result<Option<(String, String)>, ControlAuthConfigError> {
    match (issuer, audience) {
        (None, None) => Ok(None),
        (Some(issuer), Some(audience)) => Ok(Some((issuer, audience))),
        (Some(_), None) => Err(ControlAuthConfigError(
            "EDGER_OIDC_ISSUER requires EDGER_OIDC_AUDIENCE".into(),
        )),
        (None, Some(_)) => Err(ControlAuthConfigError(
            "EDGER_OIDC_AUDIENCE requires EDGER_OIDC_ISSUER".into(),
        )),
    }
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

    #[test]
    fn partial_oidc_configuration_is_rejected() {
        assert!(validate_oidc_pair(Some("issuer".into()), None).is_err());
        assert!(validate_oidc_pair(None, Some("audience".into())).is_err());
        assert_eq!(validate_oidc_pair(None, None).unwrap(), None);
        assert_eq!(
            validate_oidc_pair(Some("issuer".into()), Some("audience".into())).unwrap(),
            Some(("issuer".into(), "audience".into()))
        );
    }

    #[tokio::test]
    async fn seeded_console_closes_open_gate_and_session_authenticates_as_root() {
        use crate::console_auth::ConsoleAuthService;

        let service = Arc::new(ConsoleAuthService::in_memory().unwrap());
        service.seed_root_if_empty(Some("Str0ng!Passw0rd")).unwrap();
        let auth = ControlAuth::new(ControlAuthConfig::default()).with_console_service(service);
        // Sem root key/OIDC, MAS com root semeado: o gate NÃO fica aberto.
        assert!(auth.is_open_without_console());
        assert!(!auth.is_open());

        let token = auth
            .console_service()
            .unwrap()
            .login(
                std::net::IpAddr::from([10u8, 0, 0, 1]),
                "root",
                "Str0ng!Passw0rd",
            )
            .unwrap();

        // Sessão autentica via Bearer E via X-API-Key, virando root.
        for header in ["authorization", "x-api-key"] {
            let mut headers = HeaderMap::new();
            if header == "authorization" {
                headers.insert(
                    "authorization",
                    HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
                );
            } else {
                headers.insert("x-api-key", HeaderValue::from_str(&token).unwrap());
            }
            let principal = auth.authenticate_headers(&headers).await.unwrap();
            assert!(principal.is_root, "via {header}");
        }
    }

    #[tokio::test]
    async fn empty_console_keeps_open_gate_and_rejects_unknown_session() {
        use crate::console_auth::ConsoleAuthService;

        let service = Arc::new(ConsoleAuthService::in_memory().unwrap());
        // Sem semente: banco vazio, nenhum usuário criado.
        assert!(!service.has_root_user().unwrap());
        let auth = ControlAuth::new(ControlAuthConfig::default()).with_console_service(service);
        assert!(auth.is_open());

        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer ses-nao-existe"),
        );
        assert!(auth.authenticate_headers(&headers).await.is_none());
    }

    /// Convivência OIDC: com console ligado e OIDC configurado, o JWT segue
    /// validando pelo OIDC e o prefixo `ses-` NUNCA cai no OIDC (disjoint de
    /// JWT, que sempre começa com `eyJ`).
    #[tokio::test]
    async fn oidc_and_console_session_coexist() {
        use crate::console_auth::ConsoleAuthService;
        use crate::oidc::{JwksSource, OidcDiscovery, OidcError};
        use async_trait::async_trait;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        use jsonwebtoken::jwk::JwkSet;
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        use rand::thread_rng;
        use rsa::pkcs8::{EncodePrivateKey, LineEnding};
        use rsa::traits::PublicKeyParts;
        use rsa::{RsaPrivateKey, RsaPublicKey};
        use serde_json::{json, Value};
        use std::collections::VecDeque;
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Arc, Mutex};
        use std::time::{SystemTime, UNIX_EPOCH};

        const AUDIENCE: &str = "edger-control";
        const ISSUER: &str = "https://issuer.example.test";

        struct StaticJwks {
            jwks: Arc<Mutex<VecDeque<JwkSet>>>,
            calls: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl JwksSource for StaticJwks {
            async fn discovery(&self, _issuer: &str) -> Result<OidcDiscovery, OidcError> {
                Ok(OidcDiscovery {
                    jwks_uri: "https://issuer.example.test/jwks".into(),
                })
            }

            async fn jwks(&self, _uri: &str) -> Result<JwkSet, OidcError> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(self.jwks.lock().unwrap().front().unwrap().clone())
            }
        }

        let mut rng = thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key = RsaPublicKey::from(&private_key);
        let private_pem = private_key.to_pkcs8_pem(LineEnding::LF).unwrap();
        let n = URL_SAFE_NO_PAD.encode(public_key.n().to_bytes_be());
        let e = URL_SAFE_NO_PAD.encode(public_key.e().to_bytes_be());
        let jwks: JwkSet = serde_json::from_value(json!({
            "keys": [{
                "alg": "RS256", "e": e, "kid": "kid-1", "kty": "RSA", "n": n, "use": "sig"
            }]
        }))
        .unwrap();
        let source = Arc::new(StaticJwks {
            jwks: Arc::new(Mutex::new(VecDeque::from([jwks]))),
            calls: Arc::default(),
        });

        let config = OidcConfig {
            admin_role: Some("edger-admin".into()),
            audience: AUDIENCE.into(),
            issuer: ISSUER.into(),
            namespaces_claim: "namespaces".into(),
            required_role: None,
            roles_claim: Some("groups".into()),
        };
        let auth = ControlAuth::with_oidc_source(config, source.clone())
            .with_console_service(Arc::new(ConsoleAuthService::in_memory().unwrap()));

        // `ses-` com OIDC ativo: NUNCA cai no OIDC (e sem sessão no store).
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer ses-qualquer-coisa"),
        );
        assert!(auth.authenticate_headers(&headers).await.is_none());
        // O OIDC não foi consultado por causa do prefixo (0 chamadas JWKS).
        assert_eq!(source.calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        // JWT válido com o papel admin segue validando (convivência).
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claims: Value = json!({
            "aud": AUDIENCE, "exp": now + 3600, "iat": now, "iss": ISSUER,
            "nbf": now.saturating_sub(1), "sub": "user-1", "groups": ["edger-admin"]
        });
        let token = encode(
            &Header {
                alg: Algorithm::RS256,
                kid: Some("kid-1".into()),
                ..Default::default()
            },
            &claims,
            &EncodingKey::from_rsa_pem(private_pem.as_bytes()).unwrap(),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let principal = auth.authenticate_headers(&headers).await.unwrap();
        assert!(
            principal.is_root,
            "OIDC admin role must keep authenticating"
        );
    }

    #[test]
    fn is_open_reflects_console_seed_state_with_static_key() {
        use crate::console_auth::ConsoleAuthService;

        // Root key configurada: fechado, com ou sem console.
        let keyed = ControlAuth::with_static_key("root-secret");
        assert!(!keyed.is_open());
        let seeded = Arc::new(ConsoleAuthService::in_memory().unwrap());
        seeded.seed_root_if_empty(Some("Str0ng!Passw0rd")).unwrap();
        assert!(!keyed.clone().with_console_service(seeded.clone()).is_open());
        // Sem root key e sem seed: o gate segue aberto (sem credencial nenhuma).
        let unseeded = ConsoleAuthService::in_memory().unwrap();
        let open =
            ControlAuth::new(ControlAuthConfig::default()).with_console_service(Arc::new(unseeded));
        assert!(open.is_open());
    }
}
