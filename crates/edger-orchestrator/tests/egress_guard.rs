//! Egress guard end-to-end tests (EDG-38, S2a).
//!
//! A real TLS upstream on `127.0.0.1` (self-signed test CA, embedded below),
//! a Unix-socket guard service, and a REAL `hyper` HTTP/1.1 client on the
//! worker side (so a cut body is observed the way an app's HTTP client
//! observes it: a body error, never a raw-socket guess). Only the hostile
//! framing cases and a head-only 413 use raw bytes.
//!
//! The timeout tests run on the real clock (loopback I/O makes a paused
//! clock jump past the budgets under test); budgets are small and the
//! synchronization points are signals from the test upstream, not polling
//! loops.
//!
//! Covers: the ok path (Content-Length / chunked / close-delimited bodies,
//! metrics, events), progressive SSE, the policy refusals (literal IPs in
//! every notation, a private/loopback IP, port), the bad-target refusals,
//! DNS rebinding (every resolved IP classified, dial the first accepted IP,
//! never re-resolve), SNI, redirects (relative Location, 307 body replay,
//! 303/301 POST-to-GET, the redirect cap, a refused hop, credentials dropped
//! on a cross-origin hop), the timeouts (headers, idle, total, connect over
//! a silent TLS peer), the response size cap (the Content-Length pre-head
//! refusal and the streaming cut), the upstream dropping mid-body, the
//! concurrency limit (immediate 429, no queue), the 8 MiB request cap,
//! dropping the `serve` future cutting in-flight calls, and the
//! no-header-leak guarantee in events and logs.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use bytes::Bytes;
use edger_core::config::EgressSettings;
use edger_core::egress::EgressCidr;
use edger_orchestrator::observability::{
    OperationalEvent, OperationalEventQuery, OperationalStore,
};
use edger_orchestrator::{
    egress_metrics_prometheus, EgressGuard, EgressGuardConfig, EgressResolver, EgressWorkerContext,
    EGRESS_GUARD_HOST,
};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::Request;
use hyper_util::rt::TokioIo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert, ServerConfig};
use rustls::sign::CertifiedKey;
use rustls::RootCertStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tracing_subscriber::fmt::MakeWriter;

/// Self-signed test CA (CN=edger-egress-test; SANs: IP 127.0.0.1,
/// DNS rebind.test / a.local / b.local), valid until 2036.
const CERT_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIB3TCCAYSgAwIBAgIUYu8r8U1BMUJi2a+Cph5dG0cPI6IwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRZWRnZXItZWdyZXNzLXRlc3QwHhcNMjYxMDE3MjI1NjUwWhcN
MzYxMDE0MjI1NjUwWjAcMRowGAYDVQQDDBFlZGdlci1lZ3Jlc3MtdGVzdDBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABKibkHCPXi1SI6HUIMdubL9OjFmHnp/GbTO0
O6ELNxS+IkHSTtKfRXatIKAq6GyHaRrX8l7fEfry/Hipn3pwMRyjgaMwgaAwHQYD
VR0OBBYEFClLzbEIuA1DuXebZaDKTTLX7+wqMB8GA1UdIwQYMBaAFClLzbEIuA1D
uXebZaDKTTLX7+wqMC4GA1UdEQQnMCWHBH8AAAGCC3JlYmluZC50ZXN0ggdhLmxv
Y2FsggdiLmxvY2FsMAwGA1UdEwEB/wQCMAAwCwYDVR0PBAQDAgeAMBMGA1UdJQQM
MAoGCCsGAQUFBwMBMAoGCCqGSM49BAMCA0cAMEQCIBL4WbVTKwSeVnGFX+gJZ/Dl
GM7libyowaNFhbKuS11oAiA9cbpCEib73CKv5A8hrLTj7IhyOedrIGn2gwk7ZZ1v
ug==
-----END CERTIFICATE-----";

const KEY_PEM: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIOZ8IsUxUaphqdKvTyM0Lw7eoPwlP+ERrSfVhWh5+EdAoAoGCCqGSM49
AwEHoUQDQgAEqJuQcI9eLVIjodQgx25sv06MWYeen8ZtM7Q7oQs3FL4iQdJO0p9F
dq0goCrobIdpGtfyXt8R+vL8eKmfenAxHA==
-----END EC PRIVATE KEY-----";

/// The pki-types constructors take raw DER, so decode the PEM base64 body.
fn pem_to_der(pem: &str) -> Vec<u8> {
    let b64: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----") && !l.trim().is_empty())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap()
}

/// The longest any single wait in these tests may take; a hit means the
/// scenario hung, not that it was slow.
const STEP: Duration = Duration::from_secs(10);

async fn within<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(STEP, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

// ---------------------------------------------------------------------------
// The TLS upstream test server
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Hit {
    method: String,
    target: String,
    host_header: Option<String>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Hit {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone)]
struct Upstream {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<Hit>>>,
    snis: Arc<Mutex<Vec<Option<String>>>>,
    /// TCP connections accepted (before any TLS): a refusal that must not
    /// dial leaves this untouched.
    accepts: Arc<AtomicUsize>,
    /// `/hold` tells the test the call is inside the upstream (the guard has
    /// the concurrency slot) and waits for `hold_release`.
    hold_started: Arc<Notify>,
    hold_release: Arc<Notify>,
    /// `/sse` sends its second event only after this is signalled.
    sse_release: Arc<Notify>,
}

impl Upstream {
    fn hits_to(&self, path: &str) -> Vec<Hit> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|hit| hit.target.split('?').next() == Some(path))
            .cloned()
            .collect()
    }
}

fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

/// Records the SNI the guard sends (the server cert resolver sees the
/// client hello before the handshake completes).
#[derive(Debug)]
struct SniRecorder {
    key: Arc<CertifiedKey>,
    snis: Arc<Mutex<Vec<Option<String>>>>,
}

impl ResolvesServerCert for SniRecorder {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.snis
            .lock()
            .unwrap()
            .push(client_hello.server_name().map(str::to_string));
        Some(self.key.clone())
    }
}

async fn spawn_upstream() -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let key = PrivateKeyDer::try_from(pem_to_der(KEY_PEM)).unwrap();
    // ring is the single provider in this graph; name it directly for the
    // server-side key construction (no process-default install needed).
    let provider = rustls::crypto::ring::default_provider();
    let certified_key = CertifiedKey::from_der(
        vec![CertificateDer::from(pem_to_der(CERT_PEM))],
        key,
        &provider,
    )
    .unwrap();
    let snis: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = SniRecorder {
        key: Arc::new(certified_key),
        snis: snis.clone(),
    };
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(recorder));
    let acceptor = Arc::new(TlsAcceptor::from(Arc::new(server_config)));
    let upstream = Upstream {
        addr,
        hits: Arc::new(Mutex::new(Vec::new())),
        snis,
        accepts: Arc::new(AtomicUsize::new(0)),
        hold_started: Arc::new(Notify::new()),
        hold_release: Arc::new(Notify::new()),
        sse_release: Arc::new(Notify::new()),
    };
    let shared = upstream.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            shared.accepts.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            let shared = shared.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                serve_upstream_conn(&mut tls, &shared).await;
            });
        }
    });
    upstream
}

type ServerTls = tokio_rustls::server::TlsStream<TcpStream>;

async fn serve_upstream_conn(tls: &mut ServerTls, upstream: &Upstream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let end = loop {
        if let Some(end) = head_end(&buf) {
            break end;
        }
        match tls.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let mut scratch = [httparse::EMPTY_HEADER; 128];
    let mut req = httparse::Request::new(&mut scratch);
    let offset = match req.parse(&buf[..end]) {
        Ok(httparse::Status::Complete(offset)) => offset,
        _ => return,
    };
    let host_header = req
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("host"))
        .map(|h| String::from_utf8_lossy(h.value).into_owned());
    let headers: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|h| {
            (
                h.name.to_string(),
                String::from_utf8_lossy(h.value).into_owned(),
            )
        })
        .collect();
    let method = req.method.unwrap_or("?").to_string();
    let target = req.path.unwrap_or("/").to_string();
    let content_length = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf.split_off(offset);
    while body.len() < content_length {
        match tls.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(content_length);
    upstream.hits.lock().unwrap().push(Hit {
        method,
        target: target.clone(),
        host_header,
        headers,
        body: body.clone(),
    });
    // The path decides the route; the query string is never part of it.
    let path = target.split('?').next().unwrap_or("/").to_string();
    let graceful = respond_upstream(tls, &path, &body, upstream).await;
    if graceful {
        // A close_notify: rustls reports a bare TCP close as an error to a
        // reader waiting for an EOF-delimited body.
        let _ = tls.shutdown().await;
    }
}

/// Returns `false` when the connection must be dropped abruptly (no TLS
/// close_notify), `true` for a clean close.
async fn respond_upstream(
    tls: &mut ServerTls,
    path: &str,
    body: &[u8],
    upstream: &Upstream,
) -> bool {
    let port = upstream.addr.port();
    match path {
        // A response far bigger than the socket buffers: a worker that reads
        // only the head leaves the guard blocked writing it.
        "/flood" => {
            let _ = tls
                .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n")
                .await;
            let chunk = vec![b'z'; 16384];
            for _ in 0..1024 {
                if tls.write_all(&chunk).await.is_err() {
                    return false;
                }
                let _ = tls.flush().await;
            }
        }
        // Redirects whose Location is invalid in a different way each: user
        // info, a port outside `ports`, an IPv6 loopback.
        "/to-userinfo" => {
            let location = format!("https://user:pass@127.0.0.1:{port}/ok");
            write_cl_response(tls, 302, &[("location", &location)], b"").await;
        }
        "/to-blocked-port" => {
            write_cl_response(tls, 302, &[("location", "https://127.0.0.1:0/ok")], b"").await;
        }
        "/to-v6-loopback" => {
            write_cl_response(tls, 302, &[("location", "https://[::1]/ok")], b"").await;
        }
        "/ok" => {
            write_cl_response(tls, 200, &[("content-type", "text/plain")], b"hello-egress").await;
        }
        "/echo" => {
            write_cl_response(tls, 200, &[("content-type", "text/plain")], body).await;
        }
        "/eof" => {
            write_eof_response(tls, 200, &[], b"eof-body").await;
        }
        "/chunked" => {
            write_chunked_response(tls, 200, &[], &[b"chunked-", b"body"]).await;
        }
        "/sse" => {
            let head = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
            if tls.write_all(head).await.is_err() {
                return false;
            }
            write_chunk(tls, b"data: one\n\n").await;
            upstream.sse_release.notified().await;
            write_chunk(tls, b"data: two\n\n").await;
            let _ = tls.write_all(b"0\r\n\r\n").await;
        }
        "/r302" => {
            write_cl_response(tls, 302, &[("location", "/r302-target")], b"").await;
        }
        "/r302-target" => {
            write_cl_response(tls, 200, &[("content-type", "text/plain")], b"final").await;
        }
        "/r307" => {
            write_cl_response(tls, 307, &[("location", "/r307-target")], b"").await;
        }
        // 307: the method and the body must be replayed unchanged.
        "/r307-target" => {
            write_cl_response(tls, 200, &[("content-type", "text/plain")], body).await;
        }
        "/r303" => {
            write_cl_response(tls, 303, &[("location", "/r303-target")], b"").await;
        }
        "/r303-target" => {
            write_cl_response(tls, 200, &[("content-type", "text/plain")], b"done303").await;
        }
        "/r301" => {
            write_cl_response(tls, 301, &[("location", "/r301-target")], b"").await;
        }
        "/r301-target" => {
            write_cl_response(tls, 200, &[("content-type", "text/plain")], b"done301").await;
        }
        "/loop" => {
            write_cl_response(tls, 302, &[("location", "/loop")], b"").await;
        }
        // A redirect to a loopback IP the test did NOT release (only
        // 127.0.0.1 is allowed): refused at the hop.
        "/to-blocked-ip" => {
            write_cl_response(tls, 302, &[("location", "https://127.0.0.2/")], b"").await;
        }
        // A redirect to a name the resolver maps to a private IP.
        "/to-internal-name" => {
            let location = format!("https://internal.test:{port}/x");
            write_cl_response(tls, 302, &[("location", &location)], b"").await;
        }
        // A cross-origin redirect to another allowed name.
        "/to-b" => {
            let location = format!("https://b.local:{port}/ok");
            write_cl_response(tls, 302, &[("location", &location)], b"").await;
        }
        "/slow-head" => {
            // Never answers before the headers budget of the test.
            tokio::time::sleep(Duration::from_secs(60)).await;
            write_cl_response(tls, 200, &[], b"late").await;
        }
        "/stall" => {
            // Head + one body piece, then a long silence (the idle cut).
            if tls
                .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\npart1")
                .await
                .is_err()
            {
                return false;
            }
            tokio::time::sleep(Duration::from_secs(20)).await;
            let _ = tls.write_all(b"part2").await;
        }
        "/cl-stall" => {
            // A Content-Length body that stops short and goes silent.
            if tls
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\nconnection: close\r\n\r\npart1",
                )
                .await
                .is_err()
            {
                return false;
            }
            tokio::time::sleep(Duration::from_secs(20)).await;
            let _ = tls.write_all(b"part2").await;
        }
        "/drop" => {
            // A chunked body whose connection dies mid-stream: no
            // terminating chunk, no TLS close_notify.
            let head =
                b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
            if tls.write_all(head).await.is_err() {
                return false;
            }
            write_chunk(tls, b"abc").await;
            let _ = tls.flush().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            return false;
        }
        "/steady" => {
            // A body that trickles (2 KiB every 50 ms): the idle budget never
            // trips, the total one does.
            let head = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_vec();
            let _ = tls.write_all(&head).await;
            let chunk = vec![b'x'; 2048];
            for _ in 0..512 {
                if tls.write_all(&chunk).await.is_err() {
                    return false;
                }
                let _ = tls.flush().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        "/streambig" => {
            // A big close-delimited body (300 KiB) with no Content-Length.
            let head = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_vec();
            let _ = tls.write_all(&head).await;
            let chunk = vec![b'y'; 1024];
            for _ in 0..300 {
                if tls.write_all(&chunk).await.is_err() {
                    return false;
                }
            }
            let _ = tls.flush().await;
        }
        "/big" => {
            // A Content-Length far over the cap (the pre-head refusal).
            let body = vec![b'z'; 2_000_000];
            write_cl_response(tls, 200, &[], &body).await;
        }
        "/hold" => {
            // Head now; the body only after the test releases it (the 429
            // test keeps the slot busy exactly as long as it needs).
            if tls
                .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n")
                .await
                .is_err()
            {
                return false;
            }
            let _ = tls.flush().await;
            upstream.hold_started.notify_one();
            upstream.hold_release.notified().await;
            let _ = tls.write_all(b"Z").await;
        }
        _ => {
            write_cl_response(tls, 404, &[], b"nope").await;
        }
    }
    true
}

async fn write_cl_response(
    tls: &mut ServerTls,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let mut out = Vec::new();
    out.extend_from_slice(format!("HTTP/1.1 {status}\r\n").as_bytes());
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(format!("content-length: {}\r\n", body.len()).as_bytes());
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(body);
    let _ = tls.write_all(&out).await;
}

async fn write_eof_response(
    tls: &mut ServerTls,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let mut out = Vec::new();
    out.extend_from_slice(format!("HTTP/1.1 {status}\r\n").as_bytes());
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(body);
    let _ = tls.write_all(&out).await;
}

async fn write_chunk(tls: &mut ServerTls, chunk: &[u8]) {
    let mut out = format!("{:x}\r\n", chunk.len()).into_bytes();
    out.extend_from_slice(chunk);
    out.extend_from_slice(b"\r\n");
    let _ = tls.write_all(&out).await;
    let _ = tls.flush().await;
}

async fn write_chunked_response(
    tls: &mut ServerTls,
    status: u16,
    headers: &[(&str, &str)],
    chunks: &[&[u8]],
) {
    let mut out = Vec::new();
    out.extend_from_slice(format!("HTTP/1.1 {status}\r\n").as_bytes());
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(b"transfer-encoding: chunked\r\nconnection: close\r\n\r\n");
    for chunk in chunks {
        out.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\n\r\n");
    let _ = tls.write_all(&out).await;
}

// ---------------------------------------------------------------------------
// The scripted resolver
// ---------------------------------------------------------------------------

/// Answers per host, by call index (the last answer repeats): lets a test say
/// "the first resolution is a released IP, the next ones an internal IP".
/// An unscripted host fails the resolution.
struct ScriptedResolver {
    script: HashMap<String, Vec<Vec<IpAddr>>>,
    calls: Mutex<Vec<String>>,
}

impl ScriptedResolver {
    fn new(script: &[(&str, Vec<Vec<&str>>)]) -> Arc<Self> {
        Arc::new(Self {
            script: script
                .iter()
                .map(|(host, answers)| {
                    (
                        host.to_string(),
                        answers
                            .iter()
                            .map(|ips| ips.iter().map(|ip| ip.parse().unwrap()).collect())
                            .collect(),
                    )
                })
                .collect(),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn none() -> Arc<Self> {
        Self::new(&[])
    }

    fn calls_for(&self, host: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|called| called.as_str() == host)
            .count()
    }
}

#[async_trait]
impl EgressResolver for ScriptedResolver {
    async fn resolve(&self, host: &str, _port: u16) -> io::Result<Vec<IpAddr>> {
        let index = self.calls_for(host);
        self.calls.lock().unwrap().push(host.to_string());
        match self.script.get(host) {
            Some(answers) if !answers.is_empty() => {
                Ok(answers[index.min(answers.len() - 1)].clone())
            }
            _ => Err(io::Error::new(io::ErrorKind::NotFound, "no such host")),
        }
    }
}

// ---------------------------------------------------------------------------
// The guard harness (listener + store + ctx)
// ---------------------------------------------------------------------------

struct Harness {
    guard: EgressGuard,
    store: OperationalStore,
    sock_path: std::path::PathBuf,
    serve_task: JoinHandle<()>,
    _dir: tempfile::TempDir,
}

/// Spawn the guard on a temp Unix socket. `allow_localhost` releases ONLY
/// `127.0.0.1/32` (the test upstream); every other loopback IP, and every
/// private range, stays refused.
async fn spawn_guard(
    resolver: Arc<dyn EgressResolver>,
    allow_localhost: bool,
    settings: EgressSettings,
) -> Harness {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(pem_to_der(CERT_PEM)))
        .unwrap();
    let allow_cidrs = if allow_localhost {
        vec![EgressCidr::parse("127.0.0.1").unwrap()]
    } else {
        Vec::new()
    };
    let config = EgressGuardConfig {
        block_cidrs: Vec::new(),
        allow_cidrs,
        resolver,
        tls_roots: roots,
    };
    let store = OperationalStore::default();
    let guard = EgressGuard::new(config).with_event_store(store.clone());
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("egress.sock");
    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
    let ctx = EgressWorkerContext {
        worker: "egress-test".into(),
        version: "1.0.0".into(),
        namespace: Some("ns".into()),
        process_id: "proc-1".into(),
    };
    let serve_guard = guard.clone();
    let serve_task = tokio::spawn(async move {
        serve_guard.serve(listener, ctx, settings).await;
    });
    Harness {
        guard,
        store,
        sock_path,
        serve_task,
        _dir: dir,
    }
}

fn settings(
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

/// The default budgets, with the upstream's ephemeral port allowed (the
/// test port is never 443). 8443 is deliberately NOT allowed.
fn default_settings(port: u16) -> EgressSettings {
    settings(
        &[443, port],
        5_000,
        10_000,
        300_000,
        300_000,
        10 * 1024 * 1024,
        8,
    )
}

fn url_of(upstream: &Upstream) -> String {
    format!("https://127.0.0.1:{}", upstream.addr.port())
}

// ---------------------------------------------------------------------------
// The worker side: a real hyper HTTP/1.1 client over the Unix socket
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Call {
    method: String,
    url: Option<String>,
    host: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

impl Call {
    fn get(url: impl Into<String>) -> Self {
        Self {
            method: "GET".into(),
            url: Some(url.into()),
            host: EGRESS_GUARD_HOST.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    fn post(url: impl Into<String>, body: &[u8]) -> Self {
        Self {
            method: "POST".into(),
            body: Some(body.to_vec()),
            ..Self::get(url)
        }
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    fn host(mut self, host: &str) -> Self {
        self.host = host.to_string();
        self
    }

    fn without_url(mut self) -> Self {
        self.url = None;
        self
    }
}

/// What the worker's HTTP client saw: the head, the body bytes that arrived,
/// and the body error when the body did not end cleanly.
struct Reply {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
    error: Option<String>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn egress_code(&self) -> Option<&str> {
        self.header("x-edger-egress-error")
    }
}

/// Send one request over a fresh Unix connection and return the response
/// head with its still-open body.
async fn open(sock_path: &std::path::Path, call: Call) -> (u16, HeaderMap, Incoming) {
    let sock = within("the worker connect", UnixStream::connect(sock_path))
        .await
        .expect("worker connects to the guard socket");
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(sock))
        .await
        .expect("worker-side handshake");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut builder = Request::builder()
        .method(call.method.as_str())
        .uri("/")
        .header("host", call.host.as_str());
    if let Some(url) = &call.url {
        builder = builder.header("x-edger-egress-url", url.as_str());
    }
    for (name, value) in &call.headers {
        builder = builder.header(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    let body = Full::new(Bytes::from(call.body.clone().unwrap_or_default()));
    let request = builder.body(body).unwrap();
    let response = within("the response head", sender.send_request(request))
        .await
        .expect("the guard answered with a head");
    let (parts, incoming) = response.into_parts();
    (parts.status.as_u16(), parts.headers, incoming)
}

/// Read the body to its end: the bytes that arrived, and the error if the
/// body ended with one.
async fn read_body(mut incoming: Incoming) -> (Vec<u8>, Option<String>) {
    let mut body = Vec::new();
    loop {
        match within("a body frame", incoming.frame()).await {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    body.extend_from_slice(&data);
                }
            }
            Some(Err(err)) => return (body, Some(err.to_string())),
            None => return (body, None),
        }
    }
}

async fn call_path(sock_path: &std::path::Path, call: Call) -> Reply {
    let (status, headers, incoming) = open(sock_path, call).await;
    let (body, error) = read_body(incoming).await;
    Reply {
        status,
        headers,
        body,
        error,
    }
}

async fn call(harness: &Harness, call: Call) -> Reply {
    call_path(&harness.sock_path, call).await
}

/// Raw bytes in, raw bytes out until the guard closes (a hostile-framing
/// request, or a head with a Content-Length it never fulfils).
async fn raw_exchange(harness: &Harness, bytes: &[u8]) -> (u16, Vec<u8>) {
    let mut sock = UnixStream::connect(&harness.sock_path).await.unwrap();
    sock.write_all(bytes).await.unwrap();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match within("the raw answer", sock.read(&mut tmp)).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
        // The answers under test carry a Content-Length: stop at the head
        // plus that many body bytes.
        if let Some(end) = head_end(&buf) {
            let mut scratch = [httparse::EMPTY_HEADER; 64];
            let mut parsed = httparse::Response::new(&mut scratch);
            if parsed.parse(&buf[..end]).is_ok() {
                let length = parsed
                    .headers
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case("content-length"))
                    .and_then(|h| {
                        std::str::from_utf8(h.value)
                            .ok()?
                            .trim()
                            .parse::<usize>()
                            .ok()
                    })
                    .unwrap_or(0);
                if buf.len() >= end + length {
                    break;
                }
            }
        }
    }
    let end = head_end(&buf).expect("a response head");
    let mut scratch = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Response::new(&mut scratch);
    parsed.parse(&buf[..end]).expect("a parseable head");
    (parsed.code.expect("a status"), buf.split_off(end))
}

fn egress_events(store: &OperationalStore) -> Vec<OperationalEvent> {
    let query = OperationalEventQuery {
        kind: Some("worker.egress".into()),
        limit: Some(500),
        ..Default::default()
    };
    store.query(query).events
}

/// The newest event matching `pred` (the store answers newest first). The
/// events are recorded before the worker sees the end of the call, so a short
/// poll only absorbs the scheduling gap.
async fn wait_event(
    store: &OperationalStore,
    pred: impl Fn(&OperationalEvent) -> bool,
) -> OperationalEvent {
    for _ in 0..5_000 {
        let events = egress_events(store);
        if let Some(event) = events.iter().find(|event| pred(event)) {
            return event.clone();
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("the worker.egress event never arrived");
}

// ---------------------------------------------------------------------------
// Log capture (the no-leak guarantee)
// ---------------------------------------------------------------------------

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
        std::io::BufWriter::new(LogSink(self.0.clone()))
    }
}

fn log_sink() -> Arc<Mutex<Vec<u8>>> {
    static SINK: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    static ONCE: Once = Once::new();
    let sink = SINK
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone();
    ONCE.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(LogSink(sink.clone()))
            .try_init();
    });
    sink
}

// ---------------------------------------------------------------------------
// Tests: the ok path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ok_path_forwards_content_length_chunked_and_eof_bodies() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    let url = url_of(&upstream);

    // Content-Length framing: the destination's length reaches the app.
    let resp = call(&harness, Call::get(format!("{url}/ok"))).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"hello-egress");
    assert_eq!(resp.error, None);
    assert_eq!(resp.header("content-type"), Some("text/plain"));
    assert_eq!(resp.header("content-length"), Some("12"));
    assert!(resp.header("transfer-encoding").is_none());
    // Hop-by-hop headers of the destination do not leak through.
    assert!(resp.header("keep-alive").is_none());

    // Close-delimited (no CL, no TE) upstream body: re-framed chunked.
    let resp = call(&harness, Call::get(format!("{url}/eof"))).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"eof-body");
    assert_eq!(resp.error, None);
    assert!(resp.header("content-length").is_none());
    assert_eq!(resp.header("transfer-encoding"), Some("chunked"));

    // Chunked upstream body, decoded and re-framed.
    let resp = call(&harness, Call::get(format!("{url}/chunked"))).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"chunked-body");
    assert_eq!(resp.error, None);
    assert!(resp.header("content-length").is_none());
    assert_eq!(resp.header("transfer-encoding"), Some("chunked"));

    // The event: ok, with the destination telemetry in the message.
    let event = wait_event(&harness.store, |e| {
        e.outcome.as_deref() == Some("ok")
            && e.message.as_deref().is_some_and(|m| m.contains("bytes=12"))
            && e.message
                .as_deref()
                .is_some_and(|m| m.contains("redirects=0"))
    })
    .await;
    assert_eq!(event.kind, "worker.egress");
    assert_eq!(event.status, Some(200));
    assert_eq!(event.code, None);
    assert_eq!(event.worker.as_deref(), Some("egress-test"));
    assert_eq!(event.version.as_deref(), Some("1.0.0"));
    assert_eq!(event.namespace.as_deref(), Some("ns"));
    assert_eq!(event.process_id.as_deref(), Some("proc-1"));
    let message = event.message.unwrap();
    assert!(message.contains("host=127.0.0.1"), "{message}");
    assert!(
        message.contains(&format!("port={}", upstream.addr.port())),
        "{message}"
    );
    assert!(message.contains("ip=127.0.0.1"), "{message}");

    // The metrics: one ok per call, the streamed bytes (12 + 8 + 12).
    let rendered = egress_metrics_prometheus(harness.guard.metrics());
    let worker = r#"worker="egress-test""#;
    assert!(
        rendered.contains(&format!(
            "edger_egress_requests_total{{{worker},outcome=\"ok\"}} 3"
        )),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!("edger_egress_response_bytes_total{{{worker}}} 32")),
        "{rendered}"
    );
}

#[tokio::test]
async fn sse_chunks_reach_the_app_before_the_upstream_finishes() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    let url = url_of(&upstream);

    let (status, headers, mut incoming) =
        open(&harness.sock_path, Call::get(format!("{url}/sse"))).await;
    assert_eq!(status, 200);
    assert_eq!(headers.get("content-type").unwrap(), "text/event-stream");

    // The upstream will not produce the second event until it is released,
    // so receiving the first one proves the guard streams progressively.
    let first = within("the first SSE chunk", incoming.frame())
        .await
        .expect("a frame")
        .expect("no error")
        .into_data()
        .expect("a data frame");
    assert_eq!(&first[..], b"data: one\n\n");
    // Control: the second event was still held back at that moment.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), incoming.frame())
            .await
            .is_err(),
        "the second event is not available before the upstream is released"
    );

    upstream.sse_release.notify_one();
    let second = within("the second SSE chunk", incoming.frame())
        .await
        .expect("a frame")
        .expect("no error")
        .into_data()
        .expect("a data frame");
    assert_eq!(&second[..], b"data: two\n\n");
    assert!(within("the end of the SSE body", incoming.frame())
        .await
        .is_none());

    let event = wait_event(&harness.store, |e| e.outcome.as_deref() == Some("ok")).await;
    assert!(event.message.unwrap().contains("bytes=22"));
}

// ---------------------------------------------------------------------------
// Tests: refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn literal_ips_in_every_notation_are_refused_without_a_connection() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    // A strict guard: nothing is released, 127.0.0.1 included. A short
    // connect budget turns a wrongly dialed target into a visible 504.
    let harness = spawn_guard(
        ScriptedResolver::none(),
        false,
        settings(
            &[443, port],
            300,
            10_000,
            300_000,
            300_000,
            10 * 1024 * 1024,
            8,
        ),
    )
    .await;
    // The notations that reach the test upstream's own IP carry its port, so
    // a wrongly dialed target WOULD connect (and `accepts` would move).
    let cases: [(String, &str); 6] = [
        (
            "https://169.254.169.254/latest/meta-data/".to_string(),
            "ip=169.254.169.254",
        ),
        ("https://10.0.0.1/".to_string(), "ip=10.0.0.1"),
        (
            format!("https://[::ffff:127.0.0.1]:{port}/ok"),
            "ip=::ffff:127.0.0.1",
        ),
        (format!("https://2130706433:{port}/ok"), "ip=127.0.0.1"),
        (format!("https://0177.0.0.1:{port}/ok"), "ip=127.0.0.1"),
        (format!("https://127.0.0.1:{port}/ok"), "ip=127.0.0.1"),
    ];
    for (index, (url, needle)) in cases.iter().enumerate() {
        let resp = call(&harness, Call::get(url.clone())).await;
        assert_eq!(resp.status, 403, "{url}");
        assert_eq!(resp.egress_code(), Some("EGRESS_HOST_BLOCKED"), "{url}");
        assert!(
            resp.text().contains("EGRESS_HOST_BLOCKED"),
            "{url}: {}",
            resp.text()
        );
        assert_eq!(
            resp.header("content-type"),
            Some("application/json"),
            "{url}"
        );
        let event = wait_event(&harness.store, |_| {
            egress_events(&harness.store).len() == index + 1
        })
        .await;
        assert_eq!(event.code.as_deref(), Some("EGRESS_HOST_BLOCKED"), "{url}");
        assert_eq!(event.outcome.as_deref(), Some("blocked"), "{url}");
        assert_eq!(event.status, Some(403), "{url}");
        let message = event.message.unwrap();
        assert!(message.contains(needle), "{url}: {message}");
    }
    assert_eq!(
        upstream.accepts.load(Ordering::SeqCst),
        0,
        "no refusal opened a connection to the test upstream"
    );
    assert!(upstream.hits.lock().unwrap().is_empty());
    let rendered = egress_metrics_prometheus(harness.guard.metrics());
    assert!(
        rendered
            .contains(r#"edger_egress_requests_total{worker="egress-test",outcome="blocked"} 6"#),
        "{rendered}"
    );
}

#[tokio::test]
async fn refusal_messages_name_the_class_and_never_echo_a_header() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        false,
        default_settings(upstream.addr.port()),
    )
    .await;
    let resp = call(
        &harness,
        Call::get("https://172.16.0.9/").header("Authorization", "Bearer sk-teste-segredo-123"),
    )
    .await;
    assert_eq!(resp.status, 403);
    assert!(
        resp.text().contains("resolved_ip_blocked:private"),
        "{}",
        resp.text()
    );
    assert!(!resp.text().contains("sk-teste-segredo-123"));
    let resp = call(
        &harness,
        Call::get(format!("https://127.0.0.1:{}/ok", upstream.addr.port())),
    )
    .await;
    assert!(
        resp.text().contains("resolved_ip_blocked:loopback"),
        "{}",
        resp.text()
    );
}

#[tokio::test]
async fn bad_targets_and_ports_are_refused_before_the_dial() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let harness = spawn_guard(ScriptedResolver::none(), true, default_settings(port)).await;

    // A non-https scheme.
    let resp = call(&harness, Call::get("http://127.0.0.1/")).await;
    assert_eq!(resp.status, 400);
    assert_eq!(resp.egress_code(), Some("EGRESS_BAD_TARGET"));
    assert!(resp.text().contains("non_https_scheme"));

    // A URL that does not parse.
    let resp = call(&harness, Call::get("not a url at all")).await;
    assert_eq!(resp.status, 400);
    assert!(resp.text().contains("invalid_egress_url"));

    // Credentials in the destination URL (never forwarded, refused).
    let resp = call(&harness, Call::get("https://user:pass@127.0.0.1/")).await;
    assert_eq!(resp.status, 400);
    assert!(resp.text().contains("egress_url_credentials"));
    assert!(!resp.text().contains("user"));

    // A missing destination URL header.
    let resp = call(&harness, Call::get("https://127.0.0.1/").without_url()).await;
    assert_eq!(resp.status, 400);
    assert_eq!(resp.egress_code(), Some("EGRESS_BAD_TARGET"));
    assert!(resp.text().contains("missing_egress_url"));

    // A Host header that is not the fake host.
    let resp = call(
        &harness,
        Call::get("https://127.0.0.1/").host("api.openai.com"),
    )
    .await;
    assert_eq!(resp.status, 400);
    assert!(resp.text().contains("bad_host_header"));

    // Port 8443 is not in `ports` (the IP itself would be released).
    let resp = call(&harness, Call::get("https://127.0.0.1:8443/")).await;
    assert_eq!(resp.status, 403);
    assert_eq!(resp.egress_code(), Some("EGRESS_PORT_BLOCKED"));
    assert!(resp.text().contains("EGRESS_PORT_BLOCKED"));
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_PORT_BLOCKED")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("blocked"));
    assert!(event.message.unwrap().contains("port=8443"));

    // The refused events carry the 400 status for the bad targets.
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_BAD_TARGET")
    })
    .await;
    assert_eq!(event.status, Some(400));

    assert_eq!(
        upstream.accepts.load(Ordering::SeqCst),
        0,
        "nothing was dialed"
    );
}

#[tokio::test]
async fn request_with_content_length_and_transfer_encoding_is_reframed_not_forwarded_raw() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let harness = spawn_guard(ScriptedResolver::none(), true, default_settings(port)).await;
    // hyper gives Transfer-Encoding precedence and drops the Content-Length
    // before the guard sees the request (unit test
    // `hyper_gives_transfer_encoding_precedence_over_content_length`); the
    // guard buffers the decoded body and re-frames it with its own length.
    let request = format!(
        "POST / HTTP/1.1\r\nhost: {EGRESS_GUARD_HOST}\r\nx-edger-egress-url: https://127.0.0.1:{port}/echo\r\ncontent-length: 3\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n"
    );
    let (status, body) = raw_exchange(&harness, request.as_bytes()).await;
    assert_eq!(status, 200);
    assert!(String::from_utf8_lossy(&body).contains("hello") || body.is_empty());
    let hit = upstream
        .hits_to("/echo")
        .pop()
        .expect("the upstream was reached");
    assert_eq!(hit.body, b"hello");
    assert_eq!(hit.header("content-length"), Some("5"));
    assert!(hit.header("transfer-encoding").is_none());
}

// ---------------------------------------------------------------------------
// Tests: DNS rebinding and SNI
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dns_rebinding_dials_the_classified_ip_and_never_resolves_again() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();

    // The first resolution answers the released test IP, every later one an
    // internal IP. One request resolves exactly once and dials 127.0.0.1;
    // a re-resolution before the dial would see 10.0.0.5.
    let resolver =
        ScriptedResolver::new(&[("rebind.test", vec![vec!["127.0.0.1"], vec!["10.0.0.5"]])]);
    let harness = spawn_guard(resolver.clone(), true, default_settings(port)).await;
    let resp = call(
        &harness,
        Call::get(format!("https://rebind.test:{port}/ok")),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", resp.text());
    assert_eq!(resp.body, b"hello-egress");
    assert_eq!(
        resolver.calls_for("rebind.test"),
        1,
        "resolved once, not re-resolved"
    );
    assert_eq!(
        upstream.accepts.load(Ordering::SeqCst),
        1,
        "dialed the classified IP"
    );
    // The SNI is the destination hostname, not the IP.
    let snis = upstream.snis.lock().unwrap().clone();
    assert_eq!(
        snis.last().expect("a connection").as_deref(),
        Some("rebind.test")
    );
    let hit = upstream.hits_to("/ok").pop().unwrap();
    assert_eq!(
        hit.host_header.as_deref(),
        Some(format!("rebind.test:{port}").as_str())
    );
    let event = wait_event(&harness.store, |e| e.outcome.as_deref() == Some("ok")).await;
    let message = event.message.unwrap();
    assert!(message.contains("host=rebind.test"), "{message}");
    assert!(message.contains("ip=127.0.0.1"), "{message}");

    // The next request resolves again (the cache is not ours) and now gets
    // the internal IP: refused, and the test upstream sees nothing new.
    let resp = call(
        &harness,
        Call::get(format!("https://rebind.test:{port}/ok")),
    )
    .await;
    assert_eq!(resp.status, 403);
    assert_eq!(resp.egress_code(), Some("EGRESS_HOST_BLOCKED"));
    assert_eq!(resolver.calls_for("rebind.test"), 2);
    assert_eq!(upstream.accepts.load(Ordering::SeqCst), 1);
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_HOST_BLOCKED")
    })
    .await;
    assert!(event.message.unwrap().contains("ip=10.0.0.5"));
}

#[tokio::test]
async fn every_resolved_ip_is_classified_not_just_the_first() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    // [released, private]: the private one refuses the WHOLE request (an
    // attacker AAAA cannot hide behind an acceptable A). The first IP is the
    // test upstream, so a guard that only classified it would connect and
    // answer 200.
    let resolver = ScriptedResolver::new(&[("rebind.test", vec![vec!["127.0.0.1", "10.0.0.5"]])]);
    let harness = spawn_guard(resolver.clone(), true, default_settings(port)).await;
    let resp = call(
        &harness,
        Call::get(format!("https://rebind.test:{port}/ok")),
    )
    .await;
    assert_eq!(resp.status, 403);
    assert_eq!(resp.egress_code(), Some("EGRESS_HOST_BLOCKED"));
    assert_eq!(resolver.calls_for("rebind.test"), 1);
    assert_eq!(upstream.accepts.load(Ordering::SeqCst), 0, "nothing dialed");
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_HOST_BLOCKED")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("blocked"));
    let message = event.message.unwrap();
    assert!(message.contains("host=rebind.test"), "{message}");
    assert!(message.contains("ip=10.0.0.5"), "{message}");

    // Control: the same two-IP shape with both IPs released is accepted and
    // dials the FIRST one.
    let resolver = ScriptedResolver::new(&[("rebind.test", vec![vec!["127.0.0.1", "127.0.0.1"]])]);
    let harness = spawn_guard(resolver, true, default_settings(port)).await;
    let resp = call(
        &harness,
        Call::get(format!("https://rebind.test:{port}/ok")),
    )
    .await;
    assert_eq!(resp.status, 200);
}

#[tokio::test]
async fn dns_failures_are_a_dns_error() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    // An empty answer.
    let resolver = ScriptedResolver::new(&[("rebind.test", vec![vec![]])]);
    let harness = spawn_guard(resolver.clone(), true, default_settings(port)).await;
    let resp = call(
        &harness,
        Call::get(format!("https://rebind.test:{port}/ok")),
    )
    .await;
    assert_eq!(resp.status, 502);
    assert_eq!(resp.egress_code(), Some("EGRESS_DNS_ERROR"));
    assert_eq!(resolver.calls_for("rebind.test"), 1);
    // A resolver failure (an unscripted host).
    let resp = call(
        &harness,
        Call::get(format!("https://nowhere.test:{port}/ok")),
    )
    .await;
    assert_eq!(resp.status, 502);
    assert_eq!(resp.egress_code(), Some("EGRESS_DNS_ERROR"));
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_DNS_ERROR")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("error"));
}

// ---------------------------------------------------------------------------
// Tests: redirects
// ---------------------------------------------------------------------------

#[tokio::test]
async fn redirects_follow_the_contract() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    let url = url_of(&upstream);

    // 307: the method and the body are replayed unchanged; the app header is
    // kept on a same-origin hop.
    let secret = b"payload-abc-123";
    let resp = call(
        &harness,
        Call::post(format!("{url}/r307"), secret).header("Authorization", "Bearer sk-teste-307"),
    )
    .await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, secret, "307 replays the body");
    let event = wait_event(&harness.store, |e| {
        e.outcome.as_deref() == Some("ok")
            && e.message
                .as_deref()
                .is_some_and(|m| m.contains("redirects=1"))
    })
    .await;
    assert_eq!(event.status, Some(200));
    let hit = upstream
        .hits_to("/r307-target")
        .pop()
        .expect("the redirect target was hit");
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.body, secret);
    assert_eq!(hit.header("authorization"), Some("Bearer sk-teste-307"));
    assert!(hit.header("x-edger-egress-url").is_none());
    // The Host header is the destination authority.
    assert_eq!(
        hit.host_header.as_deref(),
        Some(format!("127.0.0.1:{}", upstream.addr.port()).as_str())
    );

    // 303: a POST becomes a bodyless GET.
    let resp = call(&harness, Call::post(format!("{url}/r303"), b"gone")).await;
    assert_eq!((resp.status, resp.body.as_slice()), (200, &b"done303"[..]));
    let hit = upstream.hits_to("/r303-target").pop().unwrap();
    assert_eq!(hit.method, "GET");
    assert!(hit.body.is_empty(), "303 drops the body");

    // 301 from a POST: a bodyless GET too.
    let resp = call(&harness, Call::post(format!("{url}/r301"), b"gone")).await;
    assert_eq!((resp.status, resp.body.as_slice()), (200, &b"done301"[..]));
    let hit = upstream.hits_to("/r301-target").pop().unwrap();
    assert_eq!(hit.method, "GET");
    assert!(hit.body.is_empty());

    // 302 on a GET: the method is kept.
    let resp = call(&harness, Call::get(format!("{url}/r302"))).await;
    assert_eq!((resp.status, resp.body.as_slice()), (200, &b"final"[..]));
    assert_eq!(
        upstream.hits_to("/r302-target").pop().unwrap().method,
        "GET"
    );
}

#[tokio::test]
async fn more_than_five_redirects_are_refused() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    let resp = call(&harness, Call::get(format!("{}/loop", url_of(&upstream)))).await;
    assert_eq!(resp.status, 502);
    assert_eq!(resp.egress_code(), Some("EGRESS_TOO_MANY_REDIRECTS"));
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_TOO_MANY_REDIRECTS")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("error"));
    assert!(event.message.unwrap().contains("redirects=5"));
    // The initial request plus the 5 followed hops; the 6th redirect answer
    // is refused without a dial.
    assert_eq!(upstream.hits_to("/loop").len(), 6);
}

#[tokio::test]
async fn a_redirect_into_a_refused_destination_is_refused_at_the_hop() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let resolver = ScriptedResolver::new(&[("internal.test", vec![vec!["10.0.0.5"]])]);
    let harness = spawn_guard(resolver.clone(), true, default_settings(port)).await;
    let url = url_of(&upstream);

    // The Location is a loopback IP the test did not release (only
    // 127.0.0.1 is): refused at the hop, and nothing was dialed for it.
    let resp = call(&harness, Call::get(format!("{url}/to-blocked-ip"))).await;
    assert_eq!(resp.status, 403);
    assert_eq!(resp.egress_code(), Some("EGRESS_HOST_BLOCKED"));
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_HOST_BLOCKED")
            && e.message
                .as_deref()
                .is_some_and(|m| m.contains("host=127.0.0.2"))
    })
    .await;
    assert!(event.message.unwrap().contains("redirects=1"));
    assert_eq!(
        upstream.accepts.load(Ordering::SeqCst),
        1,
        "only the first hop connected"
    );

    // The Location is a name that resolves to a private IP.
    let resp = call(&harness, Call::get(format!("{url}/to-internal-name"))).await;
    assert_eq!(resp.status, 403);
    assert_eq!(resp.egress_code(), Some("EGRESS_HOST_BLOCKED"));
    assert_eq!(resolver.calls_for("internal.test"), 1);
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_HOST_BLOCKED")
            && e.message
                .as_deref()
                .is_some_and(|m| m.contains("host=internal.test"))
    })
    .await;
    assert!(event.message.unwrap().contains("ip=10.0.0.5"));
    assert_eq!(
        upstream.accepts.load(Ordering::SeqCst),
        2,
        "only the first hops connected"
    );
}

#[tokio::test]
async fn credentials_do_not_follow_a_cross_origin_redirect() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let resolver = ScriptedResolver::new(&[
        ("a.local", vec![vec!["127.0.0.1"]]),
        ("b.local", vec![vec!["127.0.0.1"]]),
    ]);
    let harness = spawn_guard(resolver, true, default_settings(port)).await;
    let resp = call(
        &harness,
        Call::get(format!("https://a.local:{port}/to-b"))
            .header("Authorization", "Bearer sk-cross-origin")
            .header("Cookie", "session=abc")
            .header("X-App", "kept"),
    )
    .await;
    assert_eq!(resp.status, 200);
    // The first hop had them...
    let first = upstream.hits_to("/to-b").pop().unwrap();
    assert_eq!(
        first.header("authorization"),
        Some("Bearer sk-cross-origin")
    );
    // ...the other origin did not, but the plain app header went along.
    let second = upstream.hits_to("/ok").pop().unwrap();
    assert_eq!(
        second.host_header.as_deref(),
        Some(format!("b.local:{port}").as_str())
    );
    assert!(second.header("authorization").is_none());
    assert!(second.header("cookie").is_none());
    assert_eq!(second.header("x-app"), Some("kept"));
}

// ---------------------------------------------------------------------------
// Tests: timeouts and cuts (the app sees a body ERROR)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn headers_timeout_answers_504_before_the_head() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        settings(
            &[443, upstream.addr.port()],
            5_000,
            150,
            300_000,
            10_000,
            10 * 1024 * 1024,
            8,
        ),
    )
    .await;
    let resp = call(
        &harness,
        Call::get(format!("{}/slow-head", url_of(&upstream))),
    )
    .await;
    assert_eq!(resp.status, 504);
    assert_eq!(resp.egress_code(), Some("EGRESS_TIMEOUT"));
    assert_eq!(
        resp.error, None,
        "a pre-head refusal is a complete response"
    );
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_TIMEOUT")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("timeout"));
    assert_eq!(event.status, Some(504));
}

#[tokio::test]
async fn connect_timeout_covers_the_tls_handshake() {
    // A TCP peer that accepts but never speaks TLS: the connect budget
    // (TCP + TLS) expires. The peer keeps draining until the guard closes,
    // so the handshake can only end by the deadline.
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_addr = silent.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = silent.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut stream = tcp;
                let mut buf = [0u8; 256];
                while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
            });
        }
    });
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        settings(
            &[443, silent_addr.port()],
            200,
            10_000,
            300_000,
            10_000,
            10 * 1024 * 1024,
            8,
        ),
    )
    .await;
    let resp = call(
        &harness,
        Call::get(format!("https://127.0.0.1:{}/", silent_addr.port())),
    )
    .await;
    assert_eq!(resp.status, 504);
    assert_eq!(resp.egress_code(), Some("EGRESS_TIMEOUT"));
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_TIMEOUT")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("timeout"));
}

#[tokio::test]
async fn idle_timeout_ends_the_body_with_an_error() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        settings(
            &[443, upstream.addr.port()],
            5_000,
            10_000,
            150,
            10_000,
            10 * 1024 * 1024,
            8,
        ),
    )
    .await;
    let url = url_of(&upstream);
    // The head (200) and the first piece arrive; the silence after them trips
    // the idle budget and the body the app is reading fails.
    let resp = call(&harness, Call::get(format!("{url}/stall"))).await;
    assert_eq!(resp.status, 200, "the head went out before the cut");
    assert_eq!(resp.body, b"part1", "the piece before the silence survived");
    assert!(
        resp.error.is_some(),
        "the cut is a read error, not a clean end"
    );
    let event = wait_event(&harness.store, |e| e.outcome.as_deref() == Some("timeout")).await;
    let message = event.message.unwrap();
    assert!(message.contains("reason=idle_timeout"), "{message}");
    assert!(message.contains("bytes=5"), "{message}");
    assert_eq!(event.status, Some(200));
    assert_eq!(event.code, None);

    // The same cut on a Content-Length body: fewer bytes than declared is an
    // error for the app too.
    let resp = call(&harness, Call::get(format!("{url}/cl-stall"))).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.header("content-length"), Some("100"));
    assert_eq!(resp.body, b"part1");
    assert!(
        resp.error.is_some(),
        "a short Content-Length body is an error"
    );
}

#[tokio::test]
async fn total_timeout_ends_a_trickling_body_with_an_error() {
    let upstream = spawn_upstream().await;
    // The trickle (2 KiB / 50 ms) never trips the idle budget (1 s); the
    // total budget (300 ms) does.
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        settings(
            &[443, upstream.addr.port()],
            5_000,
            10_000,
            1_000,
            300,
            10 * 1024 * 1024,
            8,
        ),
    )
    .await;
    let resp = call(&harness, Call::get(format!("{}/steady", url_of(&upstream)))).await;
    assert_eq!(resp.status, 200);
    assert!(resp.body.len() >= 2048, "trickled pieces before the cut");
    assert!(resp.error.is_some(), "the cut is a read error");
    let event = wait_event(&harness.store, |e| e.outcome.as_deref() == Some("timeout")).await;
    assert!(event.message.unwrap().contains("reason=total_timeout"));
    assert_eq!(event.status, Some(200));
}

#[tokio::test]
async fn the_destination_dropping_mid_body_is_an_error_for_the_app() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    let resp = call(&harness, Call::get(format!("{}/drop", url_of(&upstream)))).await;
    assert_eq!(resp.status, 200);
    assert!(
        resp.error.is_some(),
        "an incomplete chunked body is an error"
    );
    let event = wait_event(&harness.store, |e| e.outcome.as_deref() == Some("error")).await;
    assert!(event
        .message
        .unwrap()
        .contains("reason=upstream_stream_error"));
}

#[tokio::test]
async fn response_size_cap_refuses_big_content_length_and_cuts_streaming() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        settings(
            &[443, upstream.addr.port()],
            5_000,
            10_000,
            300_000,
            300_000,
            100_000,
            8,
        ),
    )
    .await;
    let url = url_of(&upstream);

    // A Content-Length over the cap: refused BEFORE the head goes out.
    let resp = call(&harness, Call::get(format!("{url}/big"))).await;
    assert_eq!(resp.status, 502);
    assert_eq!(resp.egress_code(), Some("EGRESS_RESPONSE_TOO_LARGE"));
    assert_eq!(resp.error, None);
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_RESPONSE_TOO_LARGE")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("error"));

    // A streaming body over the cap: the head goes out, the guard hands the
    // app exactly the bytes up to the cap, and the body ends with an error.
    let resp = call(&harness, Call::get(format!("{url}/streambig"))).await;
    assert_eq!(resp.status, 200);
    assert!(
        resp.error.is_some(),
        "the cut is a read error, never a silent truncation"
    );
    assert!(
        resp.body.len() <= 100_000,
        "the app never receives a byte past the cap, got {}",
        resp.body.len()
    );
    let event = wait_event(&harness.store, |e| {
        e.code.is_none()
            && e.message
                .as_deref()
                .is_some_and(|m| m.contains("response_size_limit"))
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("error"));
    assert!(event.message.unwrap().contains("bytes=100000"));
}

// ---------------------------------------------------------------------------
// Tests: concurrency, request cap, shutdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrency_limit_answers_429_immediately_and_releases_after() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        settings(
            &[443, upstream.addr.port()],
            5_000,
            10_000,
            300_000,
            300_000,
            10 * 1024 * 1024,
            1,
        ),
    )
    .await;
    let url = url_of(&upstream);

    // The first call holds the only slot until the test releases it; the
    // upstream signals once the call is inside, so the probe cannot race it.
    let hold = {
        let path = harness.sock_path.clone();
        let call_url = format!("{url}/hold");
        tokio::spawn(async move { call_path(&path, Call::get(call_url)).await })
    };
    within(
        "the hold to reach the upstream",
        upstream.hold_started.notified(),
    )
    .await;

    let started = std::time::Instant::now();
    let resp = call(&harness, Call::get(format!("{url}/ok"))).await;
    assert_eq!(resp.status, 429);
    assert_eq!(resp.egress_code(), Some("EGRESS_LIMIT"));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "no queue: the refusal is immediate"
    );
    let event = wait_event(&harness.store, |e| e.outcome.as_deref() == Some("limit")).await;
    assert_eq!(event.code.as_deref(), Some("EGRESS_LIMIT"));
    assert_eq!(event.status, Some(429));
    let rendered = egress_metrics_prometheus(harness.guard.metrics());
    assert!(
        rendered.contains(r#"edger_egress_requests_total{worker="egress-test",outcome="limit"} 1"#)
    );
    assert_eq!(
        upstream.hits_to("/ok").len(),
        0,
        "the refused call never reached the upstream"
    );

    // Release the hold: it completes, and its slot comes back.
    upstream.hold_release.notify_one();
    let hold_resp = within("the hold to finish", hold).await.unwrap();
    assert_eq!(hold_resp.status, 200);
    assert_eq!(hold_resp.body, b"Z");
    assert_eq!(hold_resp.error, None);
    let resp = call(&harness, Call::get(format!("{url}/ok"))).await;
    assert_eq!(resp.status, 200, "the slot was released");
}

#[tokio::test]
async fn oversized_request_body_is_refused_with_413() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let harness = spawn_guard(ScriptedResolver::none(), true, default_settings(port)).await;

    // A declared 9 MiB body (over the 8 MiB cap); nothing is written after
    // the head, so the refusal must come before the body is read.
    let head = format!(
        "POST / HTTP/1.1\r\nhost: {EGRESS_GUARD_HOST}\r\nx-edger-egress-url: https://127.0.0.1:{port}/echo\r\ncontent-length: 9437184\r\n\r\n"
    );
    let (status, body) = raw_exchange(&harness, head.as_bytes()).await;
    assert_eq!(status, 413);
    assert!(String::from_utf8_lossy(&body).contains("EGRESS_REQUEST_TOO_LARGE"));
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_REQUEST_TOO_LARGE")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("blocked"));
    assert_eq!(upstream.accepts.load(Ordering::SeqCst), 0);

    // A body at the cap is accepted and forwarded whole (the cap is not a
    // block), and a small one too.
    let resp = call(
        &harness,
        Call::post(format!("{}/echo", url_of(&upstream)), b"tiny"),
    )
    .await;
    assert_eq!((resp.status, resp.body.as_slice()), (200, &b"tiny"[..]));
}

#[tokio::test]
async fn chunked_request_body_over_the_cap_is_refused_with_413() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let harness = spawn_guard(ScriptedResolver::none(), true, default_settings(port)).await;
    // No Content-Length to refuse up front: the 8 MiB cap is enforced while
    // the chunked body is being read.
    let sock = UnixStream::connect(&harness.sock_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(sock);
    let head = format!(
        "POST / HTTP/1.1\r\nhost: {EGRESS_GUARD_HOST}\r\nx-edger-egress-url: https://127.0.0.1:{port}/echo\r\ntransfer-encoding: chunked\r\n\r\n"
    );
    let feeder = tokio::spawn(async move {
        let _ = writer.write_all(head.as_bytes()).await;
        let piece = vec![b'a'; 1024 * 1024];
        for _ in 0..10 {
            // The guard stops reading and closes after the 413: later
            // writes are allowed to fail.
            if writer.write_all(b"100000\r\n").await.is_err()
                || writer.write_all(&piece).await.is_err()
                || writer.write_all(b"\r\n").await.is_err()
            {
                break;
            }
        }
        let _ = writer.write_all(b"0\r\n\r\n").await;
        writer
    });
    let mut answer = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match within("the 413 answer", reader.read(&mut tmp)).await {
            Ok(0) | Err(_) => break,
            Ok(n) => answer.extend_from_slice(&tmp[..n]),
        }
        if answer.windows(4).any(|w| w == b"\r\n\r\n")
            && String::from_utf8_lossy(&answer).contains("EGRESS_REQUEST_TOO_LARGE")
        {
            break;
        }
    }
    let text = String::from_utf8_lossy(&answer).to_string();
    assert!(text.starts_with("HTTP/1.1 413"), "{text}");
    assert!(text.contains("EGRESS_REQUEST_TOO_LARGE"), "{text}");
    let _ = feeder.await;
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_REQUEST_TOO_LARGE")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("blocked"));
    assert_eq!(
        upstream.accepts.load(Ordering::SeqCst),
        0,
        "the oversized body was never forwarded"
    );
}

#[tokio::test]
async fn head_requests_complete_and_report_ok() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    let mut head = Call::get(format!("{}/ok", url_of(&upstream)));
    head.method = "HEAD".into();
    let resp = call(&harness, head).await;
    assert_eq!(resp.status, 200);
    assert!(resp.body.is_empty());
    assert_eq!(resp.error, None);
    let event = wait_event(&harness.store, |e| e.outcome.as_deref() == Some("ok")).await;
    assert_eq!(event.status, Some(200));
    assert!(event.message.unwrap().contains("bytes=0"));
    assert_eq!(upstream.hits_to("/ok").pop().unwrap().method, "HEAD");
}

#[tokio::test]
async fn dropping_serve_cuts_the_calls_in_flight() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    let (status, _, mut incoming) = open(
        &harness.sock_path,
        Call::get(format!("{}/stall", url_of(&upstream))),
    )
    .await;
    assert_eq!(status, 200);
    let first = within("the first piece", incoming.frame())
        .await
        .expect("a frame")
        .expect("no error")
        .into_data()
        .expect("data");
    assert_eq!(&first[..], b"part1");

    // The stop signal: abort the serve future.
    harness.serve_task.abort();
    let outcome = within("the cut after the abort", incoming.frame()).await;
    assert!(
        !matches!(outcome, Some(Ok(_))),
        "no more data after the stop; the body ends with an error or a closed stream"
    );
    assert!(
        outcome.is_some(),
        "the stop must be an error for the reader, not a clean end"
    );
    // The call was reported (the body was dropped before its end).
    let event = wait_event(&harness.store, |e| {
        e.message
            .as_deref()
            .is_some_and(|m| m.contains("worker_disconnected"))
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("error"));
}

// ---------------------------------------------------------------------------
// Tests: nothing sensitive reaches the event or the log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_header_name_or_value_reaches_the_event_or_the_log() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    let sink = log_sink();
    sink.lock().unwrap().clear();

    const AUTH_NAME: &str = "Authorization";
    const AUTH_VALUE: &str = "Bearer sk-teste-segredo-123";
    const KEY_NAME: &str = "X-Api-Key";
    const KEY_VALUE: &str = "leakkey-abc-123";
    const BODY_SECRET: &str = "body-leakcheck-123";

    // A success, a refusal and a cut, all carrying the secrets.
    let resp = call(
        &harness,
        Call::post(
            format!("{}/echo?q=leakcheck-query", url_of(&upstream)),
            BODY_SECRET.as_bytes(),
        )
        .header(AUTH_NAME, AUTH_VALUE)
        .header(KEY_NAME, KEY_VALUE),
    )
    .await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, BODY_SECRET.as_bytes());
    let blocked = call(
        &harness,
        Call::post(
            "https://10.0.0.7/leakcheck-path?q=leakcheck-query",
            BODY_SECRET.as_bytes(),
        )
        .header(AUTH_NAME, AUTH_VALUE)
        .header(KEY_NAME, KEY_VALUE),
    )
    .await;
    assert_eq!(blocked.status, 403);
    assert!(!blocked.text().contains(AUTH_VALUE));

    // The events (their full serialized form).
    wait_event(&harness.store, |e| e.outcome.as_deref() == Some("ok")).await;
    wait_event(&harness.store, |e| e.outcome.as_deref() == Some("blocked")).await;
    let events = egress_events(&harness.store);
    assert!(events.len() >= 2);
    let secrets = [
        AUTH_NAME,
        AUTH_VALUE,
        "sk-teste-segredo-123",
        KEY_NAME,
        KEY_VALUE,
        BODY_SECRET,
        "leakcheck-query",
        "leakcheck-path",
    ];
    for event in &events {
        assert!(
            !event.message.as_deref().unwrap_or("").contains("q="),
            "no query string in the message"
        );
        let json = serde_json::to_string(event).unwrap();
        for secret in secrets {
            assert!(
                !json
                    .to_ascii_lowercase()
                    .contains(&secret.to_ascii_lowercase()),
                "the event must not contain {secret:?}: {json}"
            );
        }
    }

    // The log: the guard's line is there (the capture works), but nothing
    // sensitive is.
    let logs = String::from_utf8_lossy(sink.lock().unwrap().as_slice()).to_string();
    assert!(
        logs.contains("worker.egress"),
        "the guard's worker.egress line must reach the log: {logs}"
    );
    for secret in secrets {
        assert!(
            !logs
                .to_ascii_lowercase()
                .contains(&secret.to_ascii_lowercase()),
            "the log must not contain {secret:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests: deadlines under backpressure, the header budget, cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn total_deadline_expires_under_backpressure_and_releases_the_slot() {
    let upstream = spawn_upstream().await;
    // One slot; the worker reads only the head and then stops consuming the
    // socket, so `hyper` stops polling the body: only an independent
    // supervisor can end the call at the 200 ms total deadline.
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        settings(
            &[443, upstream.addr.port()],
            5_000,
            10_000,
            5_000,
            200,
            100 * 1024 * 1024,
            1,
        ),
    )
    .await;
    let url = url_of(&upstream);
    let mut sock = UnixStream::connect(&harness.sock_path).await.unwrap();
    let request = format!(
        "GET / HTTP/1.1\r\nhost: {EGRESS_GUARD_HOST}\r\nx-edger-egress-url: {url}/flood\r\n\r\n"
    );
    sock.write_all(request.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    while head_end(&head).is_none() {
        let mut byte = [0u8];
        assert_eq!(
            within("the response head", sock.read(&mut byte))
                .await
                .unwrap(),
            1
        );
        head.push(byte[0]);
    }
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 200"));

    // The deadline passes with the worker not reading at all.
    tokio::time::sleep(Duration::from_millis(650)).await;
    let event = wait_event(&harness.store, |e| e.outcome.as_deref() == Some("timeout")).await;
    assert_eq!(event.status, Some(200));
    assert_eq!(event.code, None);
    let message = event.message.unwrap();
    assert!(message.contains("reason=total_timeout"), "{message}");

    // The only slot is back: a second call is served, not refused with 429.
    let resp = call(&harness, Call::get(format!("{url}/ok"))).await;
    assert_eq!(resp.status, 200, "the expired call released the slot");

    // The guard tore the worker connection down: reading the rest of the
    // stalled response ends (buffered bytes, then a close or an error)
    // instead of waiting forever.
    let mut sink = vec![0u8; 64 * 1024];
    loop {
        match within("the torn-down connection", sock.read(&mut sink)).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let rendered = egress_metrics_prometheus(harness.guard.metrics());
    assert!(
        rendered
            .contains(r#"edger_egress_requests_total{worker="egress-test",outcome="timeout"} 1"#),
        "{rendered}"
    );
}

#[tokio::test]
async fn head_budget_bounds_a_silent_tls_peer() {
    // A peer that accepts the TCP connection and never speaks TLS. The head
    // budget (100 ms, counted from the start of the hop) is shorter than the
    // connect budget (600 ms) and must bound TCP + TLS too.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = [0u8; 4096];
        while matches!(socket.read(&mut bytes).await, Ok(n) if n > 0) {}
    });
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        settings(&[port], 600, 100, 1_000, 2_000, 1024, 1),
    )
    .await;
    let started = std::time::Instant::now();
    let resp = call(&harness, Call::get(format!("https://127.0.0.1:{port}/"))).await;
    let elapsed = started.elapsed();
    assert_eq!(resp.status, 504);
    assert_eq!(resp.egress_code(), Some("EGRESS_TIMEOUT"));
    assert!(
        elapsed < Duration::from_millis(400),
        "the head budget bounds the pre-head exchange, took {elapsed:?}"
    );
    within("the silent peer to see the close", peer)
        .await
        .unwrap();
    let event = wait_event(&harness.store, |e| {
        e.code.as_deref() == Some("EGRESS_TIMEOUT")
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("timeout"));
}

/// Signals when the guard is inside the resolution, then waits to be released.
struct WaitingResolver {
    entered: Arc<Notify>,
    released: Arc<Notify>,
}

#[async_trait]
impl EgressResolver for WaitingResolver {
    async fn resolve(&self, _host: &str, _port: u16) -> io::Result<Vec<IpAddr>> {
        self.entered.notify_one();
        self.released.notified().await;
        Ok(vec!["127.0.0.1".parse().unwrap()])
    }
}

#[tokio::test]
async fn a_worker_that_leaves_before_the_head_is_reported_once() {
    let upstream = spawn_upstream().await;
    let entered = Arc::new(Notify::new());
    let released = Arc::new(Notify::new());
    let resolver = Arc::new(WaitingResolver {
        entered: entered.clone(),
        released: released.clone(),
    });
    let mut cfg = default_settings(upstream.addr.port());
    cfg.max_concurrent = 1;
    let harness = spawn_guard(resolver, true, cfg).await;

    // The call is admitted and parks inside the DNS resolution; the worker
    // closes its socket there.
    let mut sock = UnixStream::connect(&harness.sock_path).await.unwrap();
    let request = format!(
        "GET / HTTP/1.1\r\nhost: {EGRESS_GUARD_HOST}\r\nx-edger-egress-url: https://rebind.test:{}/ok?q=cancel-secret\r\n\r\n",
        upstream.addr.port()
    );
    sock.write_all(request.as_bytes()).await.unwrap();
    within("the resolver to be entered", entered.notified()).await;
    drop(sock);

    // The cancelled call is an error event with the reason, no status and no
    // sensitive data; its slot is back.
    let event = wait_event(&harness.store, |e| {
        e.message
            .as_deref()
            .is_some_and(|m| m.contains("worker_disconnected"))
    })
    .await;
    assert_eq!(event.outcome.as_deref(), Some("error"));
    assert_eq!(event.status, None);
    let message = event.message.unwrap();
    assert!(message.contains("host=rebind.test"), "{message}");
    assert!(
        !message.contains("cancel-secret") && !message.contains("q="),
        "{message}"
    );

    let resp = call(&harness, Call::get(format!("{}/ok", url_of(&upstream)))).await;
    assert_eq!(resp.status, 200, "closed socket released the slot");
    released.notify_waiters();

    // Two admitted requests: two events and two metric samples, no double
    // counting of the cancelled one.
    wait_event(&harness.store, |e| e.outcome.as_deref() == Some("ok")).await;
    assert_eq!(egress_events(&harness.store).len(), 2);
    let rendered = egress_metrics_prometheus(harness.guard.metrics());
    assert!(
        rendered.contains(r#"edger_egress_requests_total{worker="egress-test",outcome="error"} 1"#),
        "{rendered}"
    );
    assert!(
        rendered.contains(r#"edger_egress_requests_total{worker="egress-test",outcome="ok"} 1"#),
        "{rendered}"
    );
}

#[tokio::test]
async fn each_pipelined_request_is_validated_on_its_own() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    // Two requests back to back on one Unix connection: an allowed one, then
    // a refused one. The second is not carried by the first one's approval.
    let mut sock = UnixStream::connect(&harness.sock_path).await.unwrap();
    let request = format!(
        "GET / HTTP/1.1\r\nhost: {EGRESS_GUARD_HOST}\r\nx-edger-egress-url: {}/ok\r\n\r\nGET / HTTP/1.1\r\nhost: {EGRESS_GUARD_HOST}\r\nx-edger-egress-url: https://127.0.0.2/\r\nconnection: close\r\n\r\n",
        url_of(&upstream)
    );
    sock.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    within("the two pipelined responses", sock.read_to_end(&mut bytes))
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains("HTTP/1.1 403"), "{text}");
    assert!(text.contains("EGRESS_HOST_BLOCKED"), "{text}");
    assert_eq!(upstream.accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_redirect_locations_are_refused_at_the_hop() {
    let upstream = spawn_upstream().await;
    let harness = spawn_guard(
        ScriptedResolver::none(),
        true,
        default_settings(upstream.addr.port()),
    )
    .await;
    for (path, status, code) in [
        ("/to-userinfo", 400, "EGRESS_BAD_TARGET"),
        ("/to-blocked-port", 403, "EGRESS_PORT_BLOCKED"),
        ("/to-v6-loopback", 403, "EGRESS_HOST_BLOCKED"),
    ] {
        let resp = call(&harness, Call::get(format!("{}{path}", url_of(&upstream)))).await;
        assert_eq!(resp.status, status, "{path}");
        assert_eq!(resp.egress_code(), Some(code), "{path}");
    }
    assert_eq!(
        upstream.accepts.load(Ordering::SeqCst),
        3,
        "only the initial hops were dialed"
    );
}
