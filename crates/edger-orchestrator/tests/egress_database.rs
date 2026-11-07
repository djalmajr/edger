//! (EDG-52/54 07d2b) The private SQLD HTTP consumer on the shared guard.
//!
//! Real local TCP HTTP/1.1 upstreams (no TLS, no mock transport, no SQLD):
//! a plain `hyper` server on `127.0.0.1` observes exactly what the guest
//! request becomes behind the grant, and the `EgressDatabaseBridge` (the
//! public `WasmHttpTransport`) is the only entry under test. The guard is
//! built with NO `allow_cidrs` and no block overrides: the private/
//! loopback SQLD origin must be dialed through this authority ONLY, with
//! no global bypass. The name-resolving cases inject the fixed IP through
//! the guard's existing `EgressResolver` test API (or use an IP-literal
//! origin).
//!
//! Matrix: positive POST (path/query/body + forged guest credential and
//! `Connection` headers, exact host namespace/auth), rotation A->B
//! without recreating the bridge, a common 4xx; foreign owner/namespace/
//! origin/admin port/userinfo/fragment denied without lookup or dial;
//! missing credential 503 without dial; service 3xx (with/without
//! `Location`) refused without a second hop and the slot recovers;
//! `Content-Length` and chunked over the effective cap refuse (no partial
//! success); cancellation during lookup/head/body reports exactly one
//! event and frees the slot; the (worker, version) slot is shared with
//! another bridge and with the general egress path (same guard).

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{AppSecretResolver, EgressSettings};
use edger_isolation::wasm::{WasmHttpError, WasmHttpRequest, WasmHttpResponse, WasmHttpTransport};
use edger_orchestrator::egress_database::EgressDatabaseBridge;
use edger_orchestrator::observability::{
    OperationalEvent, OperationalEventQuery, OperationalStore,
};
use edger_orchestrator::{
    EgressDatabaseGrant, EgressGuard, EgressGuardConfig, EgressResolver, EgressWorkerContext,
    EGRESS_GUARD_HOST,
};
use http_body::{Body, Frame, SizeHint};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{
    HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE,
    COOKIE, LOCATION, PROXY_AUTHORIZATION,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::RootCertStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::util::SubscriberInitExt;

/// The longest any single wait in these tests may take.
const STEP: Duration = Duration::from_secs(10);

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(STEP, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Capture the tracing lines (the no-leak guarantee for logs).
struct LogSink(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogSink {
    type Writer = std::io::BufWriter<LogSink>;
    fn make_writer(&'a self) -> Self::Writer {
        std::io::BufWriter::new(LogSink(Arc::clone(&self.0)))
    }
}

fn log_sink() -> (LogSink, Arc<Mutex<Vec<u8>>>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    (LogSink(Arc::clone(&buf)), buf)
}

// ---------------------------------------------------------------------------
// The plain TCP HTTP/1.1 upstream (the SQLD service stand-in, no TLS)
// ---------------------------------------------------------------------------

/// One request as the upstream saw it (the observation surface).
#[derive(Debug, Default, Clone)]
struct Seen {
    method: String,
    target: String,
    host: Option<String>,
    authorization: Option<String>,
    namespace_bin: Option<String>,
    cookie: Option<String>,
    proxy_authorization: Option<String>,
    connection: Option<String>,
    egress_url: Option<String>,
    body: Vec<u8>,
}

struct UpstreamState {
    seen: Mutex<Vec<Seen>>,
    second_hop_hits: AtomicUsize,
    closed_conns: AtomicUsize,
    head_signal: Arc<Notify>,
    chunk_signal: Arc<Notify>,
}

impl Default for UpstreamState {
    fn default() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            second_hop_hits: AtomicUsize::new(0),
            closed_conns: AtomicUsize::new(0),
            head_signal: Arc::new(Notify::new()),
            chunk_signal: Arc::new(Notify::new()),
        }
    }
}

type UpBody = UnsyncBoxBody<Bytes, std::convert::Infallible>;

fn full_body(bytes: impl Into<Bytes>) -> UpBody {
    Full::new(bytes.into())
        .map_err(|never: std::convert::Infallible| match never {})
        .boxed_unsync()
}

fn cl_response(status: u16, content_type: &str, body: Vec<u8>) -> Response<UpBody> {
    let mut response = Response::new(full_body(body.clone()));
    *response.status_mut() = StatusCode::from_u16(status).unwrap();
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_str(content_type).unwrap());
    headers.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));
    response
}

/// A chunked body of `n` bytes in 64-byte frames (no `Content-Length`:
/// `hyper` frames it chunked).
struct ChunkBody {
    remaining: Vec<u8>,
}

impl Body for ChunkBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.remaining.is_empty() {
            return std::task::Poll::Ready(None);
        }
        let take = this.remaining.len().min(64);
        let chunk: Vec<u8> = this.remaining.drain(..take).collect();
        std::task::Poll::Ready(Some(Ok(Frame::data(Bytes::from(chunk)))))
    }
    fn is_end_stream(&self) -> bool {
        self.remaining.is_empty()
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

/// One small chunk, then stall forever (the cancellation-during-body case).
struct StallBody {
    first: bool,
    signal: Arc<Notify>,
}

impl Body for StallBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.first {
            this.first = false;
            this.signal.notify_one();
            return std::task::Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"partial-")))));
        }
        std::task::Poll::Pending
    }
    fn is_end_stream(&self) -> bool {
        false
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

fn header_opt<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

async fn upstream_service(
    req: Request<Incoming>,
    state: Arc<UpstreamState>,
    port: u16,
) -> Result<Response<UpBody>, std::convert::Infallible> {
    let method = req.method().clone().to_string();
    let target = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();
    let path = req.uri().path().to_string();
    let host = header_opt(req.headers(), &HeaderName::from_static("host")).map(str::to_owned);
    let authorization = header_opt(req.headers(), &AUTHORIZATION).map(str::to_owned);
    let namespace_bin =
        header_opt(req.headers(), &HeaderName::from_static("x-namespace-bin")).map(str::to_owned);
    let cookie = header_opt(req.headers(), &COOKIE).map(str::to_owned);
    let proxy_authorization = header_opt(req.headers(), &PROXY_AUTHORIZATION).map(str::to_owned);
    let connection = header_opt(req.headers(), &CONNECTION).map(str::to_owned);
    let egress_url = header_opt(
        req.headers(),
        &HeaderName::from_static("x-edger-egress-url"),
    )
    .map(str::to_owned);
    let body = req
        .into_body()
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();
    let seen = Seen {
        method,
        target,
        host,
        authorization,
        namespace_bin,
        cookie,
        proxy_authorization,
        connection,
        egress_url,
        body: body.to_vec(),
    };
    state.seen.lock().unwrap().push(seen);

    match path.as_str() {
        "/ready" => {
            let text = String::from_utf8_lossy(&body);
            Ok(cl_response(
                200,
                "text/plain",
                format!("ok:{text}").into_bytes(),
            ))
        }
        "/error404" => Ok(cl_response(
            404,
            "application/json",
            br#"{"error":"not-found"}"#.to_vec(),
        )),
        "/redir" => {
            let mut response = Response::new(full_body(Vec::new()));
            *response.status_mut() = StatusCode::from_u16(302).unwrap();
            response.headers_mut().insert(
                LOCATION,
                HeaderValue::from_str(&format!("http://127.0.0.1:{port}/elsewhere")).unwrap(),
            );
            Ok(response)
        }
        "/elsewhere" => {
            state.second_hop_hits.fetch_add(1, Ordering::SeqCst);
            Ok(cl_response(200, "text/plain", b"second-hop".to_vec()))
        }
        "/redir304" | "/redir300" => {
            let status = if path == "/redir304" { 304 } else { 300 };
            let mut response = Response::new(full_body(Vec::new()));
            *response.status_mut() = StatusCode::from_u16(status).unwrap();
            Ok(response)
        }
        // `Content-Length` 2048: over the 1024 guest cap, under the guard's
        // 10 MiB — the effective min cap must refuse it before the head.
        "/cl-big" => Ok(cl_response(200, "text/plain", vec![b'x'; 2048])),
        "/chunky" => {
            let mut response = Response::new(
                ChunkBody {
                    remaining: vec![b'y'; 2048],
                }
                .boxed_unsync(),
            );
            *response.status_mut() = StatusCode::from_u16(200).unwrap();
            Ok(response)
        }
        "/slowhead" => {
            state.head_signal.notify_one();
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(cl_response(200, "text/plain", b"late".to_vec()))
        }
        "/slowbody" => {
            let mut response = Response::new(
                StallBody {
                    first: true,
                    signal: Arc::clone(&state.chunk_signal),
                }
                .boxed_unsync(),
            );
            *response.status_mut() = StatusCode::from_u16(200).unwrap();
            Ok(response)
        }
        _ => Ok(cl_response(404, "application/json", b"{}".to_vec())),
    }
}

struct DbUpstream {
    addr: SocketAddr,
    state: Arc<UpstreamState>,
    _task: JoinHandle<()>,
}

fn seen(state: &Arc<UpstreamState>, index: usize) -> Seen {
    state.seen.lock().unwrap()[index].clone()
}

async fn spawn_upstream() -> DbUpstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(UpstreamState::default());
    let retained = Arc::clone(&state);
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let _ = stream.set_nodelay(true);
            let state = Arc::clone(&state);
            let count_state = Arc::clone(&state);
            let port = addr.port();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |req| {
                    let state = Arc::clone(&state);
                    async move { upstream_service(req, state, port).await }
                });
                let _ = http1::Builder::new().serve_connection(io, service).await;
                count_state.closed_conns.fetch_add(1, Ordering::SeqCst);
            });
        }
    });
    DbUpstream {
        addr,
        state: retained,
        _task: task,
    }
}

// ---------------------------------------------------------------------------
// The guard, the fakes and the bridge
// ---------------------------------------------------------------------------

/// The guard of every test: NO `allow_cidrs` and no block overrides — the
/// loopback SQLD origin must not need a bypass on this authority.
fn spawn_db_guard(resolver: Arc<dyn EgressResolver>) -> (Arc<EgressGuard>, OperationalStore) {
    let config = EgressGuardConfig {
        block_cidrs: Vec::new(),
        allow_cidrs: Vec::new(),
        resolver,
        tls_roots: RootCertStore::empty(),
    };
    let store = OperationalStore::default();
    (
        Arc::new(EgressGuard::new(config).with_event_store(store.clone())),
        store,
    )
}

/// The fake DNS (the guard's existing test API): a fixed IP per name, an
/// optional delay (a slow lookup), and the call log.
struct FakeResolver {
    script: std::collections::HashMap<String, Vec<std::net::IpAddr>>,
    calls: Mutex<Vec<String>>,
    delay: Duration,
}

impl FakeResolver {
    fn new(script: &[(&str, &str)], delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            script: script
                .iter()
                .map(|(host, ip)| (host.to_string(), vec![ip.parse().unwrap()]))
                .collect(),
            calls: Mutex::new(Vec::new()),
            delay,
        })
    }
    fn calls_for(&self, host: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == host)
            .count()
    }
}

#[async_trait]
impl EgressResolver for FakeResolver {
    async fn resolve(&self, host: &str, _port: u16) -> io::Result<Vec<std::net::IpAddr>> {
        // The call is registered BEFORE the delay (the lookup started, not
        // settled), so a test can synchronize on the start deterministically.
        self.calls.lock().unwrap().push(host.to_string());
        if self.delay > Duration::ZERO {
            tokio::time::sleep(self.delay).await;
        }
        self.script
            .get(host)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"))
    }
}

/// The fake vault (canary/rotation and the lookup count only): the n-th
/// call takes the n-th value, `None` when exhausted, optional delay.
struct VaultScript {
    values: Mutex<VecDeque<String>>,
    calls: AtomicUsize,
    args: Mutex<Vec<(String, String)>>,
    delay: Duration,
}

impl VaultScript {
    fn new(values: &[&str], delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            values: Mutex::new(values.iter().map(|s| s.to_string()).collect()),
            calls: AtomicUsize::new(0),
            args: Mutex::new(Vec::new()),
            delay,
        })
    }

    /// The number of lookups STARTED (registered before the delay), so a
    /// test can synchronize on the admission instead of a fixed sleep.
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AppSecretResolver for VaultScript {
    async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.args
            .lock()
            .unwrap()
            .push((worker.to_string(), name.to_string()));
        if self.delay > Duration::ZERO {
            tokio::time::sleep(self.delay).await;
        }
        self.values.lock().unwrap().pop_front()
    }
}

fn db_ctx(worker: &str, namespace: Option<&str>) -> EgressWorkerContext {
    EgressWorkerContext {
        worker: worker.to_string(),
        version: "1.0.0".to_string(),
        namespace: namespace.map(str::to_string),
        process_id: String::new(),
    }
}

fn db_settings(
    ports: &[u16],
    connect: u64,
    headers: u64,
    idle: u64,
    total: u64,
    max_bytes: u64,
    max_concurrent: u32,
) -> EgressSettings {
    EgressSettings {
        ports: ports.to_vec(),
        connect_timeout_ms: connect,
        headers_timeout_ms: headers,
        idle_timeout_ms: idle,
        total_timeout_ms: total,
        max_response_bytes: max_bytes,
        max_concurrent,
    }
}

fn default_db_settings() -> EgressSettings {
    db_settings(&[443], 5_000, 10_000, 300_000, 300_000, 10 * 1024 * 1024, 8)
}

/// The host-trusted binding under test (the public `WasmHttpTransport`).
fn db_bridge(
    guard: &Arc<EgressGuard>,
    ctx: &EgressWorkerContext,
    settings: EgressSettings,
    grant: Arc<EgressDatabaseGrant>,
) -> EgressDatabaseBridge {
    EgressDatabaseBridge::new(
        Arc::clone(guard),
        Arc::new(ctx.clone()),
        Arc::new(settings),
        grant,
    )
}

fn db_request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    request_id: &str,
) -> WasmHttpRequest {
    WasmHttpRequest {
        method: method.to_string(),
        url: url.to_string(),
        headers: headers
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect(),
        body: Bytes::copy_from_slice(body),
        request_id: request_id.to_string(),
    }
}

const DB_BODY_LIMIT: usize = 1024 * 1024;

async fn db_fetch(
    bridge: &EgressDatabaseBridge,
    request: WasmHttpRequest,
) -> Result<WasmHttpResponse, WasmHttpError> {
    within(
        "the database bridge fetch",
        bridge.fetch(request, DB_BODY_LIMIT),
    )
    .await
}

/// The guest per-call cap of the capped cases: 1024 (under the guard's
/// 10 MiB response max).
async fn capped_fetch(
    bridge: &EgressDatabaseBridge,
    request: WasmHttpRequest,
) -> Result<WasmHttpResponse, WasmHttpError> {
    within("the capped fetch", bridge.fetch(request, 1024)).await
}

fn db_refusal(result: Result<WasmHttpResponse, WasmHttpError>, what: &str) -> WasmHttpError {
    match result {
        Ok(_) => panic!("{what} must be a refusal, not a normal response"),
        Err(error) => error,
    }
}

// ---------------------------------------------------------------------------
// The events
// ---------------------------------------------------------------------------

fn egress_events(store: &OperationalStore) -> Vec<OperationalEvent> {
    let query = OperationalEventQuery {
        kind: Some("worker.egress".into()),
        limit: Some(500),
        ..Default::default()
    };
    store.query(query).events
}

fn events_for(store: &OperationalStore, request_id: &str) -> Vec<OperationalEvent> {
    egress_events(store)
        .into_iter()
        .filter(|e| e.request_id.as_deref() == Some(request_id))
        .collect()
}

/// Wait until `request_id` has EXACTLY `count` events (the one-event
/// guarantee), returning them.
async fn wait_events(
    store: &OperationalStore,
    request_id: &str,
    count: usize,
) -> Vec<OperationalEvent> {
    for _ in 0..5_000 {
        let events = events_for(store, request_id);
        if events.len() == count {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!(
        "request {request_id} never settled at {count} events ({} so far)",
        events_for(store, request_id).len()
    );
}

fn assert_event(
    event: &OperationalEvent,
    outcome: &str,
    status: Option<u16>,
    code: Option<&str>,
    request_id: &str,
) {
    assert_eq!(event.request_id.as_deref(), Some(request_id));
    assert_eq!(event.outcome.as_deref(), Some(outcome), "outcome mismatch");
    assert_eq!(event.status, status, "status mismatch");
    assert_eq!(event.code.as_deref(), code, "code mismatch");
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// 1. The positive path: POST with path/query/body, forged guest
/// credential and `Connection` headers; the host's sensitive
/// `Authorization` and `x-namespace-bin` win, the legit path/query are
/// preserved and the destination `Host` is the grant's origin.
#[tokio::test]
async fn positive_post_preserves_path_query_body_and_injects_host_headers() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let origin = format!("http://127.0.0.1:{port}/");
    let resolver = FakeResolver::new(&[], Duration::ZERO);
    let (guard, store) = spawn_db_guard(resolver);
    let ctx = db_ctx("sqld-app", Some("team"));
    let vault = VaultScript::new(&["alpha", "beta"], Duration::ZERO);
    let grant = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault)).unwrap(),
    );
    let bridge = db_bridge(&guard, &ctx, default_db_settings(), grant);

    let response = db_fetch(
        &bridge,
        db_request(
            "POST",
            &format!("http://127.0.0.1:{port}/ready?q=1&ns=a"),
            &[
                ("authorization", "Bearer forged-guest"),
                ("cookie", "session=forged"),
                ("proxy-authorization", "Basic forged"),
                ("x-namespace-bin", "Zm9yZ2Vk"),
                // the guest tries to strip the host headers through
                // `Connection`: they must survive with the host values
                (
                    "connection",
                    "x-namespace-bin, authorization, x-edger-egress-url",
                ),
                ("x-edger-egress-url", "http://evil.example/"),
                ("host", "forged.example"),
            ],
            b"payload-bytes",
            "db-req-1",
        ),
    )
    .await
    .expect("the positive POST answers");
    assert_eq!(response.status, 200);
    assert_eq!(response.body.as_ref(), b"ok:payload-bytes");

    // The upstream saw exactly the host-sanitized request.
    let seen = seen(&upstream.state, 0);
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.target, "/ready?q=1&ns=a", "the legit path/query");
    assert_eq!(
        seen.host.as_deref(),
        Some(format!("127.0.0.1:{port}").as_str()),
        "the grant origin authority, not the forged Host"
    );
    assert_eq!(
        seen.authorization.as_deref(),
        Some("Bearer alpha"),
        "the grant's sensitive credential, not the guest's"
    );
    assert_eq!(
        seen.namespace_bin.as_deref(),
        Some("c3FsbnM"),
        "the grant's x-namespace-bin (base64 STANDARD_NO_PAD of sqlns, the pinned SQLD codec)"
    );
    assert!(seen.cookie.is_none(), "the guest Cookie was removed");
    assert!(seen.proxy_authorization.is_none());
    assert!(
        seen.egress_url.is_none(),
        "no guard protocol header reached the service"
    );
    assert_eq!(
        seen.connection.as_deref(),
        Some("close"),
        "the host Connection won (the guest one could not strip the host headers)"
    );
    assert_eq!(seen.body, b"payload-bytes");

    // No DNS (the IP-literal origin dials directly) and exactly one ok
    // event; the credential never reaches the event or the logs.
    assert_eq!(events_for(&store, "db-req-1").len(), 1);
    let event = &events_for(&store, "db-req-1")[0];
    assert_event(event, "ok", Some(200), None, "db-req-1");
    let message = event.message.clone().unwrap_or_default();
    assert!(message.contains("ip=127.0.0.1"));
    assert!(
        !message.contains("alpha"),
        "the token leaked into the event"
    );

    let (sink, buf) = log_sink();
    let _guard2 = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(sink)
        .with_ansi(false)
        .set_default();
    let response = db_fetch(
        &bridge,
        db_request("GET", &format!("{origin}ready"), &[], &[], "db-req-2"),
    )
    .await
    .expect("a second call answers");
    assert_eq!(response.status, 200);
    wait_events(&store, "db-req-2", 1).await;
    let logs = String::from_utf8_lossy(&buf.lock().unwrap()).to_string();
    assert!(!logs.contains("alpha"), "the token leaked into the logs");
}

/// 2. Rotation A->B without recreating the bridge: every call resolves
/// fresh, the owner-scoped args only, and the values never leak.
#[tokio::test]
async fn rotation_without_recreating_the_bridge_updates_the_authorization() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let origin = format!("http://127.0.0.1:{port}/");
    let (guard, store) = spawn_db_guard(FakeResolver::new(&[], Duration::ZERO));
    let ctx = db_ctx("sqld-app", Some("team"));
    let vault = VaultScript::new(&["alpha", "beta"], Duration::ZERO);
    let grant = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault.clone()))
            .unwrap(),
    );
    let bridge = db_bridge(&guard, &ctx, default_db_settings(), grant);

    for (request_id, expected) in [("db-rot-1", "Bearer alpha"), ("db-rot-2", "Bearer beta")] {
        let response = db_fetch(
            &bridge,
            db_request("GET", &format!("{origin}ready"), &[], &[], request_id),
        )
        .await
        .expect("the rotated call answers");
        assert_eq!(response.status, 200);
        let index = vault.calls.load(Ordering::SeqCst) - 1;
        assert_eq!(
            seen(&upstream.state, index).authorization.as_deref(),
            Some(expected),
            "the fresh credential on the {request_id} call"
        );
        let event = &wait_events(&store, request_id, 1).await[0];
        assert_event(event, "ok", Some(200), None, request_id);
    }
    // The vault saw only the owner worker and the credential name.
    assert_eq!(
        *vault.args.lock().unwrap(),
        vec![
            ("sqld-app".to_string(), "SQLD_TOKEN".to_string()),
            ("sqld-app".to_string(), "SQLD_TOKEN".to_string())
        ]
    );
    // The rotated values never reach the events.
    for event in egress_events(&store) {
        let message = event.message.clone().unwrap_or_default();
        assert!(!message.contains("alpha") && !message.contains("beta"));
    }
}

/// 3. A common 4xx is a normal response (any upstream HTTP status).
#[tokio::test]
async fn common_4xx_is_a_normal_response() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let origin = format!("http://127.0.0.1:{port}/");
    let (guard, store) = spawn_db_guard(FakeResolver::new(&[], Duration::ZERO));
    let ctx = db_ctx("sqld-app", Some("team"));
    let vault = VaultScript::new(&["alpha"], Duration::ZERO);
    let grant = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault)).unwrap(),
    );
    let bridge = db_bridge(&guard, &ctx, default_db_settings(), grant);

    let response = db_fetch(
        &bridge,
        db_request("GET", &format!("{origin}error404"), &[], &[], "db-404"),
    )
    .await
    .expect("the 404 is a normal response, not a refusal");
    assert_eq!(response.status, 404);
    assert_eq!(response.body.as_ref(), br#"{"error":"not-found"}"#);
    let event = &wait_events(&store, "db-404", 1).await[0];
    assert_event(event, "ok", Some(404), None, "db-404");
}

/// 4. Foreign owner/namespace, another origin, the admin port, userinfo
/// and a fragment: static 403 `EGRESS_DATABASE_DENIED`, no credential
/// lookup and no dial — each attempt exactly one event.
#[tokio::test]
async fn foreign_owner_origin_port_userinfo_fragment_denied_without_lookup_or_dial() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let origin = format!("http://127.0.0.1:{port}/");
    let resolver = FakeResolver::new(&[("other-host.example", "93.184.216.34")], Duration::ZERO);
    let (guard, store) = spawn_db_guard(resolver.clone());
    let ctx = db_ctx("sqld-app", Some("team"));
    let vault = VaultScript::new(&["alpha"], Duration::ZERO);
    let grant = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault.clone()))
            .unwrap(),
    );
    let bridge = db_bridge(&guard, &ctx, default_db_settings(), grant.clone());

    // (a) and (b): a foreign owner (worker or namespace) on the SAME URL.
    for (request_id, other) in [
        ("db-den-1", db_ctx("other-app", Some("team"))),
        ("db-den-2", db_ctx("sqld-app", Some("other"))),
    ] {
        let other_bridge = db_bridge(&guard, &other, default_db_settings(), grant.clone());
        let error = db_refusal(
            db_fetch(
                &other_bridge,
                db_request("GET", &format!("{origin}ready"), &[], &[], request_id),
            )
            .await,
            request_id,
        );
        assert_eq!(error, WasmHttpError::Transport);
        let event = &wait_events(&store, request_id, 1).await[0];
        assert_event(
            event,
            "blocked",
            Some(403),
            Some("EGRESS_DATABASE_DENIED"),
            request_id,
        );
        assert!(event
            .message
            .clone()
            .unwrap_or_default()
            .contains("reason=database_denied"));
    }

    // (c)-(f): hostile URL shapes on the owner's bridge: another origin,
    // the admin port 9090, userinfo and a fragment.
    for (request_id, url) in [
        ("db-den-3", "http://other-host.example/ready"),
        ("db-den-4", "http://127.0.0.1:9090/ready"),
        (
            "db-den-5",
            &format!("http://user:pw@127.0.0.1:{port}/ready"),
        ),
        ("db-den-6", &format!("http://127.0.0.1:{port}/ready#frag")),
    ] {
        let error = db_refusal(
            db_fetch(&bridge, db_request("GET", url, &[], &[], request_id)).await,
            request_id,
        );
        assert_eq!(error, WasmHttpError::Transport);
        let event = &wait_events(&store, request_id, 1).await[0];
        assert_event(
            event,
            "blocked",
            Some(403),
            Some("EGRESS_DATABASE_DENIED"),
            request_id,
        );
    }

    // No credential lookup and no dial for any of the six denials.
    assert_eq!(vault.calls.load(Ordering::SeqCst), 0, "no vault lookup");
    assert_eq!(resolver.calls_for("other-host.example"), 0, "no DNS");
    assert!(
        upstream.state.seen.lock().unwrap().is_empty(),
        "no request reached the service"
    );
    assert_eq!(
        upstream.state.closed_conns.load(Ordering::SeqCst),
        0,
        "no dial"
    );
}

/// 5. A missing credential answers 503 `EGRESS_DATABASE_AUTH_UNAVAILABLE`
/// without dialing, and the slot is released (with `maxConcurrent = 1` the
/// next call is admitted again, not a 429).
#[tokio::test]
async fn missing_credential_answers_auth_unavailable_without_dial_and_frees_the_slot() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let origin = format!("http://127.0.0.1:{port}/");
    let (guard, store) = spawn_db_guard(FakeResolver::new(&[], Duration::ZERO));
    let ctx = db_ctx("sqld-app", Some("team"));
    // The resolver exists but has no record: the lookup runs and fails.
    let vault = VaultScript::new(&[], Duration::ZERO);
    let grant = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault.clone()))
            .unwrap(),
    );
    let bridge = db_bridge(
        &guard,
        &ctx,
        db_settings(&[443], 5_000, 10_000, 300_000, 300_000, 10 * 1024 * 1024, 1),
        grant,
    );

    for request_id in ["db-auth-1", "db-auth-2"] {
        let error = db_refusal(
            db_fetch(
                &bridge,
                db_request("GET", &format!("{origin}ready"), &[], &[], request_id),
            )
            .await,
            request_id,
        );
        assert_eq!(error, WasmHttpError::Transport);
        let event = &wait_events(&store, request_id, 1).await[0];
        assert_event(
            event,
            "error",
            Some(503),
            Some("EGRESS_DATABASE_AUTH_UNAVAILABLE"),
            request_id,
        );
        assert!(event
            .message
            .clone()
            .unwrap_or_default()
            .contains("reason=database_auth_unavailable"));
    }
    // The lookup ran once per call; no dial; the second call was admitted
    // (the slot was released: `maxConcurrent = 1` and no `EGRESS_LIMIT`).
    assert_eq!(vault.calls.load(Ordering::SeqCst), 2);
    assert!(upstream.state.seen.lock().unwrap().is_empty());
    assert_eq!(upstream.state.closed_conns.load(Ordering::SeqCst), 0);
}

/// 6. Any 3xx (with a `Location`, a bare 304 and a bare 300) refuses
/// `EGRESS_DATABASE_DENIED`, never makes a second hop, closes the
/// upstream and leaves the slot for the next request.
#[tokio::test]
async fn service_redirects_are_refused_without_a_second_hop() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let origin = format!("http://127.0.0.1:{port}/");
    let (guard, store) = spawn_db_guard(FakeResolver::new(&[], Duration::ZERO));
    let ctx = db_ctx("sqld-app", Some("team"));
    // One fresh value per fetch: every admitted call resolves the vault
    // before any refusal (the redirect refusals happen after the head).
    let vault = VaultScript::new(&["alpha", "beta", "gamma", "delta"], Duration::ZERO);
    let grant = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault)).unwrap(),
    );
    let bridge = db_bridge(&guard, &ctx, default_db_settings(), grant);

    for (request_id, path) in [
        // Relative to the origin (which already ends in a `/`): no leading
        // slash (a double slash would miss the upstream's routes and fall
        // to its 404 catch-all).
        ("db-red-1", "redir"),
        ("db-red-2", "redir304"),
        ("db-red-3", "redir300"),
    ] {
        let error = db_refusal(
            db_fetch(
                &bridge,
                db_request("GET", &format!("{origin}{path}"), &[], &[], request_id),
            )
            .await,
            request_id,
        );
        assert_eq!(error, WasmHttpError::Transport);
        let event = &wait_events(&store, request_id, 1).await[0];
        assert_event(
            event,
            "blocked",
            Some(403),
            Some("EGRESS_DATABASE_DENIED"),
            request_id,
        );
    }
    // No second hop (the `/elsewhere` target was never reached), the
    // upstream connections were closed by the refusals, and the slot is
    // free: the next request succeeds.
    assert_eq!(
        upstream.state.second_hop_hits.load(Ordering::SeqCst),
        0,
        "no second hop"
    );
    assert!(
        upstream.state.closed_conns.load(Ordering::SeqCst) >= 3,
        "each refused 3xx closed its upstream connection"
    );
    let response = db_fetch(
        &bridge,
        db_request("GET", &format!("{origin}ready"), &[], &[], "db-red-4"),
    )
    .await
    .expect("the slot recovered after the redirect refusals");
    assert_eq!(response.status, 200);
}

/// 7. The effective `min(guest, responseMax)` cap: a `Content-Length` over
/// the guest cap refuses before the head, and a chunked body over it cuts
/// with an error — never a partial body as success.
#[tokio::test]
async fn content_length_and_chunked_over_the_effective_cap_are_not_partial_success() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let origin = format!("http://127.0.0.1:{port}/");
    let (guard, store) = spawn_db_guard(FakeResolver::new(&[], Duration::ZERO));
    let ctx = db_ctx("sqld-app", Some("team"));
    // One fresh value per fetch: the cap refusals (a) and (b) happen
    // AFTER the credential lookup, and the control (c) needs its own.
    let vault = VaultScript::new(&["alpha", "beta", "gamma"], Duration::ZERO);
    let grant = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault)).unwrap(),
    );
    let bridge = db_bridge(&guard, &ctx, default_db_settings(), grant);

    // (a) A declared `Content-Length` of 2048 refuses BEFORE the head
    // under the effective min cap (the pre-head check sees 1024, not the
    // 10 MiB).
    let error = db_refusal(
        within(
            "the cl-big fetch",
            capped_fetch(
                &bridge,
                db_request("GET", &format!("{origin}cl-big"), &[], &[], "db-cap-1"),
            ),
        )
        .await,
        "cl-big",
    );
    assert_eq!(error, WasmHttpError::ResponseTooLarge);
    let event = &wait_events(&store, "db-cap-1", 1).await[0];
    assert_event(
        event,
        "error",
        Some(502),
        Some("EGRESS_RESPONSE_TOO_LARGE"),
        "db-cap-1",
    );

    // (b) A chunked body of 2048 cuts with an error mid-stream: no
    // partial body is a success and the upstream goes with the cut.
    let error = db_refusal(
        within(
            "the chunky fetch",
            capped_fetch(
                &bridge,
                db_request("GET", &format!("{origin}chunky"), &[], &[], "db-cap-2"),
            ),
        )
        .await,
        "chunky",
    );
    assert_eq!(error, WasmHttpError::ResponseTooLarge);
    let event = &wait_events(&store, "db-cap-2", 1).await[0];
    // (the cut event carries the noted upstream status, like the WASM
    // entry's cut events — the shared tail reports the head's status)
    assert_event(event, "error", Some(200), None, "db-cap-2");
    assert!(event
        .message
        .clone()
        .unwrap_or_default()
        .contains("reason=response_size_limit"));

    // (c) Control: a body under the cap is a full success.
    let response = within(
        "the small fetch",
        capped_fetch(
            &bridge,
            db_request("GET", &format!("{origin}ready"), &[], &[], "db-cap-3"),
        ),
    )
    .await
    .expect("a body under the cap answers");
    assert_eq!(response.status, 200);
    assert_eq!(response.body.len(), 3); // "ok:" — under 1024
    let event = &wait_events(&store, "db-cap-3", 1).await[0];
    assert_event(event, "ok", Some(200), None, "db-cap-3");
}

/// 8. Cancellation during the vault lookup, the head and the body: the
/// slot is held until the drop, exactly one event per dropped attempt
/// (with its request id), and the next request passes.
#[tokio::test]
async fn cancellation_during_lookup_head_and_body_reports_once_and_frees_the_slot() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let origin = format!("http://127.0.0.1:{port}/");
    let (guard, store) = spawn_db_guard(FakeResolver::new(&[], Duration::ZERO));
    let ctx = db_ctx("sqld-app", Some("team"));
    let settings = db_settings(&[443], 5_000, 10_000, 300_000, 3_000, 10 * 1024 * 1024, 1);

    // (a) During the vault lookup: the slow lookup holds the slot; the
    // drop releases it and reports once (`worker_disconnected`). One
    // fresh value per fetch: (b), (c) and (d) each resolve their own.
    let vault = VaultScript::new(
        &["alpha", "beta", "gamma", "delta"],
        Duration::from_millis(800),
    );
    let grant = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault.clone()))
            .unwrap(),
    );
    let bridge = db_bridge(&guard, &ctx, settings.clone(), grant.clone());
    let in_flight = origin.clone();
    let handle = tokio::spawn(async move {
        within(
            "the in-flight lookup",
            bridge.fetch(
                db_request("GET", &in_flight, &[], &[], "db-cxl-1"),
                DB_BODY_LIMIT,
            ),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await; // inside the vault await
    handle.abort();
    let events = wait_events(&store, "db-cxl-1", 1).await;
    assert_event(&events[0], "error", None, None, "db-cxl-1");
    assert!(events[0]
        .message
        .clone()
        .unwrap_or_default()
        .contains("reason=worker_disconnected"));
    assert_eq!(vault.calls.load(Ordering::SeqCst), 1);

    // (b) During the head: the service received the request and stalls.
    let bridge2 = db_bridge(&guard, &ctx, settings.clone(), grant.clone());
    let head_signal = Arc::clone(&upstream.state.head_signal);
    let in_flight = format!("{origin}slowhead");
    let handle = tokio::spawn(async move {
        within(
            "the in-flight head",
            bridge2.fetch(
                db_request("GET", &in_flight, &[], &[], "db-cxl-2"),
                DB_BODY_LIMIT,
            ),
        )
        .await
    });
    within("the head signal", head_signal.notified()).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.abort();
    let events = wait_events(&store, "db-cxl-2", 1).await;
    assert_eq!(events[0].outcome.as_deref(), Some("error"));

    // (c) During the body: one chunk was delivered, then the drop.
    let bridge3 = db_bridge(&guard, &ctx, settings.clone(), grant.clone());
    let chunk_signal = Arc::clone(&upstream.state.chunk_signal);
    let in_flight = format!("{origin}slowbody");
    let handle = tokio::spawn(async move {
        within(
            "the in-flight body",
            bridge3.fetch(
                db_request("GET", &in_flight, &[], &[], "db-cxl-3"),
                DB_BODY_LIMIT,
            ),
        )
        .await
    });
    within("the chunk signal", chunk_signal.notified()).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    handle.abort();
    let events = wait_events(&store, "db-cxl-3", 1).await;
    assert_eq!(events[0].outcome.as_deref(), Some("error"));

    // The slot survived all three drops: the next request passes.
    let bridge4 = db_bridge(&guard, &ctx, settings, grant);
    let response = db_fetch(
        &bridge4,
        db_request("GET", &format!("{origin}ready"), &[], &[], "db-cxl-4"),
    )
    .await
    .expect("the next request passes after the cancellations");
    assert_eq!(response.status, 200);
}

/// 9. The (worker, version) slot is the SAME map as the general egress:
/// another database bridge and the socket path of the same guard see the
/// 429 while a slot is held — and the general path still refuses the very
/// same loopback IP that this authority dials (no CIDR bypass).
#[tokio::test]
async fn the_same_worker_version_slot_is_shared_with_other_bridges_and_general_egress() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let origin = format!("http://127.0.0.1:{port}/");
    let resolver = FakeResolver::new(
        // Loopback on purpose: the delay is above the total budget, so the
        // lookup never settles inside it and nothing is ever dialed — the
        // IP is inert, and it cannot become a dial target.
        &[("slowdns.example.", "127.0.0.1")],
        Duration::from_secs(10),
    );
    let (guard, store) = spawn_db_guard(resolver.clone());
    let ctx = db_ctx("sqld-app", Some("team"));
    let settings = db_settings(
        &[443, port],
        5_000,
        10_000,
        300_000,
        3_000,
        10 * 1024 * 1024,
        1,
    );

    // First, the contrast: the GENERAL path refuses the very loopback IP
    // this authority dials (classifier: loopback; no `allow_cidrs`).
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("general.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let serve_guard = Arc::clone(&guard);
    let serve_task = tokio::spawn({
        let ctx = ctx.clone();
        let settings = settings.clone();
        async move { serve_guard.serve(listener, ctx, settings).await }
    });
    let response = raw_general_call(&sock, &format!("https://127.0.0.1:{port}/x")).await;
    assert_eq!(response, 403, "the general path blocks the loopback IP");
    let blocked = within(
        "the host_blocked event",
        wait_for_event(&store, |e| e.code.as_deref() == Some("EGRESS_HOST_BLOCKED")),
    )
    .await;
    assert_eq!(blocked.outcome.as_deref(), Some("blocked"));

    // (a) Another bridge, the same (worker, version) key: while bridge A
    // holds the slot in a slow vault lookup, bridge B is a 429 `limit`
    // (no new internet grant, no second profile).
    let vault = VaultScript::new(&["alpha"], Duration::from_millis(800));
    let grant = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault.clone()))
            .unwrap(),
    );
    let bridge_a = db_bridge(&guard, &ctx, settings.clone(), grant.clone());
    let bridge_b = db_bridge(&guard, &ctx, settings.clone(), grant.clone());
    let in_flight = origin.clone();
    let handle = tokio::spawn(async move {
        within(
            "the in-flight bridge A",
            bridge_a.fetch(
                db_request("GET", &in_flight, &[], &[], "db-share-1"),
                DB_BODY_LIMIT,
            ),
        )
        .await
    });
    // Synchronized on the vault lookup itself (not a fixed sleep): the
    // call is registered before the delay, so `calls >= 1` means bridge A
    // already holds the slot for the whole vault delay.
    within("the in-flight bridge A vault lookup", async {
        while vault.calls() < 1 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    let error = db_refusal(
        db_fetch(
            &bridge_b,
            db_request("GET", &format!("{origin}ready"), &[], &[], "db-share-2"),
        )
        .await,
        "bridge B",
    );
    assert_eq!(error, WasmHttpError::Transport);
    let event = &wait_events(&store, "db-share-2", 1).await[0];
    assert_event(
        event,
        "limit",
        Some(429),
        Some("EGRESS_LIMIT"),
        "db-share-2",
    );
    handle.abort();
    wait_events(&store, "db-share-1", 1).await;

    // (b) The general socket path of the SAME guard and key: a slow DNS
    // holds the slot; the database bridge answers the 429 `limit` without
    // any new grant.
    let vault_fast = VaultScript::new(&["beta"], Duration::ZERO);
    let grant_fast = Arc::new(
        EgressDatabaseGrant::new(&ctx, &origin, "sqlns", "SQLD_TOKEN", Some(vault_fast)).unwrap(),
    );
    let bridge = db_bridge(&guard, &ctx, settings.clone(), grant_fast);
    let general =
        tokio::spawn(async move { raw_general_call(&sock, "https://slowdns.example/hold").await });
    // Synchronized on the DNS START (not a fixed sleep): the resolver
    // registers the call before its delay, the permit is taken before the
    // DNS, and the delay (10 s) is above the total budget (3 s) — the
    // general call can never dial; it only holds the slot until the total
    // deadline cuts the resolution.
    within("the slow dns start", async {
        while resolver.calls_for("slowdns.example.") < 1 {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    let error = db_refusal(
        db_fetch(
            &bridge,
            db_request("GET", &format!("{origin}ready"), &[], &[], "db-share-3"),
        )
        .await,
        "the bridge during the general DNS",
    );
    assert_eq!(error, WasmHttpError::Transport);
    let event = &wait_events(&store, "db-share-3", 1).await[0];
    assert_event(
        event,
        "limit",
        Some(429),
        Some("EGRESS_LIMIT"),
        "db-share-3",
    );
    let general_status = general.await.unwrap();
    assert!(
        general_status != 429,
        "the general call itself was admitted first"
    );
    // The general DNS settled (a timeout: the total budget ran out); the
    // slot is free again and the bridge passes.
    within(
        "the general dns event",
        wait_for_event(&store, |e| e.code.as_deref() == Some("EGRESS_TIMEOUT")),
    )
    .await;
    let response = db_fetch(
        &bridge,
        db_request("GET", &format!("{origin}ready"), &[], &[], "db-share-4"),
    )
    .await
    .expect("the bridge passes once the general call released the slot");
    assert_eq!(response.status, 200);
    serve_task.abort();
}

/// One general-egress call over the Unix socket (the real worker
/// protocol): the fake host, the destination in `x-edger-egress-url`.
async fn raw_general_call(sock: &std::path::Path, egress_url: &str) -> u16 {
    let mut stream = within("the unix connect", UnixStream::connect(sock))
        .await
        .expect("the guard socket connects");
    let request = format!(
        "GET / HTTP/1.1\r\nhost: {EGRESS_GUARD_HOST}\r\nx-edger-egress-url: {egress_url}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let mut one = [0u8; 1024];
    loop {
        let n = stream.read(&mut one).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&one[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    status
}

/// Wait for the newest egress event matching `pred`.
async fn wait_for_event(
    store: &OperationalStore,
    pred: impl Fn(&OperationalEvent) -> bool,
) -> OperationalEvent {
    for _ in 0..5_000 {
        if let Some(event) = egress_events(store).iter().find(|event| pred(event)) {
            return event.clone();
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("the matching worker.egress event never arrived")
}
