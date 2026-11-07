//! Private SQLD egress authority (EDG-52/54 07d1).
//!
//! A host-side grant that pins ONE owner (worker + namespace), ONE
//! plain-HTTP SQLD origin and ONE vault credential, resolved privately at
//! use. This slice has no network, no DB runtime, no manifest and no
//! provisioner: it is the closed API the 07d2/3 consumer receives. The
//! authority is immutable and deliberately opaque (no `Debug`, no serde,
//! no `Default`), shared by `Arc` in the future consumer. The 07d2b
//! consumer (`EgressDatabaseBridge`) runs that authority through the
//! shared egress guard as a private `WasmHttpTransport`.

use std::sync::Arc;

use async_trait::async_trait;
use edger_core::{AppSecretResolver, EgressSettings};
use edger_isolation::wasm::{WasmHttpError, WasmHttpRequest, WasmHttpResponse, WasmHttpTransport};

use crate::egress_guard::{EgressGuard, EgressWorkerContext};

/// Fail-closed error of the SQLD grant. Static payloads only: never a
/// secret value, a credential name, an origin or an owner identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressDatabaseGrantError {
    /// The grant shape is malformed (origin, owner, namespace or
    /// credential name).
    Invalid,
    /// The calling context is not the grant's owner.
    Denied,
    /// The credential cannot be produced (no resolver, or a missing,
    /// empty or invalid value); closed without any hint.
    Unavailable,
}

impl std::fmt::Display for EgressDatabaseGrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => write!(f, "invalid egress database grant"),
            Self::Denied => write!(f, "egress database grant denied"),
            Self::Unavailable => write!(f, "egress database credential unavailable"),
        }
    }
}

impl std::error::Error for EgressDatabaseGrantError {}

/// NUL, CR and LF are the only control shapes this slice rejects (in the
/// SQLD namespace and in a resolved credential value); no further regex.
fn contains_forbidden_control(bytes: &[u8]) -> bool {
    bytes.iter().any(|b| matches!(b, 0x00 | 0x0d | 0x0a))
}

/// Private SQLD egress authority for ONE host binding (EDG-52/54 07d1).
///
/// All fields are private: the grant and the credential it can produce
/// never render into logs, panics or serialized state.
pub struct EgressDatabaseGrant {
    /// The owner's worker name (the app; the DB grant is per app and
    /// survives a new version of the same app — version and process id
    /// are deliberately NOT stored).
    owner_worker: String,
    /// The owner's namespace, stored exact as the context carries it.
    owner_namespace: Option<String>,
    /// The normalized SQLD origin: plain `http`, path `/`, no userinfo,
    /// query or fragment; the fixed port comes from the URL (the default
    /// 80 included; the known admin 9090 refused).
    origin: url::Url,
    /// The SQLD namespace, sent base64(STANDARD_NO_PAD) as
    /// `x-namespace-bin` — the codec the pinned SQLD
    /// `try_namespace_from_metadata` decodes; separate from the
    /// worker's RBAC namespace.
    namespace: String,
    /// The vault credential name, validated by the core name rule.
    credential_name: String,
    /// The vault resolver; `None` keeps the grant valid but the
    /// credential fails closed at use.
    resolver: Option<Arc<dyn AppSecretResolver>>,
}

impl EgressDatabaseGrant {
    /// Builds the grant WITHOUT ever consulting the resolver: this is
    /// authority creation, not credential use.
    pub fn new(
        owner: &EgressWorkerContext,
        origin: &str,
        namespace: &str,
        credential_name: &str,
        resolver: Option<Arc<dyn AppSecretResolver>>,
    ) -> Result<Self, EgressDatabaseGrantError> {
        // The owner is the app identity: an empty worker name is an empty
        // owner. The namespace stays exactly as the context carries it.
        if owner.worker.is_empty() {
            return Err(EgressDatabaseGrantError::Invalid);
        }
        let origin = parse_sqld_origin(origin)?;
        // The SQLD namespace is carried in a header; reject the empty and
        // the control shapes only — no SQLD namespace regex beyond that.
        if namespace.is_empty() || contains_forbidden_control(namespace.as_bytes()) {
            return Err(EgressDatabaseGrantError::Invalid);
        }
        // The existing core name rule (shape + the reserved `EDGER_`
        // prefix); a credential/resolver `None` does NOT invalidate the
        // authority — it fails closed at use.
        edger_core::validate_app_secret_name(credential_name)
            .map_err(|_| EgressDatabaseGrantError::Invalid)?;
        Ok(Self {
            owner_worker: owner.worker.clone(),
            owner_namespace: owner.namespace.clone(),
            origin,
            namespace: namespace.to_owned(),
            credential_name: credential_name.to_owned(),
            resolver,
        })
    }

    /// Exact owner: worker AND namespace. Version and process id are
    /// ignored on purpose — the grant is per app and survives a new
    /// version of the same app.
    pub fn matches_owner(&self, ctx: &EgressWorkerContext) -> bool {
        ctx.worker == self.owner_worker && ctx.namespace == self.owner_namespace
    }

    /// Whether this grant covers `request_url` for `ctx`: the owner must
    /// match and the request must be a plain-HTTP URL without userinfo
    /// or fragment whose NORMALIZED origin (scheme, host, fixed port)
    /// equals the grant's. Path and query are free. A pure predicate: no
    /// DNS, no network, no secret lookup, and it never rewrites a URL of
    /// another origin — admin port, https, subdomain and credentials are
    /// all denials.
    pub fn allows_request(&self, ctx: &EgressWorkerContext, request_url: &str) -> bool {
        if !self.matches_owner(ctx) {
            return false;
        }
        let Ok(request) = url::Url::parse(request_url) else {
            return false;
        };
        if request.scheme() != "http"
            || !request.username().is_empty()
            || request.password().is_some()
            || request.fragment().is_some()
        {
            return false;
        }
        request.origin() == self.origin.origin()
    }

    /// The normalized origin, for operator metadata; never a secret.
    pub fn origin(&self) -> &url::Url {
        &self.origin
    }

    /// The SQLD namespace as `x-namespace-bin` (STANDARD_NO_PAD base64 of
    /// the namespace bytes — the pinned SQLD
    /// `try_namespace_from_metadata` decodes exactly this profile). Never
    /// the worker's RBAC namespace, never a guest header.
    pub fn namespace_header(&self) -> http::HeaderValue {
        let encoded = base64::engine::Engine::encode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            self.namespace.as_bytes(),
        );
        // NO_PAD base64 is `[A-Za-z0-9+/]`, always a valid header value.
        http::HeaderValue::from_str(&encoded)
            .expect("NO_PAD base64 output is always a valid header value")
    }

    /// The `Authorization` header for the grant's credential, resolved
    /// FRESH on every call (rotation per execution; the token is never
    /// cached). The owner is checked first; a foreign context is denied
    /// before any vault lookup. The value is zeroized on drop and the
    /// header is marked sensitive; the static error payloads carry no
    /// value, name, origin or URL.
    pub async fn authorization(
        &self,
        ctx: &EgressWorkerContext,
    ) -> Result<http::HeaderValue, EgressDatabaseGrantError> {
        if !self.matches_owner(ctx) {
            return Err(EgressDatabaseGrantError::Denied);
        }
        let Some(resolver) = self.resolver.as_ref() else {
            return Err(EgressDatabaseGrantError::Unavailable);
        };
        // Always the OWNER's worker and the credential name — never the
        // caller's version, process id, origin or URL.
        let Some(value) = resolver
            .resolve_secret(&self.owner_worker, &self.credential_name)
            .await
        else {
            return Err(EgressDatabaseGrantError::Unavailable);
        };
        // Zeroize the plaintext from the moment it is bound.
        let value = zeroize::Zeroizing::new(value);
        if value.is_empty() || contains_forbidden_control(value.as_bytes()) {
            return Err(EgressDatabaseGrantError::Unavailable);
        }
        let token = &*value;
        let mut header = http::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| EgressDatabaseGrantError::Unavailable)?;
        header.set_sensitive(true);
        Ok(header)
    }
}

/// The SQLD service origin is plain HTTP
/// (deploy/production/sqld.yaml: user 8080, admin 9090). Only the shapes
/// a SQLD user dial needs are admitted: `http`, a valid host, the URL's
/// fixed port (the default 80 included; the known admin 9090 refused),
/// no userinfo, query or fragment, and the path `/`.
fn parse_sqld_origin(origin: &str) -> Result<url::Url, EgressDatabaseGrantError> {
    let invalid = EgressDatabaseGrantError::Invalid;
    let url = url::Url::parse(origin).map_err(|_| invalid)?;
    if url.scheme() != "http" {
        return Err(invalid);
    }
    let Some(host) = url.host_str() else {
        return Err(invalid);
    };
    if host.is_empty() {
        return Err(invalid);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid);
    }
    if url.port_or_known_default() == Some(9090) {
        return Err(invalid);
    }
    if url.query().is_some() || url.fragment().is_some() || url.path() != "/" {
        return Err(invalid);
    }
    Ok(url)
}

/// (EDG-52/54 07d2b) The host binding of the private SQLD transport: the
/// shared egress guard running the grant's authority for ONE worker
/// identity. Deliberately without `Debug`/serde: the grant, the settings
/// and the guard are policy material, and the requests/answers are
/// payload.
pub struct EgressDatabaseBridge {
    guard: Arc<EgressGuard>,
    ctx: Arc<EgressWorkerContext>,
    settings: Arc<EgressSettings>,
    grant: Arc<EgressDatabaseGrant>,
}

impl EgressDatabaseBridge {
    /// Bind the four host-trusted inputs of every call: the shared guard
    /// (the SAME process-wide instance as the general egress — one
    /// (worker, version) semaphore map and one policy), the worker
    /// identity, the version's limits and the authority. None of them
    /// comes from a guest frame; the token is never resolved at
    /// construction (the lookup stays per call, inside the guard entry).
    pub fn new(
        guard: Arc<EgressGuard>,
        ctx: Arc<EgressWorkerContext>,
        settings: Arc<EgressSettings>,
        grant: Arc<EgressDatabaseGrant>,
    ) -> Self {
        Self {
            guard,
            ctx,
            settings,
            grant,
        }
    }
}

#[async_trait]
impl WasmHttpTransport for EgressDatabaseBridge {
    /// One guest request through the grant: the exact owner and origin
    /// BEFORE the credential lookup, DNS or the dial; the fresh per-call
    /// credential; no service redirects; the effective
    /// `min(guest, responseMax)` cap before the head and in the
    /// collection (no partial body is a success).
    async fn fetch(
        &self,
        request: WasmHttpRequest,
        response_body_limit: usize,
    ) -> Result<WasmHttpResponse, WasmHttpError> {
        self.guard
            .fetch_database(
                request,
                self.ctx.clone(),
                self.settings.clone(),
                self.grant.clone(),
                response_body_limit,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn owner_ctx(worker: &str, namespace: Option<&str>) -> EgressWorkerContext {
        EgressWorkerContext {
            worker: worker.to_owned(),
            version: "1.0.0".to_owned(),
            namespace: namespace.map(str::to_owned),
            process_id: "p1".to_owned(),
        }
    }

    fn new_grant(owner: &EgressWorkerContext, origin: &str) -> EgressDatabaseGrant {
        EgressDatabaseGrant::new(owner, origin, "sqlns", "SQLD_TOKEN", None).expect("valid shape")
    }

    /// Records every lookup (count, and the exact `(worker, name)`
    /// arguments) and answers `values[n]` on the n-th call, `None` when
    /// exhausted (fail closed).
    struct CountingResolver {
        calls: Arc<AtomicUsize>,
        args: Arc<Mutex<Vec<(String, String)>>>,
        values: Arc<Mutex<VecDeque<String>>>,
    }

    /// The counting resolver's parts as named types (the clippy
    /// `type_complexity` of the raw 3-tuple signature): the resolver's
    /// arc, the call counter and the recorded `(worker, name)` arguments.
    type ResolverArc = Arc<dyn AppSecretResolver>;
    type CallCount = Arc<AtomicUsize>;
    type CallArgs = Arc<Mutex<Vec<(String, String)>>>;

    fn counting_resolver(values: &[&str]) -> (ResolverArc, CallCount, CallArgs) {
        let calls = Arc::new(AtomicUsize::new(0));
        let args = Arc::new(Mutex::new(Vec::new()));
        let stored = Arc::new(Mutex::new(VecDeque::from(
            values.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )));
        (
            Arc::new(CountingResolver {
                calls: calls.clone(),
                args: args.clone(),
                values: stored,
            }),
            calls,
            args,
        )
    }

    #[async_trait::async_trait]
    impl AppSecretResolver for CountingResolver {
        async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            self.args
                .lock()
                .unwrap()
                .push((worker.to_owned(), name.to_owned()));
            self.values.lock().unwrap().get(n).cloned()
        }
    }

    #[test]
    fn constructor_never_consults_the_resolver() {
        let (resolver, calls, _args) = counting_resolver(&["v1"]);
        let owner = owner_ctx("app", Some("ns-a"));
        let grant = EgressDatabaseGrant::new(
            &owner,
            "http://sqld.local:8080/",
            "sqlns",
            "SQLD_TOKEN",
            Some(resolver),
        )
        .expect("valid grant shape");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "new() must not resolve");
        assert_eq!(grant.origin().as_str(), "http://sqld.local:8080/");
    }

    #[test]
    fn constructor_rejects_the_hostile_origin_owner_and_name_table() {
        let owner = owner_ctx("app", Some("ns-a"));
        let origins = [
            ("notaurl", "no URL at all"),
            (
                "https://sqld.local:8080/",
                "https is not the SQLD user scheme",
            ),
            (
                "http://user:pass@sqld.local:8080/",
                "userinfo with password",
            ),
            ("http://user@sqld.local:8080/", "user without password"),
            ("http://sqld.local:9090/", "the known admin port"),
            ("http://sqld.local:8080/admin", "a path other than /"),
            ("http://sqld.local:8080/?q=1", "a query"),
            ("http://sqld.local:8080/#f", "a fragment"),
        ];
        for (origin, why) in origins {
            assert!(
                EgressDatabaseGrant::new(&owner, origin, "sqlns", "SQLD_TOKEN", None).is_err(),
                "{origin} must be refused ({why})"
            );
        }
        assert!(
            EgressDatabaseGrant::new(
                &owner_ctx("", Some("ns-a")),
                "http://sqld.local:8080/",
                "sqlns",
                "SQLD_TOKEN",
                None
            )
            .is_err(),
            "an empty owner worker"
        );
        let namespaces = [
            ("", "empty"),
            ("a\0b", "NUL"),
            ("a\nb", "LF"),
            ("a\rb", "CR"),
        ];
        for (namespace, why) in namespaces {
            assert!(
                EgressDatabaseGrant::new(
                    &owner,
                    "http://sqld.local:8080/",
                    namespace,
                    "SQLD_TOKEN",
                    None
                )
                .is_err(),
                "a namespace {why:?}",
            );
        }
        for name in ["lower", "EDGER_X", "WITH-DASH"] {
            assert!(
                EgressDatabaseGrant::new(&owner, "http://sqld.local:8080/", "sqlns", name, None)
                    .is_err(),
                "the credential name {name:?} must follow the core rule"
            );
        }
        // and the accepted shapes
        assert!(
            EgressDatabaseGrant::new(
                &owner,
                "http://sqld.local:8080/",
                "sqlns",
                "SQLD_TOKEN",
                None
            )
            .is_ok(),
            "the SQLD user port"
        );
        assert!(
            EgressDatabaseGrant::new(&owner, "http://sqld.local/", "sqlns", "SQLD_TOKEN", None)
                .is_ok(),
            "the default port 80 is allowed"
        );
        assert!(
            EgressDatabaseGrant::new(
                &owner,
                "http://127.0.0.1:8080/",
                "sqlns",
                "SQLD_TOKEN",
                None
            )
            .is_ok(),
            "an IP host"
        );
    }

    #[test]
    fn owner_matching_is_exact_and_ignores_version_and_process() {
        let owner = owner_ctx("app", Some("ns-a"));
        let grant = new_grant(&owner, "http://sqld.local:8080/");
        assert!(grant.matches_owner(&owner));
        let mut newer = owner.clone();
        newer.version = "2.0.0".to_owned();
        newer.process_id = "p2".to_owned();
        assert!(
            grant.matches_owner(&newer),
            "a new version of the same app keeps the grant"
        );
        assert!(
            !grant.matches_owner(&owner_ctx("app", Some("ns-b"))),
            "cross namespace"
        );
        assert!(
            !grant.matches_owner(&owner_ctx("app", None)),
            "no namespace"
        );
        assert!(
            !grant.matches_owner(&owner_ctx("other", Some("ns-a"))),
            "cross app"
        );
    }

    #[test]
    fn allows_request_table_origin_and_shapes() {
        let owner = owner_ctx("app", Some("ns-a"));
        let grant = new_grant(&owner, "http://sqld.local:8080/");
        // path and query are free on the normalized origin
        assert!(grant.allows_request(&owner, "http://sqld.local:8080/ready"));
        assert!(grant.allows_request(&owner, "http://sqld.local:8080/api?q=1"));
        assert!(grant.allows_request(&owner, "http://sqld.local:8080"));
        // default-port normalization
        let grant80 = new_grant(&owner, "http://sqld.local/");
        assert!(grant80.allows_request(&owner, "http://sqld.local:80/ready"));
        // denied: other scheme, subdomain, other host, admin port,
        // userinfo, fragment, unparseable
        assert!(
            !grant.allows_request(&owner, "https://sqld.local:8080/ready"),
            "https"
        );
        assert!(
            !grant.allows_request(&owner, "http://sub.sqld.local:8080/"),
            "subdomain"
        );
        assert!(
            !grant.allows_request(&owner, "http://other.sqld.local:8080/"),
            "other host"
        );
        assert!(
            !grant.allows_request(&owner, "http://sqld.local:9090/"),
            "admin port"
        );
        assert!(
            !grant.allows_request(&owner, "http://user@sqld.local:8080/"),
            "userinfo"
        );
        assert!(
            !grant.allows_request(&owner, "http://sqld.local:8080/#frag"),
            "fragment"
        );
        assert!(!grant.allows_request(&owner, "notaurl"), "unparseable");
        // a foreign owner is denied before any URL consideration
        assert!(!grant.allows_request(
            &owner_ctx("other", Some("ns-a")),
            "http://sqld.local:8080/ready"
        ));
    }

    #[tokio::test]
    async fn missing_credential_keeps_owner_and_request_but_unavailable() {
        let owner = owner_ctx("app", Some("ns-a"));
        // resolver present but the (owner, name) pair has no record
        let (resolver, _calls, _args) = counting_resolver(&[]);
        let grant = EgressDatabaseGrant::new(
            &owner,
            "http://sqld.local:8080/",
            "sqlns",
            "SQLD_TOKEN",
            Some(resolver),
        )
        .expect("a missing token does not invalidate the authority");
        assert!(grant.matches_owner(&owner));
        assert!(grant.allows_request(&owner, "http://sqld.local:8080/ready"));
        assert_eq!(
            grant.authorization(&owner).await,
            Err(EgressDatabaseGrantError::Unavailable)
        );
        // and with no resolver at all
        let grant = new_grant(&owner, "http://sqld.local:8080/");
        assert!(grant.matches_owner(&owner));
        assert!(grant.allows_request(&owner, "http://sqld.local:8080/ready"));
        assert_eq!(
            grant.authorization(&owner).await,
            Err(EgressDatabaseGrantError::Unavailable)
        );
    }

    #[tokio::test]
    async fn foreign_context_is_denied_without_lookup() {
        let owner = owner_ctx("app", Some("ns-a"));
        let (resolver, calls, args) = counting_resolver(&["v1"]);
        let grant = EgressDatabaseGrant::new(
            &owner,
            "http://sqld.local:8080/",
            "sqlns",
            "SQLD_TOKEN",
            Some(resolver),
        )
        .expect("valid grant");
        assert_eq!(
            grant.authorization(&owner_ctx("other", Some("ns-a"))).await,
            Err(EgressDatabaseGrantError::Denied)
        );
        assert_eq!(
            grant.authorization(&owner_ctx("app", Some("ns-b"))).await,
            Err(EgressDatabaseGrantError::Denied)
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the denial must happen before any vault lookup"
        );
        assert!(args.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn resolution_is_owner_scoped_and_renewed_per_call() {
        let owner = owner_ctx("app", Some("ns-a"));
        let (resolver, calls, args) = counting_resolver(&["alpha", "beta"]);
        let grant = EgressDatabaseGrant::new(
            &owner,
            "http://sqld.local:8080/",
            "sqlns",
            "SQLD_TOKEN",
            Some(resolver),
        )
        .expect("valid grant");
        let first = grant.authorization(&owner).await.expect("first value");
        let second = grant.authorization(&owner).await.expect("rotated value");
        assert_eq!(first.to_str().unwrap(), "Bearer alpha");
        assert_eq!(
            second.to_str().unwrap(),
            "Bearer beta",
            "rotation without recreating the grant"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2, "every call resolves fresh");
        // the resolver receives ONLY the owner worker and the credential name
        assert_eq!(
            *args.lock().unwrap(),
            vec![
                ("app".to_owned(), "SQLD_TOKEN".to_owned()),
                ("app".to_owned(), "SQLD_TOKEN".to_owned())
            ]
        );
    }

    #[test]
    fn namespace_header_is_exact_no_pad_base64() {
        let owner = owner_ctx("app", Some("ns-a"));
        let grant = new_grant(&owner, "http://sqld.local:8080/");
        let expected = base64::engine::Engine::encode(
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            b"sqlns",
        );
        assert_eq!(expected, "c3FsbnM");
        assert_eq!(grant.namespace_header().to_str().unwrap(), expected);
        // the worker's RBAC namespace must not leak into the header
        assert_ne!(grant.namespace_header().to_str().unwrap(), "bnMtYQ");
    }

    #[test]
    fn namespace_header_is_accepted_by_pinned_sqld_decoder() {
        // SQLD (libsql-server-v0.24.32 @ 40c272de, db_factory.rs
        // `try_namespace_from_metadata`) decodes `x-namespace-bin` with
        // base64::engine::general_purpose::STANDARD_NO_PAD. The grant's
        // header must be produced by the same profile: the NO_PAD
        // encoding of the SQLD namespace bytes — never the worker's RBAC
        // namespace, never a guest header.
        let owner = owner_ctx("app", Some("ns-a"));
        let sqld = base64::engine::general_purpose::STANDARD_NO_PAD;
        for ns in ["sqlns", "sqlnsx", "sqlnsxy"] {
            let grant =
                EgressDatabaseGrant::new(&owner, "http://sqld.local:8080/", ns, "SQLD_TOKEN", None)
                    .expect("valid shape");
            let header = grant.namespace_header().to_str().unwrap().to_owned();
            assert_eq!(
                sqld.decode(header.as_bytes()),
                Ok(ns.as_bytes().to_vec()),
                "SQLD's pinned NO_PAD decoder accepts the grant's header for {ns:?}"
            );
            // Control: the header is the SQLD namespace, never the
            // worker's RBAC namespace.
            assert_ne!(
                header,
                sqld.encode("ns-a"),
                "the RBAC namespace must not ride in x-namespace-bin"
            );
        }
        // The pinned decoder rejects the PADDED profile: a header that
        // still carries `=` (the old STANDARD encoder) does not decode.
        assert!(
            sqld.decode("c3FsbnM=").is_err(),
            "STANDARD_NO_PAD must reject the padded STANDARD output"
        );
    }

    #[tokio::test]
    async fn authorization_header_is_sensitive_and_never_debugs_the_value() {
        let owner = owner_ctx("app", Some("ns-a"));
        let (resolver, _calls, _args) = counting_resolver(&["canary-token-42"]);
        let grant = EgressDatabaseGrant::new(
            &owner,
            "http://sqld.local:8080/",
            "sqlns",
            "SQLD_TOKEN",
            Some(resolver),
        )
        .expect("valid grant");
        let header = grant.authorization(&owner).await.expect("value present");
        assert!(header.is_sensitive());
        assert_eq!(header.to_str().unwrap(), "Bearer canary-token-42");
        // the sensitive Debug must not carry the canary
        let debug = format!("{header:?}");
        assert!(!debug.contains("canary-token-42"), "Debug leaked the value");
    }

    #[tokio::test]
    async fn authorization_rejects_invalid_token_shapes() {
        let owner = owner_ctx("app", Some("ns-a"));
        for value in ["", "a\nb", "a\rb", "a\0b"] {
            let (resolver, _calls, _args) = counting_resolver(&[value]);
            let grant = EgressDatabaseGrant::new(
                &owner,
                "http://sqld.local:8080/",
                "sqlns",
                "SQLD_TOKEN",
                Some(resolver),
            )
            .expect("valid grant");
            assert_eq!(
                grant.authorization(&owner).await,
                Err(EgressDatabaseGrantError::Unavailable),
                "token shape {value:?}"
            );
        }
    }
}
