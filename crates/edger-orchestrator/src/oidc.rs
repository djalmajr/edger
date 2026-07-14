//! Generic OIDC JWT validation for the control plane.
//!
//! OAuth 2.0 Token Introspection (RFC 7662) is the future escape hatch for
//! opaque tokens; this v1 validator intentionally handles local JWT validation only.

use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use edger_core::ApiKeyPrincipal;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;

const JWKS_KID_MISS_REFRESH_LIMIT: Duration = Duration::from_secs(5);
const JWKS_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OidcConfig {
    pub admin_role: Option<String>,
    pub audience: String,
    pub issuer: String,
    pub namespaces_claim: String,
    pub required_role: Option<String>,
    pub roles_claim: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OidcDiscovery {
    pub jwks_uri: String,
}

#[async_trait]
pub trait JwksSource: Send + Sync {
    async fn discovery(&self, issuer: &str) -> Result<OidcDiscovery, OidcError>;

    async fn jwks(&self, jwks_uri: &str) -> Result<JwkSet, OidcError>;
}

#[derive(Clone)]
pub struct HttpJwksSource {
    client: reqwest::Client,
}

impl HttpJwksSource {
    pub fn new() -> Result<Self, OidcError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|err| OidcError::new(format!("could not build OIDC HTTP client: {err}")))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl JwksSource for HttpJwksSource {
    async fn discovery(&self, issuer: &str) -> Result<OidcDiscovery, OidcError> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|err| OidcError::new(format!("OIDC discovery request failed: {err}")))?
            .error_for_status()
            .map_err(|err| OidcError::new(format!("OIDC discovery returned error: {err}")))?;
        response
            .json::<OidcDiscovery>()
            .await
            .map_err(|err| OidcError::new(format!("OIDC discovery JSON is invalid: {err}")))
    }

    async fn jwks(&self, jwks_uri: &str) -> Result<JwkSet, OidcError> {
        let response = self
            .client
            .get(jwks_uri)
            .send()
            .await
            .map_err(|err| OidcError::new(format!("OIDC JWKS request failed: {err}")))?
            .error_for_status()
            .map_err(|err| OidcError::new(format!("OIDC JWKS returned error: {err}")))?;
        response
            .json::<JwkSet>()
            .await
            .map_err(|err| OidcError::new(format!("OIDC JWKS JSON is invalid: {err}")))
    }
}

#[derive(Clone)]
pub struct OidcValidator {
    cache: Arc<RwLock<JwksCache>>,
    config: OidcConfig,
    source: Arc<dyn JwksSource>,
}

#[derive(Clone, Debug, Default)]
struct JwksCache {
    jwks: Option<JwkSet>,
    jwks_uri: Option<String>,
    last_successful_refresh: Option<Instant>,
    last_kid_miss_refresh: Option<Instant>,
}

impl OidcValidator {
    pub fn new(config: OidcConfig, source: Arc<dyn JwksSource>) -> Self {
        Self {
            cache: Arc::default(),
            config,
            source,
        }
    }

    pub fn with_http_source(config: OidcConfig) -> Result<Self, OidcError> {
        Ok(Self::new(config, Arc::new(HttpJwksSource::new()?)))
    }

    pub async fn validate_token(&self, token: &str) -> Result<ApiKeyPrincipal, OidcError> {
        let header = decode_header(token)
            .map_err(|err| OidcError::new(format!("OIDC JWT header is invalid: {err}")))?;
        let jwks = self.cached_jwks().await?;
        match self.decode_with_jwks(token, &jwks).await {
            Ok(claims) => self.claims_to_principal(&claims),
            Err(err) if is_unknown_kid(&err, header.kid.as_deref()) => {
                let refreshed = self.refresh_after_kid_miss().await?;
                let claims = self.decode_with_jwks(token, &refreshed).await?;
                self.claims_to_principal(&claims)
            }
            Err(err) => Err(err),
        }
    }

    async fn cached_jwks(&self) -> Result<JwkSet, OidcError> {
        {
            let cache = self.cache.read().await;
            if cache
                .last_successful_refresh
                .is_some_and(|last| last.elapsed() < JWKS_CACHE_TTL)
            {
                if let Some(jwks) = cache.jwks.clone() {
                    return Ok(jwks);
                }
            }
        }
        self.refresh_jwks(false).await
    }

    async fn refresh_after_kid_miss(&self) -> Result<JwkSet, OidcError> {
        let now = Instant::now();
        {
            let cache = self.cache.read().await;
            if cache
                .last_kid_miss_refresh
                .is_some_and(|last| now.duration_since(last) < JWKS_KID_MISS_REFRESH_LIMIT)
            {
                return cache
                    .jwks
                    .clone()
                    .ok_or_else(|| OidcError::new("OIDC JWKS cache is empty"));
            }
        }

        let jwks = self.refresh_jwks(true).await?;
        self.cache.write().await.last_kid_miss_refresh = Some(now);
        Ok(jwks)
    }

    async fn refresh_jwks(&self, reuse_discovery: bool) -> Result<JwkSet, OidcError> {
        let cached_uri = self.cache.read().await.jwks_uri.clone();
        let jwks_uri = if reuse_discovery {
            match cached_uri {
                Some(uri) => uri,
                None => self.source.discovery(&self.config.issuer).await?.jwks_uri,
            }
        } else {
            self.source.discovery(&self.config.issuer).await?.jwks_uri
        };
        let jwks = self.source.jwks(&jwks_uri).await?;
        let mut cache = self.cache.write().await;
        cache.jwks = Some(jwks.clone());
        cache.jwks_uri = Some(jwks_uri);
        cache.last_successful_refresh = Some(Instant::now());
        Ok(jwks)
    }

    async fn decode_with_jwks(&self, token: &str, jwks: &JwkSet) -> Result<Value, OidcError> {
        let header = decode_header(token)
            .map_err(|err| OidcError::new(format!("OIDC JWT header is invalid: {err}")))?;
        let jwk = match header.kid.as_deref() {
            Some(kid) => jwks
                .find(kid)
                .ok_or_else(|| OidcError::unknown_kid(kid.to_string()))?,
            None if jwks.keys.len() == 1 => &jwks.keys[0],
            None => return Err(OidcError::new("OIDC JWT is missing kid")),
        };
        let decoding_key = DecodingKey::from_jwk(jwk)
            .map_err(|err| OidcError::new(format!("OIDC JWK is unsupported: {err}")))?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[self.config.audience.as_str()]);
        validation.set_issuer(&[self.config.issuer.as_str()]);
        validation.validate_nbf = true;

        decode::<Value>(token, &decoding_key, &validation)
            .map(|data| data.claims)
            .map_err(|err| OidcError::new(format!("OIDC JWT validation failed: {err}")))
    }

    fn claims_to_principal(&self, claims: &Value) -> Result<ApiKeyPrincipal, OidcError> {
        let roles = self
            .config
            .roles_claim
            .as_deref()
            .and_then(|path| claim_path(claims, path));
        if let (Some(path), Some(required_role)) = (
            self.config.roles_claim.as_deref(),
            self.config.required_role.as_deref(),
        ) {
            let roles = roles
                .ok_or_else(|| OidcError::new(format!("OIDC roles claim not found: {path}")))?;
            if !claim_contains_role(roles, required_role) {
                return Err(OidcError::new(format!(
                    "OIDC token does not include required role: {required_role}"
                )));
            }
        }

        let is_root = self.admin_role().is_some_and(|admin_role| {
            roles.is_some_and(|roles| claim_contains_role(roles, admin_role))
        });
        let namespaces = claim_path(claims, &self.config.namespaces_claim)
            .map(claim_string_values)
            .unwrap_or_default();

        Ok(ApiKeyPrincipal {
            id: 0,
            name: "oidc".into(),
            key_prefix: "oidc".into(),
            role: if is_root { "admin" } else { "oidc" }.into(),
            permissions: if is_root {
                vec!["*".into()]
            } else {
                Vec::new()
            },
            namespaces,
            is_root,
            expires_at: None,
        })
    }

    fn admin_role(&self) -> Option<&str> {
        self.config
            .admin_role
            .as_deref()
            .or(self.config.required_role.as_deref())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OidcError {
    kind: OidcErrorKind,
    message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OidcErrorKind {
    Invalid,
    UnknownKid(String),
}

impl OidcError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: OidcErrorKind::Invalid,
            message: message.into(),
        }
    }

    fn unknown_kid(kid: String) -> Self {
        Self {
            message: format!("OIDC JWKS does not contain kid: {kid}"),
            kind: OidcErrorKind::UnknownKid(kid),
        }
    }
}

impl Display for OidcError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OidcError {}

fn is_unknown_kid(err: &OidcError, kid: Option<&str>) -> bool {
    matches!(
        (&err.kind, kid),
        (OidcErrorKind::UnknownKid(missing), Some(kid)) if missing == kid
    )
}

fn claim_path<'a>(claims: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = claims;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn claim_contains_role(value: &Value, required_role: &str) -> bool {
    match value {
        Value::Array(items) => items
            .iter()
            .any(|item| item.as_str() == Some(required_role)),
        Value::String(role) => role == required_role,
        _ => false,
    }
}

fn claim_string_values(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str())
            .filter_map(trimmed_string)
            .collect(),
        Value::String(value) => trimmed_string(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn trimmed_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use rand::thread_rng;
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};
    use rsa::traits::PublicKeyParts;
    use rsa::{RsaPrivateKey, RsaPublicKey};
    use serde_json::json;

    use super::*;

    const AUDIENCE: &str = "edger-control";
    const ISSUER: &str = "https://issuer.example.test";
    const JWKS_URI: &str = "https://issuer.example.test/jwks";

    #[derive(Clone)]
    struct TestKey {
        encoding_key: EncodingKey,
        jwk: Value,
        kid: String,
    }

    #[derive(Clone)]
    struct StaticJwksSource {
        jwks_calls: Arc<AtomicUsize>,
        sets: Arc<Mutex<VecDeque<JwkSet>>>,
    }

    impl StaticJwksSource {
        fn new(sets: Vec<JwkSet>) -> Self {
            Self {
                jwks_calls: Arc::default(),
                sets: Arc::new(Mutex::new(sets.into())),
            }
        }
    }

    #[async_trait]
    impl JwksSource for StaticJwksSource {
        async fn discovery(&self, issuer: &str) -> Result<OidcDiscovery, OidcError> {
            assert_eq!(issuer, ISSUER);
            Ok(OidcDiscovery {
                jwks_uri: JWKS_URI.into(),
            })
        }

        async fn jwks(&self, jwks_uri: &str) -> Result<JwkSet, OidcError> {
            assert_eq!(jwks_uri, JWKS_URI);
            self.jwks_calls.fetch_add(1, Ordering::SeqCst);
            let mut sets = self.sets.lock().unwrap();
            if sets.len() > 1 {
                Ok(sets.pop_front().unwrap())
            } else {
                Ok(sets.front().unwrap().clone())
            }
        }
    }

    fn oidc_config() -> OidcConfig {
        OidcConfig {
            admin_role: None,
            audience: AUDIENCE.into(),
            issuer: ISSUER.into(),
            namespaces_claim: "namespaces".into(),
            required_role: None,
            roles_claim: None,
        }
    }

    fn validator(config: OidcConfig, source: StaticJwksSource) -> OidcValidator {
        OidcValidator::new(config, Arc::new(source))
    }

    fn test_key(kid: &str) -> TestKey {
        let mut rng = thread_rng();
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key = RsaPublicKey::from(&private_key);
        let private_pem = private_key.to_pkcs8_pem(LineEnding::LF).unwrap();
        let n = URL_SAFE_NO_PAD.encode(public_key.n().to_bytes_be());
        let e = URL_SAFE_NO_PAD.encode(public_key.e().to_bytes_be());
        TestKey {
            encoding_key: EncodingKey::from_rsa_pem(private_pem.as_bytes()).unwrap(),
            jwk: json!({
                "alg": "RS256",
                "e": e,
                "kid": kid,
                "kty": "RSA",
                "n": n,
                "use": "sig"
            }),
            kid: kid.into(),
        }
    }

    fn jwks(keys: &[&TestKey]) -> JwkSet {
        serde_json::from_value(json!({
            "keys": keys.iter().map(|key| key.jwk.clone()).collect::<Vec<_>>()
        }))
        .unwrap()
    }

    fn token(key: &TestKey, claims: Value) -> String {
        let header = Header {
            alg: Algorithm::RS256,
            kid: Some(key.kid.clone()),
            ..Default::default()
        };
        encode(&header, &claims, &key.encoding_key).unwrap()
    }

    fn base_claims() -> Value {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        json!({
            "aud": AUDIENCE,
            "exp": now + 3600,
            "iat": now,
            "iss": ISSUER,
            "nbf": now.saturating_sub(1),
            "sub": "user-1"
        })
    }

    #[tokio::test]
    async fn oidc_valid_token_without_scope_claims_returns_non_root_empty_namespaces() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let validator = validator(oidc_config(), source);

        let principal = validator
            .validate_token(&token(&key, base_claims()))
            .await
            .unwrap();

        assert!(!principal.is_root);
        assert_eq!(principal.name, "oidc");
        assert_eq!(principal.role, "oidc");
        assert!(principal.permissions.is_empty());
        assert!(principal.namespaces.is_empty());
    }

    #[tokio::test]
    async fn oidc_admin_role_claim_returns_root_principal() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let mut config = oidc_config();
        config.admin_role = Some("edger-admin".into());
        config.roles_claim = Some("groups".into());
        let validator = validator(config, source);
        let mut claims = base_claims();
        claims["groups"] = json!(["edger-admin"]);

        let principal = validator
            .validate_token(&token(&key, claims))
            .await
            .unwrap();

        assert!(principal.is_root);
        assert_eq!(principal.role, "admin");
        assert_eq!(principal.permissions, vec!["*"]);
        assert!(principal.namespaces.is_empty());
    }

    #[tokio::test]
    async fn oidc_non_admin_token_uses_scoped_namespaces() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let mut config = oidc_config();
        config.admin_role = Some("edger-admin".into());
        config.namespaces_claim = "edger.namespaces".into();
        config.roles_claim = Some("groups".into());
        let validator = validator(config, source);
        let mut claims = base_claims();
        claims["edger"] = json!({ "namespaces": ["@acme"] });
        claims["groups"] = json!(["viewer"]);

        let principal = validator
            .validate_token(&token(&key, claims))
            .await
            .unwrap();

        assert!(!principal.is_root);
        assert_eq!(principal.role, "oidc");
        assert!(principal.permissions.is_empty());
        assert_eq!(principal.namespaces, vec!["@acme"]);
    }

    #[tokio::test]
    async fn oidc_namespaces_claim_maps_array_exactly() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let mut config = oidc_config();
        config.namespaces_claim = "edger_namespaces".into();
        let validator = validator(config, source);
        let mut claims = base_claims();
        claims["edger_namespaces"] = json!(["@acme", "@beta"]);

        let principal = validator
            .validate_token(&token(&key, claims))
            .await
            .unwrap();

        assert!(!principal.is_root);
        assert_eq!(principal.namespaces, vec!["@acme", "@beta"]);
    }

    #[tokio::test]
    async fn oidc_expired_token_is_rejected() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let validator = validator(oidc_config(), source);
        let mut claims = base_claims();
        claims["exp"] = json!(1);

        let err = validator
            .validate_token(&token(&key, claims))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("ExpiredSignature"));
    }

    #[tokio::test]
    async fn oidc_wrong_audience_is_rejected() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let validator = validator(oidc_config(), source);
        let mut claims = base_claims();
        claims["aud"] = json!("other-audience");

        let err = validator
            .validate_token(&token(&key, claims))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("InvalidAudience"));
    }

    #[tokio::test]
    async fn oidc_wrong_issuer_is_rejected() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let validator = validator(oidc_config(), source);
        let mut claims = base_claims();
        claims["iss"] = json!("https://other-issuer.example.test");

        let err = validator
            .validate_token(&token(&key, claims))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("InvalidIssuer"));
    }

    #[tokio::test]
    async fn oidc_wrong_signature_is_rejected() {
        let trusted_key = test_key("kid-1");
        let signing_key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&trusted_key])]);
        let validator = validator(oidc_config(), source);

        let err = validator
            .validate_token(&token(&signing_key, base_claims()))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("InvalidSignature"));
    }

    #[tokio::test]
    async fn oidc_realm_access_roles_claim_authorizes_required_role() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let mut config = oidc_config();
        config.roles_claim = Some("realm_access.roles".into());
        config.required_role = Some("admin".into());
        let validator = validator(config, source);
        let mut claims = base_claims();
        claims["realm_access"] = json!({ "roles": ["viewer", "admin"] });

        assert!(
            validator
                .validate_token(&token(&key, claims))
                .await
                .unwrap()
                .is_root
        );
    }

    #[tokio::test]
    async fn oidc_realm_access_roles_claim_rejects_missing_role() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let mut config = oidc_config();
        config.roles_claim = Some("realm_access.roles".into());
        config.required_role = Some("admin".into());
        let validator = validator(config, source);
        let mut claims = base_claims();
        claims["realm_access"] = json!({ "roles": ["viewer"] });

        let err = validator
            .validate_token(&token(&key, claims))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("required role"));
    }

    #[tokio::test]
    async fn oidc_groups_claim_authorizes_required_role() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let mut config = oidc_config();
        config.roles_claim = Some("groups".into());
        config.required_role = Some("admin".into());
        let validator = validator(config, source);
        let mut claims = base_claims();
        claims["groups"] = json!(["admin"]);

        assert!(
            validator
                .validate_token(&token(&key, claims))
                .await
                .unwrap()
                .is_root
        );
    }

    #[tokio::test]
    async fn oidc_groups_claim_rejects_missing_claim() {
        let key = test_key("kid-1");
        let source = StaticJwksSource::new(vec![jwks(&[&key])]);
        let mut config = oidc_config();
        config.roles_claim = Some("groups".into());
        config.required_role = Some("admin".into());
        let validator = validator(config, source);

        let err = validator
            .validate_token(&token(&key, base_claims()))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("roles claim not found"));
    }

    #[tokio::test]
    async fn oidc_unknown_kid_refreshes_jwks_cache() {
        let old_key = test_key("kid-old");
        let new_key = test_key("kid-new");
        let source = StaticJwksSource::new(vec![jwks(&[&old_key]), jwks(&[&new_key])]);
        let calls = source.jwks_calls.clone();
        let validator = validator(oidc_config(), source);

        let principal = validator
            .validate_token(&token(&new_key, base_claims()))
            .await
            .unwrap();

        assert!(!principal.is_root);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn oidc_expired_jwks_cache_stops_trusting_removed_key() {
        let old_key = test_key("kid-old");
        let new_key = test_key("kid-new");
        let source = StaticJwksSource::new(vec![jwks(&[&old_key]), jwks(&[&new_key])]);
        let calls = source.jwks_calls.clone();
        let validator = validator(oidc_config(), source);
        let old_token = token(&old_key, base_claims());

        validator.validate_token(&old_token).await.unwrap();
        validator.cache.write().await.last_successful_refresh =
            Some(Instant::now() - JWKS_CACHE_TTL);

        let err = validator.validate_token(&old_token).await.unwrap_err();
        assert!(err.to_string().contains("does not contain kid"));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
