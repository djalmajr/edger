//! Egress guard service (EDG-38, S2a).
//!
//! A Unix-socket HTTP/1.1 service that the persistent worker processes use
//! to reach user-chosen HTTPS destinations. The app sends the request with
//! the fake host [`EGRESS_GUARD_HOST`] (the `Host` header equals it, in
//! absolute-form or origin-form) and the real destination in the
//! `x-edger-egress-url` header. Both HTTP/1.1 framings are `hyper`: the
//! worker side is `hyper::server::conn::http1` over the accepted
//! `UnixStream`, the destination side is `hyper::client::conn::http1` over
//! the `tokio_rustls` stream of a `TcpStream` already connected to the pinned
//! IP. Per request the guard:
//!
//! - validates the destination: absolute `https://` URL, port in
//!   [`EgressSettings::ports`], host through S1's `parse_egress_host`;
//! - resolves a `Name` destination with the injected resolver and
//!   classifies EVERY resolved IP with S1's `classify_egress_ip` plus the
//!   operator-configured block CIDRs; one refused IP refuses the whole
//!   request (`EGRESS_HOST_BLOCKED`);
//! - connects to the first accepted IP — the same one it classified, never
//!   re-resolving — with the hostname's SNI, over `tokio-rustls`;
//! - follows redirects (301/302/303/307/308 with a `Location`, a relative
//!   `Location` resolved against the current URL) revalidating every hop,
//!   up to [`MAX_REDIRECTS`] hops; `Authorization`, `Cookie` and
//!   `Proxy-Authorization` are dropped when a hop changes the origin (the
//!   `fetch` behavior), so a redirect cannot carry the app's credentials to
//!   another host;
//! - applies the `EgressSettings` limits: `connect` covers TCP plus TLS and
//!   `headers` runs until the response head (either expiring before the
//!   head answers 504 `EGRESS_TIMEOUT`); `idle` is time without body bytes
//!   and `total` the whole exchange (a post-head expiry cuts the body with
//!   an error); the response body is capped at `max_response_bytes` (a
//!   `Content-Length` over the cap answers 502 `EGRESS_RESPONSE_TOO_LARGE`
//!   before the head; a streaming body over the cap is cut with an error);
//! - shares a semaphore per (worker, version) across the processes of the
//!   version, answering 429 `EGRESS_LIMIT` immediately when no slot is
//!   free (no queue);
//! - streams the destination's response back (status, headers without the
//!   hop-by-hop ones, body) with explicit framing: the destination's
//!   `Content-Length` when it sent one (and it fits the cap), chunked
//!   otherwise (EOF-delimited and chunked destination bodies).
//!
//! The total deadline of a streaming call is supervised by a timer of its
//! own (not only checked when `hyper` polls the body): if the worker stops
//! reading, `hyper` stops polling, so at the deadline the supervisor reports
//! the `timeout`, releases the slot and tears the worker connection (and with
//! it the destination connection) down. A call whose worker connection is
//! dropped before any result (during DNS, connect, TLS, the head or the
//! request-body read) is reported too, as an `error` with the reason
//! `worker_disconnected`: every admitted request records exactly one event and
//! one metric sample.
//!
//! A cut after the head (idle, total, size cap or the destination dropping
//! the connection) ends the response body with an ERROR: the body the
//! worker's `hyper` serves stops with an error, the connection closes
//! without the terminating chunk (or with fewer bytes than the declared
//! `Content-Length`), and any HTTP client sees an incomplete body — never a
//! silent truncation. There is no `SO_LINGER` trick and no extra bytes.
//!
//! Guard refusals answer with `x-edger-egress-error: <code>` and a JSON
//! body `{"error": "<code>", "message": "..."}` whose message never echoes
//! the value of any request header. Every request that reaches the service
//! records ONE `worker.egress` operational event (in the `OperationalStore`
//! when the guard is built with one, and always as a `tracing` line): worker,
//! version, namespace, process id, destination host and port, connected IP,
//! final status, duration, bytes received, redirect count, outcome
//! (`ok`/`blocked`/`error`/`timeout`/`limit`) and the code when there is
//! one — never a header (neither name nor value), no path, no query string
//! and no body. The destination host/port, the connected IP, the byte count
//! and the redirect count ride the event's `message` field as stable
//! `key=value` tokens (the event struct's fixed fields cannot be extended
//! here: every other construction site of `OperationalEventInput` is in a
//! module owned by another slice). A request `hyper` itself refuses while
//! parsing (a malformed head, an ambiguous `Content-Length`, a header value
//! with a line break) is answered 400 by `hyper` and closed before the
//! service sees it, so it records no event.
//!
//! S2a ships the service standalone: `serve` runs on a caller-supplied
//! [`tokio::net::UnixListener`] and is NOT wired to the worker spawn. The
//! spawn integration (socket creation, `EDGER_EGRESS_SOCKET` /
//! `EDGER_EGRESS_HOST` injection, the `unix:` `--allow-net` entry, the
//! `/metrics` wiring of `egress_metrics_prometheus`) is S2b.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    classify_egress_ip, parse_egress_host, EgressBlock, EgressCidr, EgressHost, EgressSettings,
};
use http::header::{
    HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE,
    COOKIE, HOST, LOCATION, PROXY_AUTHORIZATION, TRANSFER_ENCODING,
};
use http::{Method, Request, Response, StatusCode, Uri, Version};
use http_body::{Body, Frame, SizeHint};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio::time::{sleep_until, timeout_at, Instant, Sleep};
use tokio_rustls::TlsConnector;
use tracing::{info, warn};
use url::{Host, Url};

use crate::metrics::EgressMetrics;
use crate::observability::{
    OperationalEventInput, OperationalEventLevel, OperationalEventSource, OperationalStore,
};

/// The fake host the worker dials; it never resolves.
pub const EGRESS_GUARD_HOST: &str = "edger-egress.invalid";

/// The destination URL header of the guard protocol.
pub const EGRESS_URL_HEADER: &str = "x-edger-egress-url";

/// Inbound request body cap, buffered in full so 307/308 can replay it.
pub const MAX_REQUEST_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Redirect hops followed before `EGRESS_TOO_MANY_REDIRECTS`: up to 5 hops
/// are followed; a sixth redirect answer is refused.
pub const MAX_REDIRECTS: u32 = 5;

/// Head cap (request line + headers) on both HTTP/1.1 sides.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// Pause after a failed `accept` (a transient resource error such as
/// `EMFILE`), so the accept loop never spins.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

/// Error codes of the guard (contract section 4). Present in the
/// `x-edger-egress-error` response header and the event `code`.
pub const EGRESS_BAD_TARGET: &str = "EGRESS_BAD_TARGET";
pub const EGRESS_PORT_BLOCKED: &str = "EGRESS_PORT_BLOCKED";
pub const EGRESS_HOST_BLOCKED: &str = "EGRESS_HOST_BLOCKED";
pub const EGRESS_DNS_ERROR: &str = "EGRESS_DNS_ERROR";
pub const EGRESS_UPSTREAM_ERROR: &str = "EGRESS_UPSTREAM_ERROR";
pub const EGRESS_TOO_MANY_REDIRECTS: &str = "EGRESS_TOO_MANY_REDIRECTS";
pub const EGRESS_RESPONSE_TOO_LARGE: &str = "EGRESS_RESPONSE_TOO_LARGE";
pub const EGRESS_TIMEOUT: &str = "EGRESS_TIMEOUT";
pub const EGRESS_LIMIT: &str = "EGRESS_LIMIT";
pub const EGRESS_REQUEST_TOO_LARGE: &str = "EGRESS_REQUEST_TOO_LARGE";

/// The `worker.egress` outcome and the `edger_egress_requests_total` label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EgressOutcome {
    /// A destination response was forwarded (any upstream HTTP status).
    Ok,
    /// The guard's policy refused the request before dialing.
    Blocked,
    /// An upstream or protocol failure.
    Error,
    /// A guard timeout (a pre-head 504, or a post-head idle/total cut).
    Timeout,
    /// The per-version concurrency limit (`maxConcurrent`).
    Limit,
}

impl EgressOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            EgressOutcome::Ok => "ok",
            EgressOutcome::Blocked => "blocked",
            EgressOutcome::Error => "error",
            EgressOutcome::Timeout => "timeout",
            EgressOutcome::Limit => "limit",
        }
    }
}

/// A pre-head refusal: the guard answers with the status and the code and
/// stops before (or without) touching the destination. The message is a
/// fixed, value-free string — it never echoes the value of any request
/// header.
#[derive(Debug)]
struct Refusal {
    status: u16,
    code: &'static str,
    message: String,
    outcome: EgressOutcome,
}

impl Refusal {
    fn bad_target(message: &'static str) -> Self {
        Self {
            status: 400,
            code: EGRESS_BAD_TARGET,
            message: message.to_string(),
            outcome: EgressOutcome::Blocked,
        }
    }

    fn port_blocked() -> Self {
        Self {
            status: 403,
            code: EGRESS_PORT_BLOCKED,
            message: "destination_port_not_allowed".to_string(),
            outcome: EgressOutcome::Blocked,
        }
    }

    fn host_blocked(reason: EgressBlock) -> Self {
        Self {
            status: 403,
            code: EGRESS_HOST_BLOCKED,
            message: format!("resolved_ip_blocked:{reason}"),
            outcome: EgressOutcome::Blocked,
        }
    }

    fn dns_error() -> Self {
        Self {
            status: 502,
            code: EGRESS_DNS_ERROR,
            message: "dns_resolution_failed".to_string(),
            outcome: EgressOutcome::Error,
        }
    }

    fn upstream_error() -> Self {
        Self {
            status: 502,
            code: EGRESS_UPSTREAM_ERROR,
            message: "upstream_error".to_string(),
            outcome: EgressOutcome::Error,
        }
    }

    fn too_many_redirects() -> Self {
        Self {
            status: 502,
            code: EGRESS_TOO_MANY_REDIRECTS,
            message: "too_many_redirects".to_string(),
            outcome: EgressOutcome::Error,
        }
    }

    fn response_too_large() -> Self {
        Self {
            status: 502,
            code: EGRESS_RESPONSE_TOO_LARGE,
            message: "response_content_length_over_limit".to_string(),
            outcome: EgressOutcome::Error,
        }
    }

    fn timeout() -> Self {
        Self {
            status: 504,
            code: EGRESS_TIMEOUT,
            message: "connect_or_headers_timeout".to_string(),
            outcome: EgressOutcome::Timeout,
        }
    }

    fn limit() -> Self {
        Self {
            status: 429,
            code: EGRESS_LIMIT,
            message: "concurrency_limit_reached".to_string(),
            outcome: EgressOutcome::Limit,
        }
    }

    fn request_too_large() -> Self {
        Self {
            status: 413,
            code: EGRESS_REQUEST_TOO_LARGE,
            message: "request_body_over_limit".to_string(),
            outcome: EgressOutcome::Blocked,
        }
    }
}

/// An injectable DNS resolver for the guard. The default
/// ([`SystemEgressResolver`]) uses `tokio::net::lookup_host`; tests install
/// a fake to prove rebinding: the guard dials the IP it classified and
/// never re-resolves the name.
#[async_trait]
pub trait EgressResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<IpAddr>>;
}

/// The default resolver: the OS resolver (`getaddrinfo` through tokio).
#[derive(Clone, Debug, Default)]
pub struct SystemEgressResolver;

#[async_trait]
impl EgressResolver for SystemEgressResolver {
    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<IpAddr>> {
        let mut ips = Vec::new();
        for addr in tokio::net::lookup_host((host, port)).await? {
            ips.push(addr.ip());
        }
        Ok(ips)
    }
}

/// Guard configuration, assembled at boot (S2b); this slice ships the struct
/// and `from_env`.
#[derive(Clone)]
pub struct EgressGuardConfig {
    /// CIDRs refused with `EgressBlock::Configured` (S1's `extra_blocked`):
    /// from `EDGER_EGRESS_BLOCK_CIDRS` (comma list) plus `EDGER_NODE_IP` as
    /// a /32. Default `10.42.0.0/16,10.43.0.0/16` (already inside `10/8`,
    /// kept explicit).
    pub block_cidrs: Vec<EgressCidr>,
    /// DEV-ONLY CIDRs that override the classification — they release
    /// ranges the classifier would refuse. From `EDGER_EGRESS_ALLOW_CIDRS`,
    /// empty by default; a non-empty set warns once at boot.
    pub allow_cidrs: Vec<EgressCidr>,
    /// The resolver for `Name` destinations (default: the OS resolver).
    pub resolver: Arc<dyn EgressResolver>,
    /// The TLS root store for the outbound connections. The default is
    /// `webpki-roots` (the static Mozilla CA set already in the lockfile):
    /// deterministic in the production container and on local dev, with no
    /// dependence on the node's system trust store (which is absent or
    /// varied in slim images). Tests install a local CA here.
    pub tls_roots: RootCertStore,
}

impl fmt::Debug for EgressGuardConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressGuardConfig")
            .field("block_cidrs", &self.block_cidrs)
            .field("allow_cidrs", &self.allow_cidrs)
            .field("resolver", &"<resolver>")
            .field("tls_roots", &self.tls_roots)
            .finish()
    }
}

impl Default for EgressGuardConfig {
    fn default() -> Self {
        Self {
            block_cidrs: default_block_cidrs(),
            allow_cidrs: Vec::new(),
            resolver: Arc::new(SystemEgressResolver),
            tls_roots: webpki_roots(),
        }
    }
}

impl EgressGuardConfig {
    /// Read `EDGER_EGRESS_BLOCK_CIDRS`, `EDGER_NODE_IP` and
    /// `EDGER_EGRESS_ALLOW_CIDRS` from the environment. See
    /// [`Self::from_values`] for the parsing rules.
    pub fn from_env() -> Self {
        Self::from_values(
            std::env::var("EDGER_EGRESS_BLOCK_CIDRS").ok().as_deref(),
            std::env::var("EDGER_NODE_IP").ok().as_deref(),
            std::env::var("EDGER_EGRESS_ALLOW_CIDRS").ok().as_deref(),
        )
    }

    /// Build the config from the raw env values (the testable core of
    /// `from_env`). Comma lists with whitespace trimmed; invalid entries
    /// are skipped with a `warn`. A set block list that yields no valid
    /// CIDR falls back to the k3s defaults, and `EDGER_NODE_IP` (a bare
    /// IP) is appended as a /32 when set and valid.
    pub fn from_values(block: Option<&str>, node_ip: Option<&str>, allow: Option<&str>) -> Self {
        let mut block_cidrs = parse_cidrs("EDGER_EGRESS_BLOCK_CIDRS", block);
        if block_cidrs.is_empty() {
            if block.map(str::trim).is_some_and(|value| !value.is_empty()) {
                warn!(
                    "edger egress: EDGER_EGRESS_BLOCK_CIDRS set but no valid CIDR in it; using the k3s defaults"
                );
            }
            block_cidrs = default_block_cidrs();
        }
        if let Some(ip_text) = node_ip.map(str::trim).filter(|value| !value.is_empty()) {
            match ip_text.parse::<IpAddr>() {
                Ok(ip) => match EgressCidr::parse(&ip.to_string()) {
                    Some(cidr) => block_cidrs.push(cidr),
                    None => warn!("edger egress: ignoring invalid EDGER_NODE_IP {ip_text:?}"),
                },
                Err(_) => warn!("edger egress: ignoring invalid EDGER_NODE_IP {ip_text:?}"),
            }
        }
        let allow_cidrs = parse_cidrs("EDGER_EGRESS_ALLOW_CIDRS", allow);
        Self {
            block_cidrs,
            allow_cidrs,
            resolver: Arc::new(SystemEgressResolver),
            tls_roots: webpki_roots(),
        }
    }
}

fn default_block_cidrs() -> Vec<EgressCidr> {
    const DEFAULTS: [&str; 2] = ["10.42.0.0/16", "10.43.0.0/16"];
    DEFAULTS
        .iter()
        .filter_map(|text| EgressCidr::parse(text))
        .collect()
}

fn webpki_roots() -> RootCertStore {
    RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().map(ToOwned::to_owned))
}

fn parse_cidrs(name: &str, value: Option<&str>) -> Vec<EgressCidr> {
    let Some(value) = value else {
        return Vec::new();
    };
    value
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            match EgressCidr::parse(part) {
                Some(cidr) => Some(cidr),
                None => {
                    warn!("edger egress: ignoring invalid CIDR {part:?} from {name}");
                    None
                }
            }
        })
        .collect()
}

/// The identity of the worker process behind one guard socket: the owner of
/// the events, the metrics and the concurrency limit (S2b sets it from the
/// spawn; the listener belongs to the process's workdir).
#[derive(Clone, Debug)]
pub struct EgressWorkerContext {
    pub worker: String,
    pub version: String,
    pub namespace: Option<String>,
    pub process_id: String,
}

type VersionKey = (String, String);

/// The body type of every answer the worker's `hyper` serves: a boxed body
/// whose error is a cut after the head.
type EgressBody = UnsyncBoxBody<Bytes, CutError>;

/// The egress guard service. One instance per `edger` process: the
/// (worker, version) concurrency semaphores and the metrics are shared
/// across the `serve` calls (one per worker process listener), so the limit
/// is per version across its processes.
#[derive(Clone)]
pub struct EgressGuard {
    config: Arc<EgressGuardConfig>,
    client_config: Arc<ClientConfig>,
    reporter: Reporter,
    semaphores: Arc<Mutex<BTreeMap<VersionKey, Arc<Semaphore>>>>,
    allow_warned: Arc<AtomicBool>,
}

impl EgressGuard {
    pub fn new(config: EgressGuardConfig) -> Self {
        // The provider is named, not auto-detected: the process-level
        // default is whatever the graph enables (today only `ring`), and an
        // ambiguous graph would make `ClientConfig::builder()` panic.
        let client_config = Arc::new(
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("ring supports the default TLS protocol versions")
                .with_root_certificates(config.tls_roots.clone())
                .with_no_client_auth(),
        );
        Self {
            config: Arc::new(config),
            client_config,
            reporter: Reporter {
                metrics: EgressMetrics::default(),
                event_store: None,
            },
            semaphores: Arc::new(Mutex::new(BTreeMap::new())),
            allow_warned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Wire the operational store that receives the `worker.egress` events
    /// (S2b passes the process's store; without it the events are only
    /// `tracing` lines).
    pub fn with_event_store(mut self, store: OperationalStore) -> Self {
        self.reporter.event_store = Some(store);
        self
    }

    /// The shared metrics (rendered at `/metrics` by
    /// `egress_metrics_prometheus`, wired by S2b).
    pub fn metrics(&self) -> &EgressMetrics {
        &self.reporter.metrics
    }

    /// Serve the listener. The future runs until it is dropped or aborted
    /// (the stop signal): dropping it closes the listener and cuts every
    /// in-flight connection of this listener (they live in a `JoinSet` owned
    /// by this future). A failed `accept` is a transient resource error
    /// (`EMFILE`, a connection aborted before accept): it is logged and the
    /// loop backs off instead of ending the service. One task per
    /// connection; each request on a connection is served independently (a
    /// request without a concurrency slot answers 429 immediately and holds
    /// nothing).
    pub async fn serve(
        &self,
        listener: tokio::net::UnixListener,
        ctx: EgressWorkerContext,
        settings: EgressSettings,
    ) {
        if !self.config.allow_cidrs.is_empty()
            && self
                .allow_warned
                .compare_exchange(false, true, AtomicOrdering::SeqCst, AtomicOrdering::SeqCst)
                .is_ok()
        {
            warn!(
                "edger egress: EDGER_EGRESS_ALLOW_CIDRS is active ({} CIDR(s)); dev-only override of the IP classification — never enable in production",
                self.config.allow_cidrs.len()
            );
        }
        let ctx = Arc::new(ctx);
        let settings = Arc::new(settings);
        let mut connections: JoinSet<()> = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((socket, _)) => {
                        let guard = self.clone();
                        let ctx = ctx.clone();
                        let settings = settings.clone();
                        connections.spawn(async move {
                            guard.serve_connection(socket, ctx, settings).await;
                        });
                    }
                    Err(err) => {
                        warn!("edger egress: accept failed: {err}");
                        tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                    }
                },
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    }

    async fn serve_connection(
        &self,
        socket: UnixStream,
        ctx: Arc<EgressWorkerContext>,
        settings: Arc<EgressSettings>,
    ) {
        let guard = self.clone();
        // The total-deadline supervisor of a streaming call signals this to
        // tear the connection down even when `hyper` has stopped polling the
        // body (the worker stopped reading): dropping the connection future
        // closes the worker socket, drops the response body and with it the
        // destination connection.
        let kill = Arc::new(Notify::new());
        let connection_kill = kill.clone();
        let service = service_fn(move |request: Request<Incoming>| {
            let guard = guard.clone();
            let ctx = ctx.clone();
            let settings = settings.clone();
            let kill = kill.clone();
            async move { Ok::<_, Infallible>(guard.handle(request, ctx, settings, kill).await) }
        });
        let connection = server_builder().serve_connection(TokioIo::new(socket), service);
        tokio::pin!(connection);
        tokio::select! {
            _ = &mut connection => {}
            _ = connection_kill.notified() => {}
        }
    }

    /// Classify one resolved IP: the dev-only allow CIDRs win outright
    /// (they release ranges the classifier would refuse), otherwise S1's
    /// classifier runs with the operator block CIDRs. A v4 CIDR catches an
    /// IPv4-mapped v6 through the canonicalized form, mirroring what
    /// `classify_egress_ip` does for `extra_blocked`.
    fn classify(&self, ip: IpAddr) -> Result<(), EgressBlock> {
        let canonical = canonical_ip(ip);
        if self
            .config
            .allow_cidrs
            .iter()
            .any(|cidr| cidr.contains(ip) || cidr.contains(canonical))
        {
            return Ok(());
        }
        classify_egress_ip(ip, &self.config.block_cidrs)
    }

    /// One request: parse, take the concurrency slot, buffer the body, then
    /// dispatch. Every exit answers (a refusal, or the destination's
    /// response) and records exactly one event.
    async fn handle(
        &self,
        request: Request<Incoming>,
        ctx: Arc<EgressWorkerContext>,
        settings: Arc<EgressSettings>,
        kill: Arc<Notify>,
    ) -> Response<EgressBody> {
        let started = Instant::now();
        let total_deadline = deadline_after(started, settings.total_timeout_ms);
        let (parts, body) = request.into_parts();

        // 1. The protocol checks (hyper already framed the request).
        let meta = match parse_request(&parts) {
            Ok(meta) => meta,
            Err(refusal) => return self.refuse(&ctx, started, refusal, None, 0),
        };

        // 2. The concurrency slot (immediate 429 when none is free — no
        //    queue). Taken before the body is read, so a refused request
        //    never buffers one.
        let Some(permit) = self.acquire_permit(&ctx, &settings) else {
            let target = hint_target(meta.destination_hint());
            return self.refuse(&ctx, started, Refusal::limit(), target, 0);
        };
        // From here the call is admitted: the guard reports it exactly once
        // — a refusal, the end of the response, or (dropped before any of
        // those) a cancellation — and releases the slot with it.
        let call = CallGuard::new(self.reporter.clone(), ctx.clone(), started, permit);

        // 3. The body, buffered (8 MiB cap) so 307/308 can replay it.
        if meta
            .declared_length
            .is_some_and(|length| length > MAX_REQUEST_BODY_BYTES)
        {
            let target = hint_target(meta.destination_hint());
            return self.refuse_admitted(&call, Refusal::request_too_large(), target.as_ref(), 0);
        }
        let collected = timeout_at_opt(
            total_deadline,
            Limited::new(body, MAX_REQUEST_BODY_BYTES as usize).collect(),
        )
        .await;
        let body = match collected {
            Ok(Ok(collected)) => collected.to_bytes(),
            Ok(Err(err)) => {
                let refusal = if err.downcast_ref::<LengthLimitError>().is_some() {
                    Refusal::request_too_large()
                } else {
                    Refusal::bad_target("invalid_request_body")
                };
                let target = hint_target(meta.destination_hint());
                return self.refuse_admitted(&call, refusal, target.as_ref(), 0);
            }
            Err(_) => {
                let target = hint_target(meta.destination_hint());
                return self.refuse_admitted(&call, Refusal::timeout(), target.as_ref(), 0);
            }
        };

        // 4. The dispatch (validation, dial, TLS, redirects, streaming).
        self.dispatch(&settings, total_deadline, meta, body, call, kill)
            .await
    }

    /// The (worker, version) semaphore shared across the processes of the
    /// version; created with the first serve's `maxConcurrent` (a version
    /// is immutable, so later serves agree). Returns `None` when no slot is
    /// free (no queueing).
    fn acquire_permit(
        &self,
        ctx: &EgressWorkerContext,
        settings: &EgressSettings,
    ) -> Option<OwnedSemaphorePermit> {
        let key: VersionKey = (ctx.worker.clone(), ctx.version.clone());
        let semaphore = {
            let mut map = self
                .semaphores
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            map.entry(key)
                .or_insert_with(|| Arc::new(Semaphore::new(settings.max_concurrent as usize)))
                .clone()
        };
        semaphore.try_acquire_owned().ok()
    }

    /// A pre-head refusal: the event/metrics sample and the error answer.
    fn refuse(
        &self,
        ctx: &EgressWorkerContext,
        started: Instant,
        refusal: Refusal,
        target: Option<HopTarget>,
        redirects: u32,
    ) -> Response<EgressBody> {
        let record = refusal_record(&refusal, target.as_ref(), redirects);
        self.reporter.report(ctx, started, &record);
        error_response(&refusal)
    }

    /// A refusal of an admitted call: reported through the call guard (which
    /// releases the slot) and answered.
    fn refuse_admitted(
        &self,
        call: &CallGuard,
        refusal: Refusal,
        target: Option<&HopTarget>,
        redirects: u32,
    ) -> Response<EgressBody> {
        call.finish_with(refusal_record(&refusal, target, redirects));
        error_response(&refusal)
    }

    async fn dispatch(
        &self,
        settings: &EgressSettings,
        total_deadline: Option<Instant>,
        meta: RequestMeta,
        body: Bytes,
        call: CallGuard,
        kill: Arc<Notify>,
    ) -> Response<EgressBody> {
        let RequestMeta {
            method: mut current_method,
            headers: mut forwarded,
            egress_url,
            ..
        } = meta;
        let mut body = Some(body);
        let mut url_text = egress_url;
        let mut redirects = 0u32;
        let mut last_target: Option<HopTarget> = None;
        let mut previous_url: Option<Url> = None;

        loop {
            let refuse = |refusal: Refusal, target: Option<&HopTarget>, redirects: u32| {
                self.refuse_admitted(&call, refusal, target, redirects)
            };
            let Some(text) = url_text.take() else {
                return refuse(
                    Refusal::bad_target("missing_egress_url"),
                    last_target.as_ref(),
                    redirects,
                );
            };
            let url = match Url::parse(&text) {
                Ok(url) => url,
                Err(_) => {
                    return refuse(
                        Refusal::bad_target("invalid_egress_url"),
                        last_target.as_ref(),
                        redirects,
                    );
                }
            };
            if url.scheme() != "https" {
                return refuse(
                    Refusal::bad_target("non_https_scheme"),
                    last_target.as_ref(),
                    redirects,
                );
            }
            // Credentials in the destination URL are never forwarded: the
            // guard refuses them (fail closed) instead of echoing them into
            // the upstream request.
            if !url.username().is_empty() || url.password().is_some() {
                return refuse(
                    Refusal::bad_target("egress_url_credentials"),
                    last_target.as_ref(),
                    redirects,
                );
            }
            // A hop to another origin never carries the app's credentials.
            if previous_url
                .as_ref()
                .is_some_and(|previous| !same_origin(previous, &url))
            {
                forwarded.remove(AUTHORIZATION);
                forwarded.remove(COOKIE);
                forwarded.remove(PROXY_AUTHORIZATION);
            }
            let outcome = self
                .hop(
                    settings,
                    &current_method,
                    body.as_ref().unwrap_or(&Bytes::new()),
                    &url,
                    &forwarded,
                    total_deadline,
                    &call,
                )
                .await;
            match outcome {
                HopOutcome::Refused(refusal, target) => {
                    return refuse(refusal, Some(&target), redirects);
                }
                HopOutcome::Redirect {
                    target,
                    location,
                    status,
                } => {
                    if redirects >= MAX_REDIRECTS {
                        return refuse(Refusal::too_many_redirects(), Some(&target), redirects);
                    }
                    let Some(next_url) = resolve_location(&url, &location) else {
                        return refuse(Refusal::upstream_error(), Some(&target), redirects);
                    };
                    last_target = Some(target);
                    let (next_method, keep_body) = redirect_conversion(status, &current_method);
                    current_method = next_method;
                    if !keep_body {
                        body = None;
                    }
                    redirects += 1;
                    call.note_redirects(redirects);
                    url_text = Some(next_url.as_str().to_string());
                    previous_url = Some(url);
                }
                HopOutcome::Respond {
                    target,
                    status,
                    headers,
                    upstream,
                } => {
                    call.note_target(&target);
                    call.note_status(status.as_u16());
                    return forward_response(
                        status,
                        &headers,
                        upstream,
                        settings,
                        total_deadline,
                        call,
                        kill,
                    );
                }
            }
        }
    }

    /// One hop: validate the destination, resolve + classify (every IP),
    /// dial the classified IP with SNI, TLS, send the request, read the
    /// head. The caller has already parsed the URL and checked the scheme
    /// and the credentials.
    #[allow(clippy::too_many_arguments)]
    async fn hop(
        &self,
        settings: &EgressSettings,
        method: &Method,
        body: &Bytes,
        url: &Url,
        forwarded: &HeaderMap,
        total_deadline: Option<Instant>,
        call: &CallGuard,
    ) -> HopOutcome {
        let host_text = match url.host() {
            Some(Host::Domain(domain)) => domain.to_string(),
            Some(Host::Ipv4(ip)) => ip.to_string(),
            Some(Host::Ipv6(ip)) => ip.to_string(),
            None => String::new(),
        };
        let port = url.port_or_known_default().unwrap_or(443);
        let mut target = HopTarget {
            host: host_text.clone(),
            port,
            ip: None,
        };
        call.note_target(&target);

        if !settings.ports.contains(&port) {
            return HopOutcome::Refused(Refusal::port_blocked(), target);
        }
        let ip = match parse_egress_host(&host_text) {
            EgressHost::Invalid => {
                return HopOutcome::Refused(Refusal::bad_target("invalid_host"), target);
            }
            EgressHost::Ip(ip) => {
                target.ip = Some(ip);
                call.note_target(&target);
                if let Err(reason) = self.classify(ip) {
                    return HopOutcome::Refused(Refusal::host_blocked(reason), target);
                }
                ip
            }
            EgressHost::Name(name) => {
                let resolved =
                    match timeout_at_opt(total_deadline, self.config.resolver.resolve(&name, port))
                        .await
                    {
                        Ok(Ok(ips)) if !ips.is_empty() => ips,
                        Ok(_) => return HopOutcome::Refused(Refusal::dns_error(), target),
                        Err(_) => return HopOutcome::Refused(Refusal::timeout(), target),
                    };
                // EVERY resolved IP is classified; one refusal refuses the
                // whole request (an attacker-controlled AAAA could answer a
                // private IP behind a public one).
                for ip in &resolved {
                    if let Err(reason) = self.classify(*ip) {
                        target.ip = Some(*ip);
                        return HopOutcome::Refused(Refusal::host_blocked(reason), target);
                    }
                }
                // The connection goes to the first accepted IP — the same
                // one just classified; the name is never resolved again
                // (rebinding cannot pull the dial to another IP).
                target.ip = Some(resolved[0]);
                call.note_target(&target);
                resolved[0]
            }
        };
        self.dial_and_exchange(
            settings,
            ip,
            &host_text,
            method,
            body,
            url,
            forwarded,
            target,
            total_deadline,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn dial_and_exchange(
        &self,
        settings: &EgressSettings,
        ip: IpAddr,
        host_text: &str,
        method: &Method,
        body: &Bytes,
        url: &Url,
        forwarded: &HeaderMap,
        target: HopTarget,
        total_deadline: Option<Instant>,
    ) -> HopOutcome {
        let Some(server_name) = server_name_for(host_text, ip) else {
            return HopOutcome::Refused(Refusal::bad_target("invalid_host"), target);
        };
        let Some(request) = build_upstream_request(method, url, forwarded, body) else {
            return HopOutcome::Refused(Refusal::bad_target("invalid_egress_url"), target);
        };
        let socket_addr = SocketAddr::new(ip, target.port);
        let hop_start = Instant::now();
        // The head budget runs from the hop start and bounds EVERYTHING
        // before the head (TCP, TLS, the request write and the head read);
        // the connect budget additionally bounds TCP PLUS TLS, and the
        // call's total deadline bounds all of it. One head deadline is
        // computed here and kept for the send and the head read.
        let head_deadline = earliest(
            deadline_after(hop_start, settings.headers_timeout_ms),
            total_deadline,
        );
        let connect_deadline = earliest(
            deadline_after(hop_start, settings.connect_timeout_ms),
            head_deadline,
        );
        let tcp = match timeout_at_opt(connect_deadline, TcpStream::connect(socket_addr)).await {
            Ok(Ok(tcp)) => tcp,
            Ok(Err(_)) => return HopOutcome::Refused(Refusal::upstream_error(), target),
            Err(_) => return HopOutcome::Refused(Refusal::timeout(), target),
        };
        let _ = tcp.set_nodelay(true);
        let connector = TlsConnector::from(self.client_config.clone());
        let tls = match timeout_at_opt(connect_deadline, connector.connect(server_name, tcp)).await
        {
            Ok(Ok(tls)) => tls,
            Ok(Err(_)) => return HopOutcome::Refused(Refusal::upstream_error(), target),
            Err(_) => return HopOutcome::Refused(Refusal::timeout(), target),
        };

        let (mut sender, connection) = match hyper::client::conn::http1::Builder::new()
            .max_buf_size(MAX_HEAD_BYTES)
            .handshake::<_, Full<Bytes>>(TokioIo::new(tls))
            .await
        {
            Ok(pair) => pair,
            Err(_) => return HopOutcome::Refused(Refusal::upstream_error(), target),
        };
        let connection_task = AbortOnDrop(tokio::spawn(async move {
            let _ = connection.await;
        }));

        let response = match timeout_at_opt(head_deadline, sender.send_request(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => return HopOutcome::Refused(Refusal::upstream_error(), target),
            Err(_) => return HopOutcome::Refused(Refusal::timeout(), target),
        };
        let (parts, incoming) = response.into_parts();
        if matches!(parts.status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            if let Some(location) = single_header_value(&parts.headers, &LOCATION) {
                // The 3xx body is discarded: the hop's connection closes
                // (the task is aborted on drop) and the next one starts
                // fresh (revalidated).
                return HopOutcome::Redirect {
                    target,
                    location,
                    status: parts.status.as_u16(),
                };
            }
        }
        // A `Content-Length` over the cap refuses the whole response BEFORE
        // the head goes out.
        if incoming
            .size_hint()
            .exact()
            .is_some_and(|length| length > settings.max_response_bytes)
        {
            return HopOutcome::Refused(Refusal::response_too_large(), target);
        }
        HopOutcome::Respond {
            target,
            status: parts.status,
            headers: parts.headers,
            upstream: UpstreamParts {
                incoming,
                sender,
                connection_task,
            },
        }
    }
}

/// The `hyper` server builder of the worker side (shared by `serve` and the
/// framing tests): the head capped at [`MAX_HEAD_BYTES`].
fn server_builder() -> hyper::server::conn::http1::Builder {
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder.max_buf_size(MAX_HEAD_BYTES);
    builder
}

/// What reports a finished call: the `worker.egress` event (the `tracing`
/// line always, the `OperationalStore` record when wired) and the metrics.
#[derive(Clone)]
struct Reporter {
    metrics: EgressMetrics,
    event_store: Option<OperationalStore>,
}

impl Reporter {
    fn report(&self, ctx: &EgressWorkerContext, started: Instant, record: &CallRecord) {
        self.record_event(ctx, started, record);
        self.metrics
            .record(&ctx.worker, record.outcome.as_str(), record.bytes);
    }

    /// The `worker.egress` event. The fields carry no header (name or
    /// value), no path, no query string and no body.
    fn record_event(&self, ctx: &EgressWorkerContext, started: Instant, record: &CallRecord) {
        let duration_ms = (Instant::now() - started).as_millis() as u64;
        let level = if record.outcome == EgressOutcome::Ok {
            OperationalEventLevel::Info
        } else {
            OperationalEventLevel::Warn
        };
        let outcome = record.outcome.as_str();
        let status = record.status;
        let code = record.code;
        let message = record.message.as_deref();
        let worker = ctx.worker.as_str();
        let version = ctx.version.as_str();
        let process_id = ctx.process_id.as_str();
        match level {
            OperationalEventLevel::Info => {
                info!(
                    target: "edger_orchestrator::egress_guard",
                    worker, version, process_id,
                    status = ?status,
                    outcome,
                    duration_ms,
                    code = ?code,
                    message = ?message,
                    "worker.egress"
                );
            }
            _ => {
                warn!(
                    target: "edger_orchestrator::egress_guard",
                    worker, version, process_id,
                    status = ?status,
                    outcome,
                    duration_ms,
                    code = ?code,
                    message = ?message,
                    "worker.egress"
                );
            }
        }
        let Some(store) = &self.event_store else {
            return;
        };
        store.record(OperationalEventInput {
            source: OperationalEventSource::Runtime,
            kind: "worker.egress".into(),
            level,
            namespace: ctx.namespace.clone(),
            worker: Some(ctx.worker.clone()),
            version: Some(ctx.version.clone()),
            process_id: Some(ctx.process_id.clone()),
            request_id: None,
            trace_id: None,
            outcome: Some(outcome.to_string()),
            status,
            duration_ms: Some(duration_ms),
            timeout_ms: None,
            method: None,
            path: None,
            content_type: None,
            code: code.map(str::to_string),
            message: record.message.clone(),
            truncated: None,
            dropped_count: None,
        });
    }
}

/// The destination of one hop, for the event telemetry.
#[derive(Clone, Debug)]
struct HopTarget {
    host: String,
    port: u16,
    /// The IP the guard dials (the classified one); `None` when no IP was
    /// chosen (DNS failure, a pre-dial refusal, an unparseable host).
    ip: Option<IpAddr>,
}

/// The final state of one egress call, for the event and the metrics.
struct CallRecord {
    /// The status the worker saw (the upstream status when forwarded or
    /// cut; the guard's error status on a refusal).
    status: Option<u16>,
    /// The guard code (refusals only; post-head cuts carry none — the head
    /// already went out).
    code: Option<&'static str>,
    outcome: EgressOutcome,
    /// The stable `key=value` telemetry (destination host/port, connected
    /// IP, bytes, redirects, the cut/refusal reason when there is one).
    /// Never carries a header value, a path or a query string.
    message: Option<String>,
    bytes: u64,
}

fn refusal_record(refusal: &Refusal, target: Option<&HopTarget>, redirects: u32) -> CallRecord {
    let (host, port, ip) = target_fields(target);
    CallRecord {
        status: Some(refusal.status),
        code: Some(refusal.code),
        outcome: refusal.outcome,
        message: build_message(
            host.as_deref(),
            port,
            ip,
            0,
            redirects,
            Some(&refusal.message),
        ),
        bytes: 0,
    }
}

fn forwarded_record(
    status: u16,
    cut: Option<CutKind>,
    bytes: u64,
    redirects: u32,
    target: Option<&HopTarget>,
) -> CallRecord {
    let (host, port, ip) = target_fields(target);
    CallRecord {
        status: Some(status),
        code: None,
        outcome: cut.map_or(EgressOutcome::Ok, cut_outcome),
        message: build_message(
            host.as_deref(),
            port,
            ip,
            bytes,
            redirects,
            cut.map(cut_reason),
        ),
        bytes,
    }
}

/// The result of one hop.
enum HopOutcome {
    Refused(Refusal, HopTarget),
    Redirect {
        target: HopTarget,
        location: String,
        status: u16,
    },
    Respond {
        target: HopTarget,
        status: StatusCode,
        headers: HeaderMap,
        upstream: UpstreamParts,
    },
}

/// The live pieces of an upstream response whose head was read: the body, the
/// request handle (kept alive so the connection is not wound down under the
/// body) and the connection driver (aborted on drop, so a dropped or cut
/// response closes the upstream socket).
struct UpstreamParts {
    incoming: Incoming,
    sender: SendRequest<Full<Bytes>>,
    connection_task: AbortOnDrop,
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A post-head body cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CutKind {
    /// No bytes for `idleTimeout`.
    Idle,
    /// The `totalTimeout` expired (the whole exchange).
    Total,
    /// The body passed `maxResponseBytes` mid-stream.
    SizeLimit,
    /// The upstream read failed (connection dropped / TLS error).
    UpstreamStreamError,
    /// The worker went away (its connection closed or the response was
    /// dropped before the end).
    WorkerDisconnected,
}

fn cut_outcome(kind: CutKind) -> EgressOutcome {
    match kind {
        CutKind::Idle | CutKind::Total => EgressOutcome::Timeout,
        _ => EgressOutcome::Error,
    }
}

fn cut_reason(kind: CutKind) -> &'static str {
    match kind {
        CutKind::Idle => "idle_timeout",
        CutKind::Total => "total_timeout",
        CutKind::SizeLimit => "response_size_limit",
        CutKind::UpstreamStreamError => "upstream_stream_error",
        CutKind::WorkerDisconnected => "worker_disconnected",
    }
}

/// The error the response body ends with on a cut: `hyper` aborts the
/// connection on it, without the terminating chunk.
#[derive(Debug)]
struct CutError(CutKind);

impl fmt::Display for CutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "egress response cut: {}", cut_reason(self.0))
    }
}

impl StdError for CutError {}

/// The state of an admitted call until it is reported: the concurrency slot
/// and what the event needs to describe the call at that moment.
struct CallState {
    _permit: OwnedSemaphorePermit,
    /// The upstream status, once the head arrived.
    status: Option<u16>,
    redirects: u32,
    /// The destination of the hop in progress (the last one noted).
    target: Option<HopTarget>,
}

/// What the call guard, the response body and the total-deadline supervisor
/// share. The state is taken by whoever reports first, so the call is
/// reported exactly once and the slot is released with that report.
struct CallShared {
    reporter: Reporter,
    ctx: Arc<EgressWorkerContext>,
    started: Instant,
    /// Body bytes handed to the worker so far (read by the supervisor).
    bytes: AtomicU64,
    state: Mutex<Option<CallState>>,
}

impl CallShared {
    /// Report the call once with the record `build` makes from the state;
    /// `true` when this caller did the reporting.
    fn finish_with(&self, build: impl FnOnce(&CallState) -> CallRecord) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(state) = state else {
            return false;
        };
        let record = build(&state);
        self.reporter.report(&self.ctx, self.started, &record);
        // `state` (and the slot with it) drops after the report.
        true
    }

    /// Report a forwarded response that ended (`cut` is `None` for a clean
    /// end).
    fn finish_forwarded(&self, cut: Option<CutKind>, bytes: u64) -> bool {
        self.finish_with(|state| {
            forwarded_record(
                state.status.unwrap_or(0),
                cut,
                bytes,
                state.redirects,
                state.target.as_ref(),
            )
        })
    }

    /// Report a call dropped before any result: the worker went away (its
    /// connection closed) during DNS, TCP/TLS, the head or the body read.
    fn finish_cancelled(&self) -> bool {
        let bytes = self.bytes.load(AtomicOrdering::Relaxed);
        self.finish_with(|state| {
            let (host, port, ip) = target_fields(state.target.as_ref());
            CallRecord {
                status: state.status,
                code: None,
                outcome: cut_outcome(CutKind::WorkerDisconnected),
                message: build_message(
                    host.as_deref(),
                    port,
                    ip,
                    bytes,
                    state.redirects,
                    Some(cut_reason(CutKind::WorkerDisconnected)),
                ),
                bytes,
            }
        })
    }
}

/// Owns an admitted call from the slot until its result: created right after
/// the slot is taken, before the first wait (body read, DNS, connect, TLS,
/// head), and moved into the response body once the head is forwarded. If it
/// is dropped without a result — `hyper` drops the service future when the
/// worker closes its socket — the call is reported as a cancellation, so
/// every admitted request produces exactly one event and one metric sample.
struct CallGuard {
    shared: Arc<CallShared>,
}

impl CallGuard {
    fn new(
        reporter: Reporter,
        ctx: Arc<EgressWorkerContext>,
        started: Instant,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            shared: Arc::new(CallShared {
                reporter,
                ctx,
                started,
                bytes: AtomicU64::new(0),
                state: Mutex::new(Some(CallState {
                    _permit: permit,
                    status: None,
                    redirects: 0,
                    target: None,
                })),
            }),
        }
    }

    fn update(&self, change: impl FnOnce(&mut CallState)) {
        if let Some(state) = self
            .shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            change(state);
        }
    }

    fn note_target(&self, target: &HopTarget) {
        self.update(|state| state.target = Some(target.clone()));
    }

    fn note_redirects(&self, redirects: u32) {
        self.update(|state| state.redirects = redirects);
    }

    fn note_status(&self, status: u16) {
        self.update(|state| state.status = Some(status));
    }

    /// Report an already built record (a refusal).
    fn finish_with(&self, record: CallRecord) {
        self.shared.finish_with(|_| record);
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        self.shared.finish_cancelled();
    }
}

/// The destination's response body as the worker's `hyper` serves it:
/// streams frame by frame (natural backpressure from the worker's socket),
/// enforcing the idle and total deadlines and the size cap, and ends with a
/// [`CutError`] on any cut so the worker's HTTP client sees an incomplete
/// body.
struct UpstreamBody {
    inner: Incoming,
    _sender: SendRequest<Full<Bytes>>,
    connection_task: AbortOnDrop,
    /// The idle budget and its timer (armed when the body has to wait for
    /// the destination; re-armed after every data frame).
    idle: Option<(Pin<Box<Sleep>>, Duration)>,
    idle_armed: bool,
    total: Option<Pin<Box<Sleep>>>,
    max_bytes: u64,
    bytes: u64,
    /// The exact body length when the destination declared one
    /// (`Content-Length`): `hyper` stops polling a fixed-length body once the
    /// last byte is written, so the call is reported complete at that frame
    /// (or at the drop, if all expected bytes were handed out).
    expected: Option<u64>,
    /// A cut decided while handing out the last allowed bytes (the size
    /// cap): returned by the next poll.
    pending_cut: Option<CutKind>,
    done: bool,
    /// The call: reported by whoever finishes it first (this body, or the
    /// supervisor), dropped with the body.
    call: CallGuard,
    /// Ends the call at the total deadline even if `hyper` never polls the
    /// body again (aborted with the body).
    _supervisor: Option<AbortOnDrop>,
}

impl UpstreamBody {
    fn new(
        upstream: UpstreamParts,
        settings: &EgressSettings,
        total_deadline: Option<Instant>,
        call: CallGuard,
        kill: Arc<Notify>,
    ) -> Self {
        // The total-deadline supervisor does not depend on the body being
        // polled: under backpressure (the worker stopped reading) `hyper`
        // stops asking for frames, so the poll-time check cannot fire. At the
        // deadline, if the call is still running, the supervisor reports the
        // timeout (releasing the slot) and has the worker connection torn
        // down, which drops this body and the destination connection.
        let supervisor = total_deadline.map(|deadline| {
            let shared = call.shared.clone();
            AbortOnDrop(tokio::spawn(async move {
                sleep_until(deadline).await;
                let bytes = shared.bytes.load(AtomicOrdering::Relaxed);
                if shared.finish_forwarded(Some(CutKind::Total), bytes) {
                    kill.notify_one();
                }
            }))
        });
        let expected = upstream.incoming.size_hint().exact();
        let idle = (settings.idle_timeout_ms > 0).then(|| {
            let duration = Duration::from_millis(settings.idle_timeout_ms);
            (Box::pin(sleep_until(Instant::now() + duration)), duration)
        });
        Self {
            inner: upstream.incoming,
            _sender: upstream.sender,
            connection_task: upstream.connection_task,
            idle,
            idle_armed: false,
            total: total_deadline.map(|deadline| Box::pin(sleep_until(deadline))),
            max_bytes: settings.max_response_bytes,
            bytes: 0,
            expected,
            pending_cut: None,
            done: false,
            call,
            _supervisor: supervisor,
        }
    }

    /// Every byte the destination declared was handed out.
    fn fully_delivered(&self) -> bool {
        self.expected == Some(self.bytes)
    }

    /// End the body with a cut: report it, close the upstream socket.
    fn cut(&mut self, kind: CutKind) -> CutError {
        self.done = true;
        self.connection_task.0.abort();
        self.call.shared.finish_forwarded(Some(kind), self.bytes);
        CutError(kind)
    }
}

impl Body for UpstreamBody {
    type Data = Bytes;
    type Error = CutError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if let Some(kind) = this.pending_cut.take() {
            return Poll::Ready(Some(Err(this.cut(kind))));
        }
        // The total budget is checked on every poll: a fast, endless body
        // that never makes the body wait still ends at the deadline.
        if let Some(total) = this.total.as_mut() {
            if total.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Some(Err(this.cut(CutKind::Total))));
            }
        }
        loop {
            match Pin::new(&mut this.inner).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                    Ok(mut data) => {
                        if data.is_empty() {
                            continue;
                        }
                        let room = this.max_bytes - this.bytes;
                        if data.len() as u64 > room {
                            // Hand out exactly the bytes up to the cap, then
                            // end with an error on the next poll.
                            data.truncate(room as usize);
                            this.pending_cut = Some(CutKind::SizeLimit);
                            if data.is_empty() {
                                let kind = CutKind::SizeLimit;
                                this.pending_cut = None;
                                return Poll::Ready(Some(Err(this.cut(kind))));
                            }
                        }
                        this.bytes += data.len() as u64;
                        this.call
                            .shared
                            .bytes
                            .store(this.bytes, AtomicOrdering::Relaxed);
                        this.idle_armed = false;
                        if this.fully_delivered() {
                            // The last byte of a fixed-length body: the call
                            // is complete (hyper will not poll for the end).
                            this.done = true;
                            this.call.shared.finish_forwarded(None, this.bytes);
                        }
                        return Poll::Ready(Some(Ok(Frame::data(data))));
                    }
                    // Trailers are not forwarded.
                    Err(_) => continue,
                },
                Poll::Ready(Some(Err(_))) => {
                    return Poll::Ready(Some(Err(this.cut(CutKind::UpstreamStreamError))));
                }
                Poll::Ready(None) => {
                    this.done = true;
                    this.call.shared.finish_forwarded(None, this.bytes);
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    if let Some((sleep, duration)) = this.idle.as_mut() {
                        if !this.idle_armed {
                            sleep.as_mut().reset(Instant::now() + *duration);
                            this.idle_armed = true;
                        }
                        if sleep.as_mut().poll(cx).is_ready() {
                            return Poll::Ready(Some(Err(this.cut(CutKind::Idle))));
                        }
                    }
                    return Poll::Pending;
                }
            }
        }
    }

    // `hyper` stops polling a body once `is_end_stream` is true; the end is
    // only known after `poll_frame` answered `None` (so the report and the
    // slot release happen on that poll, before the terminating chunk).
    fn is_end_stream(&self) -> bool {
        self.done
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for UpstreamBody {
    fn drop(&mut self) {
        // Dropped before the end was reported: complete when every declared
        // byte was handed out (an empty `HEAD`/`204` body that `hyper` never
        // polls), otherwise the worker went away or the head could not be
        // written.
        let cut = if self.fully_delivered() {
            None
        } else {
            Some(CutKind::WorkerDisconnected)
        };
        self.call.shared.finish_forwarded(cut, self.bytes);
    }
}

/// The IPv4-mapped v6 canonical form (the rest passes through).
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        other => other,
    }
}

/// The checked request from the worker (the head `hyper` already framed).
#[derive(Debug)]
struct RequestMeta {
    method: Method,
    /// The headers to forward upstream: no hop-by-hop headers, no `host`, no
    /// guard protocol header, no `content-length` (recomputed from the
    /// buffered body).
    headers: HeaderMap,
    /// The `x-edger-egress-url` value (the destination); `None` when the
    /// header is absent (400 at the first hop).
    egress_url: Option<String>,
    /// The declared `Content-Length`, for the 413 before reading the body.
    declared_length: Option<u64>,
}

impl RequestMeta {
    /// The destination host/port of the egress URL when it parses (event
    /// telemetry for refusals before the hop runs); `None` otherwise.
    fn destination_hint(&self) -> (Option<String>, Option<u16>) {
        let Some(url_text) = self.egress_url.as_deref() else {
            return (None, None);
        };
        let Ok(url) = Url::parse(url_text) else {
            return (None, None);
        };
        if url.scheme() != "https" {
            return (None, None);
        }
        let host = match url.host() {
            Some(Host::Domain(domain)) => Some(domain.to_string()),
            Some(Host::Ipv4(ip)) => Some(ip.to_string()),
            Some(Host::Ipv6(ip)) => Some(ip.to_string()),
            None => None,
        };
        (host, url.port_or_known_default())
    }
}

fn hint_target((host, port): (Option<String>, Option<u16>)) -> Option<HopTarget> {
    host.map(|host| HopTarget {
        host,
        port: port.unwrap_or(0),
        ip: None,
    })
}

/// The protocol checks on a request `hyper` framed: HTTP/1.1 only, no
/// `CONNECT`, every `Host` equal to the fake host (and the authority of an
/// absolute-form target too), at most one distinct `x-edger-egress-url`, and
/// no request carrying both `Content-Length` and `Transfer-Encoding` (the
/// guard buffers and re-frames the body, so an ambiguous one is refused
/// instead of guessed). Any violation answers 400 `EGRESS_BAD_TARGET` with a
/// fixed message (the value of no header is echoed).
fn parse_request(parts: &http::request::Parts) -> Result<RequestMeta, Refusal> {
    if parts.version != Version::HTTP_11 {
        return Err(Refusal::bad_target("invalid_request"));
    }
    if parts.method == Method::CONNECT {
        return Err(Refusal::bad_target("invalid_method"));
    }
    let mut host_seen = false;
    for value in parts.headers.get_all(HOST) {
        host_seen = true;
        match value.to_str() {
            Ok(text) if text.eq_ignore_ascii_case(EGRESS_GUARD_HOST) => {}
            _ => return Err(Refusal::bad_target("bad_host_header")),
        }
    }
    if !host_seen {
        return Err(Refusal::bad_target("bad_host_header"));
    }
    if let Some(authority) = parts.uri.authority() {
        if !authority.as_str().eq_ignore_ascii_case(EGRESS_GUARD_HOST) {
            return Err(Refusal::bad_target("bad_host_header"));
        }
    }
    let mut egress_url: Option<String> = None;
    for value in parts.headers.get_all(EGRESS_URL_HEADER) {
        let text = value
            .to_str()
            .map_err(|_| Refusal::bad_target("invalid_egress_url"))?;
        match &egress_url {
            None => egress_url = Some(text.to_string()),
            Some(first) if first == text => {}
            Some(_) => return Err(Refusal::bad_target("ambiguous_egress_url")),
        }
    }
    if parts.headers.contains_key(CONTENT_LENGTH) && parts.headers.contains_key(TRANSFER_ENCODING) {
        return Err(Refusal::bad_target("ambiguous_body_framing"));
    }
    let declared_length = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<u64>().ok());
    let mut headers = strip_hop_by_hop(&parts.headers);
    headers.remove(HOST);
    headers.remove(EGRESS_URL_HEADER);
    headers.remove(CONTENT_LENGTH);
    Ok(RequestMeta {
        method: parts.method.clone(),
        headers,
        egress_url,
        declared_length,
    })
}

/// Build the upstream request: the request target (path plus query, the
/// fragment never sent), the destination authority as `Host`, the forwarded
/// headers and the buffered body. `Connection: close`: one request per
/// upstream connection. `None` when the target or the authority is not
/// representable.
fn build_upstream_request(
    method: &Method,
    url: &Url,
    forwarded: &HeaderMap,
    body: &Bytes,
) -> Option<Request<Full<Bytes>>> {
    let uri: Uri = request_target(url).parse().ok()?;
    let mut request = Request::builder()
        .method(method.clone())
        .uri(uri)
        .version(Version::HTTP_11)
        .body(Full::new(body.clone()))
        .ok()?;
    *request.headers_mut() = forwarded.clone();
    request
        .headers_mut()
        .insert(HOST, HeaderValue::from_str(&request_authority(url)).ok()?);
    request
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    Some(request)
}

/// The request target of the upstream request: the path (at least `/`) plus
/// the query (the fragment, if any, is never sent).
fn request_target(url: &Url) -> String {
    let mut target = url.path().to_string();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    target
}

/// The `Host` header of the upstream request: the destination authority
/// (IPv6 bracketed), with the port only when it is not the 443 default.
fn request_authority(url: &Url) -> String {
    let Some(host) = url.host() else {
        return String::new();
    };
    let host = match host {
        Host::Domain(domain) => domain.to_string(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => format!("[{ip}]"),
    };
    let port = url.port_or_known_default();
    if port == Some(443) {
        host
    } else {
        format!("{host}:{}", port.unwrap_or(443))
    }
}

/// The hop-by-hop headers (RFC 7230 section 6.1): `connection`,
/// `keep-alive`, the `proxy-*` family, `te`, `trailer`,
/// `transfer-encoding` and `upgrade` — matched case-insensitively.
fn is_hop_by_hop(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "connection"
        || lower == "keep-alive"
        || lower == "te"
        || lower == "trailer"
        || lower == "transfer-encoding"
        || lower == "upgrade"
        || lower.starts_with("proxy-")
}

/// A copy of `headers` without the hop-by-hop ones and without the headers
/// the `Connection` header names (RFC 7230 section 6.1: those are
/// hop-by-hop for this message too).
fn strip_hop_by_hop(headers: &HeaderMap) -> HeaderMap {
    let named: Vec<HeaderName> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|text| text.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .collect();
    let mut out = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if is_hop_by_hop(name.as_str()) || named.contains(name) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// A single header value: `None` when absent or ambiguous (more than one
/// occurrence) — a redirect with two `Location` headers is not followed.
fn single_header_value(headers: &HeaderMap, name: &HeaderName) -> Option<String> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?;
    if values.next().is_some() {
        return None;
    }
    Some(String::from_utf8_lossy(first.as_bytes()).into_owned())
}

/// Resolve a `Location` against the current URL (RFC 3986 merge through
/// `url::Url::join`), which handles absolute, protocol-relative and path
/// references. An empty location is not resolvable.
fn resolve_location(base: &Url, location: &str) -> Option<Url> {
    let location = location.trim();
    if location.is_empty() {
        return None;
    }
    base.join(location).ok()
}

/// The method/body of a redirect hop: 303 always turns into a bodyless
/// GET; 301/302 do when the current request is a POST (the body is not
/// re-sent); 307/308 repeat the method and the (buffered) body.
fn redirect_conversion(status: u16, current_method: &Method) -> (Method, bool) {
    match status {
        303 => (Method::GET, false),
        301 | 302 => {
            if *current_method == Method::POST {
                (Method::GET, false)
            } else {
                (current_method.clone(), true)
            }
        }
        _ => (current_method.clone(), true),
    }
}

/// Same scheme, host and effective port.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// A deadline `ms` in the future; `0` disables the budget (project
/// convention: a zero limit turns the timeout off).
fn deadline_after(now: Instant, ms: u64) -> Option<Instant> {
    if ms == 0 {
        None
    } else {
        Some(now + Duration::from_millis(ms))
    }
}

/// The earlier of two optional deadlines (`None` = no deadline).
fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// `tokio::time::timeout_at` when there is a deadline, a plain await when
/// the budget is disabled.
async fn timeout_at_opt<T, F>(
    deadline: Option<Instant>,
    fut: F,
) -> Result<T, tokio::time::error::Elapsed>
where
    F: Future<Output = T>,
{
    match deadline {
        Some(deadline) => timeout_at(deadline, fut).await,
        None => Ok(fut.await),
    }
}

/// The SNI name: the hostname for `Name` destinations, the IP literal for
/// `Ip` destinations (the host text of a parsed URL is the bare IP for IP
/// hosts).
fn server_name_for(host_text: &str, ip: IpAddr) -> Option<ServerName<'static>> {
    if host_text == ip.to_string() {
        return Some(ServerName::from(ip));
    }
    ServerName::try_from(host_text.to_string()).ok()
}

/// The answer to the worker for a destination response: the status, the
/// headers without the hop-by-hop ones and with the destination's
/// `Content-Length` only when its body is exactly sized (otherwise `hyper`
/// frames the body chunked), and the streaming body.
fn forward_response(
    status: StatusCode,
    headers: &HeaderMap,
    upstream: UpstreamParts,
    settings: &EgressSettings,
    total_deadline: Option<Instant>,
    call: CallGuard,
    kill: Arc<Notify>,
) -> Response<EgressBody> {
    let exact = upstream.incoming.size_hint().exact().is_some();
    let mut forwarded = strip_hop_by_hop(headers);
    if !exact {
        forwarded.remove(CONTENT_LENGTH);
    }
    let body = UpstreamBody::new(upstream, settings, total_deadline, call, kill);
    let mut response = Response::new(body.boxed_unsync());
    *response.status_mut() = status;
    *response.headers_mut() = forwarded;
    response
}

/// The guard's error answer: the code header, the JSON body and the fixed
/// (value-free) message.
fn error_body(refusal: &Refusal) -> String {
    format!(
        r#"{{"error": "{}", "message": "{}"}}"#,
        refusal.code, refusal.message
    )
}

fn error_response(refusal: &Refusal) -> Response<EgressBody> {
    let body = Full::new(Bytes::from(error_body(refusal)))
        .map_err(|never: Infallible| match never {})
        .boxed_unsync();
    let mut response = Response::new(body);
    *response.status_mut() =
        StatusCode::from_u16(refusal.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HeaderName::from_static("x-edger-egress-error"),
        HeaderValue::from_static(refusal.code),
    );
    // The request may still have unread body bytes: do not reuse the
    // connection.
    headers.insert(CONNECTION, HeaderValue::from_static("close"));
    response
}

/// The stable `key=value` telemetry of the event's `message` field:
/// `host=… port=… ip=… bytes=… redirects=…` plus the cut/refusal reason
/// when there is one. `None` when nothing is known (the head was not even
/// parseable and there is no reason either). No header value, path or query string can reach it: the
/// inputs are the destination host (a label the app chose for the egress
/// URL — it is the identity of the connection, not a header echo), the
/// counted bytes and the hop count.
fn build_message(
    host: Option<&str>,
    port: Option<u16>,
    ip: Option<IpAddr>,
    bytes: u64,
    redirects: u32,
    reason: Option<&str>,
) -> Option<String> {
    if host.is_none() && port.is_none() && ip.is_none() && redirects == 0 && reason.is_none() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(host) = host {
        parts.push(format!("host={host}"));
    }
    if let Some(port) = port {
        parts.push(format!("port={port}"));
    }
    if let Some(ip) = ip {
        parts.push(format!("ip={ip}"));
    }
    parts.push(format!("bytes={bytes}"));
    parts.push(format!("redirects={redirects}"));
    if let Some(reason) = reason {
        parts.push(format!("reason={reason}"));
    }
    Some(parts.join(" "))
}

fn target_fields(target: Option<&HopTarget>) -> (Option<String>, Option<u16>, Option<IpAddr>) {
    match target {
        Some(target) => (Some(target.host.clone()), Some(target.port), target.ip),
        None => (None, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn parts_of(
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        version: Version,
    ) -> http::request::Parts {
        let mut builder = Request::builder().method(method).uri(uri).version(version);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(()).unwrap().into_parts().0
    }

    fn guard_parts(extra: &[(&str, &str)]) -> http::request::Parts {
        let mut headers = vec![("host", EGRESS_GUARD_HOST)];
        headers.extend_from_slice(extra);
        parts_of("POST", "/", &headers, Version::HTTP_11)
    }

    fn assert_bad_target(parts: &http::request::Parts, message: &str) {
        let refusal = parse_request(parts).unwrap_err();
        assert_eq!(refusal.code, EGRESS_BAD_TARGET);
        assert_eq!(refusal.status, 400);
        assert_eq!(refusal.message, message);
    }

    #[test]
    fn origin_form_request_with_fake_host_parses() {
        let parts = guard_parts(&[
            ("x-edger-egress-url", "https://a.local/v1?q=1"),
            ("authorization", "Bearer sk-teste-segredo-123"),
            ("content-length", "12"),
        ]);
        let meta = parse_request(&parts).expect("parses");
        assert_eq!(meta.method, Method::POST);
        assert_eq!(meta.egress_url.as_deref(), Some("https://a.local/v1?q=1"));
        assert_eq!(meta.declared_length, Some(12));
        // The protocol headers are not forwarded; the app header is.
        assert!(!meta.headers.contains_key(HOST));
        assert!(!meta.headers.contains_key(EGRESS_URL_HEADER));
        assert!(!meta.headers.contains_key(CONTENT_LENGTH));
        assert_eq!(
            meta.headers.get(AUTHORIZATION).unwrap(),
            "Bearer sk-teste-segredo-123"
        );
    }

    #[test]
    fn absolute_form_request_with_fake_host_parses() {
        let parts = parts_of(
            "GET",
            "http://edger-egress.invalid/",
            &[
                ("host", "EDGER-egress.invalid"),
                ("x-edger-egress-url", "https://a.local/"),
            ],
            Version::HTTP_11,
        );
        let meta = parse_request(&parts).expect("parses (the Host check is case-insensitive)");
        assert_eq!(meta.egress_url.as_deref(), Some("https://a.local/"));
        // An absolute-form target whose authority is another host is refused
        // even with a correct Host header.
        let parts = parts_of(
            "GET",
            "http://api.openai.com/",
            &[
                ("host", EGRESS_GUARD_HOST),
                ("x-edger-egress-url", "https://a.local/"),
            ],
            Version::HTTP_11,
        );
        assert_bad_target(&parts, "bad_host_header");
    }

    #[test]
    fn wrong_or_missing_host_is_rejected() {
        let parts = parts_of(
            "GET",
            "/",
            &[
                ("host", "api.openai.com"),
                ("x-edger-egress-url", "https://a.local/"),
            ],
            Version::HTTP_11,
        );
        assert_bad_target(&parts, "bad_host_header");
        let parts = parts_of(
            "GET",
            "/",
            &[("x-edger-egress-url", "https://a.local/")],
            Version::HTTP_11,
        );
        assert_bad_target(&parts, "bad_host_header");
        // A second, different Host is refused too.
        let parts = parts_of(
            "GET",
            "/",
            &[
                ("host", EGRESS_GUARD_HOST),
                ("host", "api.openai.com"),
                ("x-edger-egress-url", "https://a.local/"),
            ],
            Version::HTTP_11,
        );
        assert_bad_target(&parts, "bad_host_header");
    }

    #[test]
    fn http_10_and_connect_are_rejected() {
        let parts = parts_of(
            "GET",
            "/",
            &[
                ("host", EGRESS_GUARD_HOST),
                ("x-edger-egress-url", "https://a.local/"),
            ],
            Version::HTTP_10,
        );
        assert_bad_target(&parts, "invalid_request");
        let parts = parts_of(
            "CONNECT",
            "edger-egress.invalid:443",
            &[
                ("host", EGRESS_GUARD_HOST),
                ("x-edger-egress-url", "https://a.local/"),
            ],
            Version::HTTP_11,
        );
        assert_bad_target(&parts, "invalid_method");
    }

    #[test]
    fn ambiguous_egress_url_and_framing_are_rejected() {
        let parts = guard_parts(&[
            ("x-edger-egress-url", "https://a.local/"),
            ("x-edger-egress-url", "https://b.local/"),
        ]);
        assert_bad_target(&parts, "ambiguous_egress_url");
        // Two identical copies are the same destination.
        let parts = guard_parts(&[
            ("x-edger-egress-url", "https://a.local/"),
            ("x-edger-egress-url", "https://a.local/"),
        ]);
        assert!(parse_request(&parts).is_ok());
        // Content-Length together with Transfer-Encoding.
        let parts = guard_parts(&[
            ("x-edger-egress-url", "https://a.local/"),
            ("content-length", "5"),
            ("transfer-encoding", "chunked"),
        ]);
        assert_bad_target(&parts, "ambiguous_body_framing");
        // A destination header that is not text.
        let mut parts = guard_parts(&[]);
        parts.headers.insert(
            EGRESS_URL_HEADER,
            HeaderValue::from_bytes(b"https://a.local/\xff").unwrap(),
        );
        assert_bad_target(&parts, "invalid_egress_url");
    }

    #[test]
    fn hop_by_hop_and_protocol_headers_are_stripped_from_requests() {
        let parts = guard_parts(&[
            ("x-edger-egress-url", "https://a.local/"),
            ("connection", "keep-alive, x-private"),
            ("x-private", "1"),
            ("keep-alive", "timeout=5"),
            ("proxy-authorization", "Basic abc"),
            ("proxy-connection", "keep-alive"),
            ("te", "trailers"),
            ("trailer", "x-t"),
            ("upgrade", "websocket"),
            ("x-keep", "yes"),
            ("content-type", "application/json"),
            ("set-cookie", "a=1"),
            ("set-cookie", "b=2"),
        ]);
        let meta = parse_request(&parts).expect("parses");
        let names: Vec<&str> = meta.headers.keys().map(HeaderName::as_str).collect();
        for gone in [
            "connection",
            "x-private",
            "keep-alive",
            "proxy-authorization",
            "proxy-connection",
            "te",
            "trailer",
            "upgrade",
            "host",
            "x-edger-egress-url",
        ] {
            assert!(!names.contains(&gone), "{gone} must be stripped: {names:?}");
        }
        assert_eq!(meta.headers.get("x-keep").unwrap(), "yes");
        assert_eq!(
            meta.headers.get("content-type").unwrap(),
            "application/json"
        );
        // Repeated headers keep every value.
        assert_eq!(meta.headers.get_all("set-cookie").iter().count(), 2);
    }

    #[test]
    fn response_headers_strip_hop_by_hop_and_listed_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("text/plain"));
        headers.insert("connection", HeaderValue::from_static("close, x-hop"));
        headers.insert("x-hop", HeaderValue::from_static("1"));
        headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        headers.insert("x-app", HeaderValue::from_static("1"));
        let out = strip_hop_by_hop(&headers);
        let names: Vec<&str> = out.keys().map(HeaderName::as_str).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&"content-type") && names.contains(&"x-app"));
    }

    #[test]
    fn location_resolution_follows_rfc3986() {
        let base = Url::parse("https://a.local/x/y?k=1#frag").unwrap();
        let cases = [
            ("https://b.local/p?q=2#z", "https://b.local/p?q=2#z"),
            ("//b.local/p", "https://b.local/p"),
            ("/p", "https://a.local/p"),
            ("../z", "https://a.local/z"),
            ("?q=9", "https://a.local/x/y?q=9"),
            ("./w", "https://a.local/x/w"),
        ];
        for (location, expected) in cases {
            let next = resolve_location(&base, location).expect("resolves");
            assert_eq!(
                next.as_str(),
                expected,
                "location {location:?} against {base}"
            );
        }
        assert!(resolve_location(&base, "").is_none());
        assert!(resolve_location(&base, "   ").is_none());
    }

    #[test]
    fn redirect_conversion_rules() {
        // 303 always turns into a bodyless GET.
        assert_eq!(
            redirect_conversion(303, &Method::POST),
            (Method::GET, false)
        );
        assert_eq!(redirect_conversion(303, &Method::GET), (Method::GET, false));
        // 301/302 from a POST turn into a bodyless GET; other methods keep
        // the method.
        assert_eq!(
            redirect_conversion(301, &Method::POST),
            (Method::GET, false)
        );
        assert_eq!(
            redirect_conversion(302, &Method::POST),
            (Method::GET, false)
        );
        assert_eq!(redirect_conversion(301, &Method::GET), (Method::GET, true));
        assert_eq!(
            redirect_conversion(302, &Method::DELETE),
            (Method::DELETE, true)
        );
        // 307/308 repeat method and body.
        assert_eq!(
            redirect_conversion(307, &Method::POST),
            (Method::POST, true)
        );
        assert_eq!(
            redirect_conversion(308, &Method::POST),
            (Method::POST, true)
        );
    }

    #[test]
    fn origin_comparison_decides_when_credentials_are_dropped() {
        let a = Url::parse("https://a.local/x").unwrap();
        assert!(same_origin(
            &a,
            &Url::parse("https://a.local:443/y?z=1").unwrap()
        ));
        assert!(!same_origin(&a, &Url::parse("https://b.local/x").unwrap()));
        assert!(!same_origin(
            &a,
            &Url::parse("https://a.local:8443/x").unwrap()
        ));
        assert!(!same_origin(&a, &Url::parse("http://a.local/x").unwrap()));
    }

    #[test]
    fn from_values_parses_block_allow_and_node_ip() {
        let config = EgressGuardConfig::from_values(None, None, None);
        assert_eq!(config.block_cidrs.len(), 2);
        assert!(config
            .block_cidrs
            .iter()
            .any(|cidr| cidr.contains("10.42.0.5".parse::<IpAddr>().unwrap())));
        assert!(config
            .block_cidrs
            .iter()
            .any(|cidr| cidr.contains("10.43.0.1".parse::<IpAddr>().unwrap())));
        assert!(config.allow_cidrs.is_empty());

        let config = EgressGuardConfig::from_values(
            Some("192.168.0.0/16"),
            Some("167.235.206.217"),
            Some("127.0.0.0/8"),
        );
        assert_eq!(
            config.block_cidrs.len(),
            2,
            "the env list replaces the defaults and the node IP is appended"
        );
        assert!(config
            .block_cidrs
            .iter()
            .any(|cidr| cidr.contains("192.168.1.1".parse::<IpAddr>().unwrap())));
        assert!(config
            .block_cidrs
            .iter()
            .any(|cidr| cidr.contains("167.235.206.217".parse::<IpAddr>().unwrap())));
        assert!(!config
            .block_cidrs
            .iter()
            .any(|cidr| cidr.contains("10.42.0.5".parse::<IpAddr>().unwrap())));
        assert_eq!(config.allow_cidrs.len(), 1);
        assert!(config
            .allow_cidrs
            .iter()
            .any(|cidr| cidr.contains("127.0.0.1".parse::<IpAddr>().unwrap())));

        // Invalid entries are skipped, not fatal.
        let config = EgressGuardConfig::from_values(
            Some("192.168.0.0/16, not-a-cidr"),
            Some("not-an-ip"),
            Some(", 10.0.0.0/8, "),
        );
        assert_eq!(config.block_cidrs.len(), 1);
        assert_eq!(config.allow_cidrs.len(), 1);

        // An empty (but set) block list falls back to the k3s defaults.
        let config = EgressGuardConfig::from_values(Some("   "), None, None);
        assert_eq!(config.block_cidrs.len(), 2);
    }

    #[test]
    fn guard_classify_lets_allow_cidrs_override_the_tables() {
        let allow = EgressCidr::parse("127.0.0.0/8").unwrap();
        let guard = EgressGuard::new(EgressGuardConfig {
            block_cidrs: default_block_cidrs(),
            allow_cidrs: vec![allow],
            resolver: Arc::new(SystemEgressResolver),
            tls_roots: webpki_roots(),
        });
        // Loopback is released by the allow CIDR...
        assert!(guard
            .classify("127.0.0.1".parse::<IpAddr>().unwrap())
            .is_ok());
        // ...including the mapped v6 form (through the canonical v4)...
        assert!(guard
            .classify("::ffff:127.0.0.1".parse::<IpAddr>().unwrap())
            .is_ok());
        // ...while a private IP outside the allow CIDR is still refused.
        assert_eq!(
            guard
                .classify("10.0.0.1".parse::<IpAddr>().unwrap())
                .unwrap_err(),
            EgressBlock::Private
        );
        // With no allow CIDR the table refusal stands.
        let strict = EgressGuard::new(EgressGuardConfig::default());
        assert_eq!(
            strict
                .classify("127.0.0.1".parse::<IpAddr>().unwrap())
                .unwrap_err(),
            EgressBlock::Loopback
        );
    }

    #[test]
    fn build_message_covers_the_event_telemetry() {
        let none = build_message(None, None, None, 0, 0, None);
        assert!(none.is_none());
        let full = build_message(
            Some("127.0.0.1"),
            Some(443),
            Some("127.0.0.1".parse::<IpAddr>().unwrap()),
            42,
            1,
            Some("total_timeout"),
        );
        assert_eq!(
            full.as_deref(),
            Some("host=127.0.0.1 port=443 ip=127.0.0.1 bytes=42 redirects=1 reason=total_timeout")
        );
        let host_only = build_message(Some("a.local"), None, None, 0, 0, None);
        assert_eq!(
            host_only.as_deref(),
            Some("host=a.local bytes=0 redirects=0")
        );
    }

    #[test]
    fn error_body_is_json_with_code_and_message() {
        let refusal = Refusal::host_blocked(EgressBlock::Loopback);
        assert_eq!(
            error_body(&refusal),
            r#"{"error": "EGRESS_HOST_BLOCKED", "message": "resolved_ip_blocked:loopback"}"#
        );
        assert_eq!(refusal.status, 403);
        let response = error_response(&refusal);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers().get("x-edger-egress-error").unwrap(),
            "EGRESS_HOST_BLOCKED"
        );
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[test]
    fn request_target_and_authority_are_built_from_the_url() {
        let url = Url::parse("https://a.local/v1/chat?q=1#frag").unwrap();
        assert_eq!(request_target(&url), "/v1/chat?q=1");
        assert_eq!(request_authority(&url), "a.local");
        let url = Url::parse("https://a.local:8443/").unwrap();
        assert_eq!(request_authority(&url), "a.local:8443");
        let url = Url::parse("https://[2a01:4f8::1]:8443/x").unwrap();
        assert_eq!(request_authority(&url), "[2a01:4f8::1]:8443");
    }

    #[test]
    fn upstream_request_carries_the_destination_authority_and_no_guard_headers() {
        let url = Url::parse("https://a.local:8443/v1/chat?q=1#frag").unwrap();
        let mut forwarded = HeaderMap::new();
        forwarded.insert("x-app", HeaderValue::from_static("1"));
        let request =
            build_upstream_request(&Method::POST, &url, &forwarded, &Bytes::from_static(b"hi"))
                .expect("builds");
        assert_eq!(request.uri().to_string(), "/v1/chat?q=1");
        assert_eq!(request.headers().get(HOST).unwrap(), "a.local:8443");
        assert_eq!(request.headers().get(CONNECTION).unwrap(), "close");
        assert_eq!(request.headers().get("x-app").unwrap(), "1");
        assert!(!request.headers().contains_key(EGRESS_URL_HEADER));
    }

    #[test]
    fn destination_hint_extracts_host_and_port() {
        let meta = parse_request(&guard_parts(&[(
            "x-edger-egress-url",
            "https://a.local:8443/x",
        )]))
        .expect("parses");
        assert_eq!(
            meta.destination_hint(),
            (Some("a.local".to_string()), Some(8443))
        );
        // The default port of https is 443.
        let meta = parse_request(&guard_parts(&[("x-edger-egress-url", "https://a.local/x")]))
            .expect("parses");
        assert_eq!(
            meta.destination_hint(),
            (Some("a.local".to_string()), Some(443))
        );
        // A non-https (or invalid) destination yields no hint.
        let meta = parse_request(&guard_parts(&[("x-edger-egress-url", "http://a.local/x")]))
            .expect("parses");
        assert_eq!(meta.destination_hint(), (None, None));
    }

    #[test]
    fn deadlines_follow_the_zero_disables_convention() {
        let now = Instant::now();
        assert!(deadline_after(now, 0).is_none());
        assert!(deadline_after(now, 5000).is_some());
        assert_eq!(earliest(None, None), None);
        let a = deadline_after(now, 1000);
        let b = deadline_after(now, 500);
        assert_eq!(earliest(a, b), b);
    }

    #[test]
    fn single_header_value_refuses_ambiguity() {
        let mut headers = HeaderMap::new();
        assert!(single_header_value(&headers, &LOCATION).is_none());
        headers.append(LOCATION, HeaderValue::from_static("/a"));
        assert_eq!(
            single_header_value(&headers, &LOCATION).as_deref(),
            Some("/a")
        );
        headers.append(LOCATION, HeaderValue::from_static("/b"));
        assert!(single_header_value(&headers, &LOCATION).is_none());
    }

    // -- hyper's refusals of hostile request framing ---------------------------
    //
    // The guard no longer parses HTTP by hand: `hyper` frames the worker side.
    // These tests drive the SAME builder `serve` uses over an in-memory pipe
    // and prove each ambiguous or smuggling-shaped request is answered 400 by
    // `hyper` itself, before the service runs.

    type SeenHeaders = Vec<(String, Vec<u8>)>;

    /// What `raw_status` saw: the status line the server answered with, how
    /// many times the service ran, and the headers the service received.
    struct RawExchange {
        line: String,
        calls: usize,
        headers: SeenHeaders,
        body: Vec<u8>,
    }

    /// Send `request` to a `hyper` server built by `server_builder`.
    async fn raw_status(request: &[u8]) -> RawExchange {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let calls = Arc::new(AtomicUsize::new(0));
        let seen: Arc<Mutex<SeenHeaders>> = Arc::new(Mutex::new(Vec::new()));
        let seen_body: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let counter = calls.clone();
        let recorder = seen.clone();
        let body_recorder = seen_body.clone();
        let service = service_fn(move |request: Request<Incoming>| {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            *recorder.lock().unwrap() = request
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str().to_string(), value.as_bytes().to_vec()))
                .collect();
            let body_recorder = body_recorder.clone();
            async move {
                if let Ok(collected) = request.into_body().collect().await {
                    *body_recorder.lock().unwrap() = collected.to_bytes().to_vec();
                }
                Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
            }
        });
        let server_task = tokio::spawn(async move {
            let _ = server_builder()
                .serve_connection(TokioIo::new(server), service)
                .await;
        });
        client.write_all(request).await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = tokio::time::timeout(Duration::from_secs(2), client.read(&mut chunk))
                .await
                .expect("the server answers or closes");
            match read {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            if buf.windows(2).any(|window| window == b"\r\n") {
                break;
            }
        }
        server_task.abort();
        let line = String::from_utf8_lossy(&buf)
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        let headers = seen.lock().unwrap().clone();
        let body = seen_body.lock().unwrap().clone();
        RawExchange {
            line,
            calls: calls.load(AtomicOrdering::SeqCst),
            headers,
            body,
        }
    }

    #[tokio::test]
    async fn hyper_accepts_a_well_formed_request_control() {
        let exchange = raw_status(
            b"GET / HTTP/1.1\r\nhost: edger-egress.invalid\r\ncontent-length: 0\r\n\r\n",
        )
        .await;
        assert!(
            exchange.line.starts_with("HTTP/1.1 200"),
            "{}",
            exchange.line
        );
        assert_eq!(exchange.calls, 1);
    }

    #[tokio::test]
    async fn hyper_never_lets_a_line_break_into_a_header_value() {
        // A bare LF is a line end for hyper's parser (RFC 7230 allows it): the
        // text after it becomes its OWN header, never part of the previous
        // value, so nothing can ride a value into the upstream request.
        let exchange = raw_status(
            b"GET / HTTP/1.1\r\nhost: edger-egress.invalid\r\nx-a: b\nx-injected: 1\r\n\r\n",
        )
        .await;
        for (name, value) in &exchange.headers {
            assert!(
                !value.contains(&b'\n') && !value.contains(&b'\r'),
                "{name} carries a line break"
            );
        }
        let value = |wanted: &str| {
            exchange
                .headers
                .iter()
                .find(|(name, _)| name == wanted)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(value("x-a").as_deref(), Some(&b"b"[..]));
        assert_eq!(value("x-injected").as_deref(), Some(&b"1"[..]));
        // The upstream request builder refuses a value with a line break even
        // if one ever reached it.
        assert!(HeaderValue::from_bytes(b"b\r\nx-injected: 1").is_err());
        assert!(HeaderValue::from_bytes(b"b\nx-injected: 1").is_err());
        // A bare CR, an obsolete line fold and a NUL are refused outright.
        for request in [
            &b"GET / HTTP/1.1\r\nhost: edger-egress.invalid\r\nx-a: b\rx-injected: 1\r\n\r\n"[..],
            &b"GET / HTTP/1.1\r\nhost: edger-egress.invalid\r\nx-a: b\r\n folded\r\n\r\n"[..],
            &b"GET / HTTP/1.1\r\nhost: edger-egress.invalid\r\nx-a: b\x00c\r\n\r\n"[..],
        ] {
            let exchange = raw_status(request).await;
            assert!(
                exchange.line.starts_with("HTTP/1.1 400"),
                "{:?}: {}",
                String::from_utf8_lossy(request),
                exchange.line
            );
            assert_eq!(exchange.calls, 0);
        }
    }

    #[tokio::test]
    async fn hyper_refuses_an_ambiguous_content_length() {
        for request in [
            &b"POST / HTTP/1.1\r\nhost: edger-egress.invalid\r\ncontent-length: 5\r\ncontent-length: 6\r\n\r\nhello"[..],
            &b"POST / HTTP/1.1\r\nhost: edger-egress.invalid\r\ncontent-length: 5, 6\r\n\r\nhello"[..],
            &b"POST / HTTP/1.1\r\nhost: edger-egress.invalid\r\ncontent-length: +5\r\n\r\nhello"[..],
        ] {
            let exchange = raw_status(request).await;
            assert!(
                exchange.line.starts_with("HTTP/1.1 400"),
                "{:?}: {}",
                String::from_utf8_lossy(request),
                exchange.line
            );
            assert_eq!(exchange.calls, 0);
        }
    }

    #[tokio::test]
    async fn hyper_gives_transfer_encoding_precedence_over_content_length() {
        // hyper does NOT refuse the pair: Transfer-Encoding wins (RFC 7230
        // section 3.3.3 rule 3) and the Content-Length header is dropped
        // before the service sees the request, so the guard never observes
        // the ambiguity. It cannot become a desync toward the destination:
        // the guard decodes the body, buffers it and re-frames it with its
        // own Content-Length (the integration test proves the upstream
        // request). `parse_request`'s own both-headers check stays as defense
        // in depth should a future hyper keep both.
        let exchange = raw_status(
            b"POST / HTTP/1.1\r\nhost: edger-egress.invalid\r\ncontent-length: 3\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        )
        .await;
        assert!(
            exchange.line.starts_with("HTTP/1.1 200"),
            "{}",
            exchange.line
        );
        assert_eq!(exchange.calls, 1);
        let names: Vec<&str> = exchange.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"transfer-encoding"), "{names:?}");
        assert!(!names.contains(&"content-length"), "{names:?}");
        assert_eq!(
            exchange.body, b"hello",
            "the body is decoded by the chunked framing"
        );
        // Both headers visible (should hyper ever keep them): refused.
        let mut parts = guard_parts(&[]);
        parts
            .headers
            .insert(CONTENT_LENGTH, HeaderValue::from_static("5"));
        parts
            .headers
            .insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
        assert_bad_target(&parts, "ambiguous_body_framing");
    }

    #[tokio::test]
    async fn hyper_refuses_http2_prior_knowledge_and_garbage() {
        for request in [
            &b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"[..],
            &b"\x16\x03\x01garbage\r\n\r\n"[..],
        ] {
            let exchange = raw_status(request).await;
            assert!(
                !exchange.line.starts_with("HTTP/1.1 200"),
                "{}",
                exchange.line
            );
            assert_eq!(exchange.calls, 0);
        }
    }
}
