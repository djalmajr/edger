//! Server-to-server tenant identity lookup. This client never resolves resources.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::header::{HeaderValue, AUTHORIZATION, ETAG, IF_NONE_MATCH};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;

const MAX_RESPONSE_BYTES: usize = 1_024;
const MAX_CACHE_ENTRIES: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifyError {
    NotFound,
    Unavailable,
}

#[derive(Clone)]
pub struct TenantIdentityClient {
    client: Client,
    endpoint: Url,
    bearer: HeaderValue,
    cache: Arc<Mutex<HashMap<String, CachedIdentity>>>,
}

#[derive(Clone)]
struct CachedIdentity {
    slug: String,
    etag: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct IdentifyResponse {
    tenant_slug: String,
}

impl TenantIdentityClient {
    /// The endpoint is the Consumer API's exact `/v1/identify` URL. Plain HTTP
    /// is accepted only for loopback test/development servers.
    pub fn new(endpoint: Url, token: &str) -> Result<Self, &'static str> {
        if endpoint.path() != "/v1/identify"
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.username() != ""
            || endpoint.password().is_some()
        {
            return Err("invalid Tenancit identify endpoint");
        }
        let loopback = matches!(endpoint.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
        if endpoint.scheme() != "https" && !(endpoint.scheme() == "http" && loopback) {
            return Err("Tenancit identify requires HTTPS outside loopback");
        }
        if token.is_empty() || token.trim() != token {
            return Err("invalid Tenancit API client token");
        }
        let bearer = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| "invalid Tenancit API client token")?;
        let client = Client::builder()
            .timeout(Duration::from_millis(750))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot initialize Tenancit identify client")?;
        Ok(Self {
            client,
            endpoint,
            bearer,
            cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Every call revalidates with Tenancit. A cached value is returned only
    /// after a 304 for the same canonical hostname.
    pub async fn identify(&self, hostname: &str) -> Result<String, IdentifyError> {
        let hostname = canonical_hostname(hostname).ok_or(IdentifyError::NotFound)?;
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&hostname)
            .cloned();
        let mut url = self.endpoint.clone();
        url.query_pairs_mut().append_pair("hostname", &hostname);
        let mut request = self
            .client
            .get(url)
            .header(AUTHORIZATION, self.bearer.clone());
        if let Some(identity) = &cached {
            request = request.header(IF_NONE_MATCH, identity.etag.as_str());
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| IdentifyError::Unavailable)?;

        match response.status() {
            StatusCode::NOT_MODIFIED => {
                return cached
                    .map(|identity| identity.slug)
                    .ok_or(IdentifyError::Unavailable);
            }
            StatusCode::NOT_FOUND => {
                self.forget(&hostname);
                return Err(IdentifyError::NotFound);
            }
            StatusCode::OK => {}
            _ => {
                self.forget(&hostname);
                return Err(IdentifyError::Unavailable);
            }
        }

        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            self.forget(&hostname);
            return Err(IdentifyError::Unavailable);
        }
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= 128 && value.starts_with('"') && value.ends_with('"'))
            .map(str::to_owned);
        let mut body = Vec::new();
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => {
                    self.forget(&hostname);
                    return Err(IdentifyError::Unavailable);
                }
            };
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                self.forget(&hostname);
                return Err(IdentifyError::Unavailable);
            }
            body.extend_from_slice(&chunk);
        }
        let payload: IdentifyResponse = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => {
                self.forget(&hostname);
                return Err(IdentifyError::Unavailable);
            }
        };
        if !valid_tenant_slug(&payload.tenant_slug) {
            self.forget(&hostname);
            return Err(IdentifyError::Unavailable);
        }

        if let Some(etag) = etag {
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if cache.len() >= MAX_CACHE_ENTRIES && !cache.contains_key(&hostname) {
                cache.clear();
            }
            cache.insert(
                hostname,
                CachedIdentity {
                    slug: payload.tenant_slug.clone(),
                    etag,
                },
            );
        } else {
            self.forget(&hostname);
        }
        Ok(payload.tenant_slug)
    }

    fn forget(&self, hostname: &str) {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(hostname);
    }
}

/// Match Tenancit's ASCII DNS contract after removing an optional numeric port.
pub fn canonical_hostname(authority: &str) -> Option<String> {
    let authority = authority.trim();
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.parse::<u16>().is_ok() => host,
        Some(_) => return None,
        None => authority,
    };
    let hostname = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if hostname.is_empty() || hostname.len() > 253 {
        return None;
    }
    for label in hostname.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return None;
        }
    }
    Some(hostname)
}

pub fn valid_tenant_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 63
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::{Query, State};
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    #[derive(Clone)]
    struct StubState(Arc<Mutex<(String, String, usize)>>);

    async fn stub_identify(
        State(state): State<StubState>,
        headers: HeaderMap,
        Query(query): Query<HashMap<String, String>>,
    ) -> Response {
        if headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer test-token")
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        if query.get("hostname").map(String::as_str) != Some("acme.example.com") {
            return StatusCode::NOT_FOUND.into_response();
        }
        let mut value = state.0.lock().unwrap();
        value.2 += 1;
        if headers
            .get(IF_NONE_MATCH)
            .and_then(|header| header.to_str().ok())
            == Some(value.1.as_str())
        {
            return StatusCode::NOT_MODIFIED.into_response();
        }
        (
            [(ETAG, value.1.clone())],
            Json(json!({ "tenantSlug": value.0.clone() })),
        )
            .into_response()
    }

    async fn start_stub(app: Router) -> (Url, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Url::parse(&format!(
            "http://{}/v1/identify",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (endpoint, task)
    }

    #[tokio::test]
    async fn revalidates_each_request_and_replaces_reassigned_tenant() {
        let state = StubState(Arc::new(Mutex::new(("acme".into(), "\"v1\"".into(), 0))));
        let (endpoint, server) = start_stub(
            Router::new()
                .route("/v1/identify", get(stub_identify))
                .with_state(state.clone()),
        )
        .await;
        let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
        assert_eq!(
            client.identify("ACME.example.com.:443").await,
            Ok("acme".into())
        );
        assert_eq!(client.identify("acme.example.com").await, Ok("acme".into()));
        {
            let mut value = state.0.lock().unwrap();
            value.0 = "bravo".into();
            value.1 = "\"v2\"".into();
        }
        assert_eq!(
            client.identify("acme.example.com").await,
            Ok("bravo".into())
        );
        assert_eq!(state.0.lock().unwrap().2, 3);
        server.abort();
    }

    #[tokio::test]
    async fn refuses_304_without_a_prior_value_for_that_host() {
        let (endpoint, server) = start_stub(
            Router::new().route("/v1/identify", get(|| async { StatusCode::NOT_MODIFIED })),
        )
        .await;
        let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
        assert_eq!(
            client.identify("acme.example.com").await,
            Err(IdentifyError::Unavailable)
        );
        server.abort();
    }

    #[tokio::test]
    async fn classifies_missing_host_and_provider_failure() {
        let (endpoint, server) = start_stub(
            Router::new().route("/v1/identify", get(|| async { StatusCode::NOT_FOUND })),
        )
        .await;
        let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
        assert_eq!(
            client.identify("acme.example.com").await,
            Err(IdentifyError::NotFound)
        );
        server.abort();

        let (endpoint, server) = start_stub(Router::new().route(
            "/v1/identify",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        ))
        .await;
        let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
        assert_eq!(
            client.identify("acme.example.com").await,
            Err(IdentifyError::Unavailable)
        );
        server.abort();
    }

    #[tokio::test]
    async fn service_auth_rate_limit_and_bad_payload_never_supply_identity() {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let (endpoint, server) =
                start_stub(Router::new().route("/v1/identify", get(move || async move { status })))
                    .await;
            let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
            assert_eq!(
                client.identify("acme.example.com").await,
                Err(IdentifyError::Unavailable)
            );
            server.abort();
        }

        for body in [
            json!({ "tenantSlug": "Acme" }),
            json!({ "tenantSlug": "acme", "resource": "must-not-be-read" }),
        ] {
            let (endpoint, server) = start_stub(
                Router::new().route("/v1/identify", get(move || async move { Json(body) })),
            )
            .await;
            let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
            assert_eq!(
                client.identify("acme.example.com").await,
                Err(IdentifyError::Unavailable)
            );
            server.abort();
        }
    }

    #[tokio::test]
    async fn connection_failure_does_not_use_a_previous_identity() {
        let state = StubState(Arc::new(Mutex::new(("acme".into(), "\"v1\"".into(), 0))));
        let (endpoint, server) = start_stub(
            Router::new()
                .route("/v1/identify", get(stub_identify))
                .with_state(state),
        )
        .await;
        let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
        assert_eq!(client.identify("acme.example.com").await, Ok("acme".into()));
        server.abort();
        assert_eq!(
            client.identify("acme.example.com").await,
            Err(IdentifyError::Unavailable)
        );
    }

    #[tokio::test]
    async fn identify_timeout_is_unavailable() {
        let (endpoint, server) = start_stub(Router::new().route(
            "/v1/identify",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(900)).await;
                Json(json!({ "tenantSlug": "acme" }))
            }),
        ))
        .await;
        let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
        assert_eq!(
            client.identify("acme.example.com").await,
            Err(IdentifyError::Unavailable)
        );
        server.abort();
    }

    fn padded_identify_body(total_bytes: usize) -> Vec<u8> {
        let mut body = br#"{"tenantSlug":"acme"}"#.to_vec();
        assert!(
            total_bytes >= body.len(),
            "target must fit the base payload"
        );
        body.extend(std::iter::repeat_n(b' ', total_bytes - body.len()));
        body
    }

    #[tokio::test]
    async fn identify_enforces_1024_byte_ceiling_at_the_boundary() {
        let fitting = padded_identify_body(MAX_RESPONSE_BYTES);
        let (endpoint, server) = start_stub(Router::new().route(
            "/v1/identify",
            get(move || {
                let body = fitting.clone();
                async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                        .into_response()
                }
            }),
        ))
        .await;
        let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
        assert_eq!(client.identify("acme.example.com").await, Ok("acme".into()));
        server.abort();

        let oversized = padded_identify_body(MAX_RESPONSE_BYTES + 1);
        let (endpoint, server) = start_stub(Router::new().route(
            "/v1/identify",
            get(move || {
                let body = oversized.clone();
                async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        body,
                    )
                        .into_response()
                }
            }),
        ))
        .await;
        let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
        assert_eq!(
            client.identify("acme.example.com").await,
            Err(IdentifyError::Unavailable)
        );
        server.abort();
    }

    #[tokio::test]
    async fn identify_rejects_oversized_chunked_body_without_trusted_length() {
        let oversized = padded_identify_body(MAX_RESPONSE_BYTES + 1);
        let first = oversized[..MAX_RESPONSE_BYTES].to_vec();
        let second = oversized[MAX_RESPONSE_BYTES..].to_vec();
        let (endpoint, server) = start_stub(Router::new().route(
            "/v1/identify",
            get(move || {
                let first = first.clone();
                let second = second.clone();
                async move {
                    let stream = futures_util::stream::iter(vec![
                        Ok::<_, std::io::Error>(bytes::Bytes::from(first)),
                        Ok::<_, std::io::Error>(bytes::Bytes::from(second)),
                    ]);
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        Body::from_stream(stream),
                    )
                        .into_response()
                }
            }),
        ))
        .await;
        let client = TenantIdentityClient::new(endpoint, "test-token").unwrap();
        assert_eq!(
            client.identify("acme.example.com").await,
            Err(IdentifyError::Unavailable)
        );
        server.abort();
    }

    #[test]
    fn host_canonicalization_matches_tenancit_dns_rules() {
        assert_eq!(
            canonical_hostname("ACME.Example.COM.:443"),
            Some("acme.example.com".into())
        );
        assert_eq!(
            canonical_hostname("acme.example.com"),
            Some("acme.example.com".into())
        );
        for invalid in [
            "",
            "acme..com",
            "acme..",
            "-acme.com",
            "acme_.com",
            "acme.com:bad",
            "[::1]",
        ] {
            assert_eq!(canonical_hostname(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn slug_validation_matches_tenancit_contract() {
        for valid in ["a", "acme", "client-2"] {
            assert!(valid_tenant_slug(valid), "{valid}");
        }
        for invalid in ["", "Acme", "a--b", "-a", "a-", "a_b", "a.b"] {
            assert!(!valid_tenant_slug(invalid), "{invalid}");
        }
    }

    #[test]
    fn rejects_insecure_non_loopback_and_non_identity_endpoints() {
        let insecure = Url::parse("http://tenancit.internal/v1/identify").unwrap();
        assert!(TenantIdentityClient::new(insecure, "test-token").is_err());
        let wrong_path = Url::parse("https://tenancit.example/v1/resolve").unwrap();
        assert!(TenantIdentityClient::new(wrong_path, "test-token").is_err());
    }
}
