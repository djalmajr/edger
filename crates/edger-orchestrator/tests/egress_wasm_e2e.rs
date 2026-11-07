//! (EDG-52 07c2) The WASM egress E2E: a REAL WAT guest calling the
//! versioned import `edger_http_v1.fetch` through the SHARED factory
//! assembly (`assemble_wasm_isolate` — the same function the `edger` bin's
//! `ExecutionKind::WasmModule` branch delegates to, no manual
//! `WasmIsolate::new`/`with_http_transport`/`Bridge::new` in this file),
//! reaching the REAL `EgressGuard` (its own policy, DNS/IP/redirect/TLS/
//! size rules) and a local TLS upstream on `127.0.0.1` (a test CA and
//! server certificate generated at runtime; no external servers, no
//! fake transport).
//!
//! The module is a real WAT file in a TempDir worker dir, loaded through
//! the production loader (`load_wasm_from_worker_dir` — `.wat` compiled by
//! the isolation crate's `wat` dependency): the Config/WorkerRef carry the
//! module dir and a small-but-sufficient response limit
//! (`max_body_size: 8kb` -> 8192 bytes, `min(8192, out_cap - 14)` per call).
//! Every execution runs `assemble_wasm_isolate` + the real
//! `Isolate::execute_wasm`; the guard observes real network (TCP accepts
//! and hits), so a pre-dial refusal leaves them untouched.
//!
//! Canary discipline: every value is fake; failure messages never embed
//! request/response payload (lengths and stable codes only).

use std::collections::VecDeque;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use edger_core::egress::EgressCidr;
use edger_core::{
    parse_worker_config, AppSecretResolver, EgressManifest, ExecutionKind, Isolate,
    SerializedRequest, SerializedResponse, WorkerManifest, WorkerRef,
};
use edger_isolation::wasm::{load_wasm_from_worker_dir, WasmHttpError};
use edger_orchestrator::observability::{
    OperationalEvent, OperationalEventQuery, OperationalStore,
};
use edger_orchestrator::{
    assemble_wasm_isolate, assemble_wasm_isolate_with_database, EgressDatabaseGrant, EgressGuard,
    EgressGuardConfig, EgressWorkerContext, SystemEgressResolver,
};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::ServerConfig;
use rustls::RootCertStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

/// The watchdog of every network/guest future of this file (on top of the
/// process watchdog): one 10 s bound, no unbounded loop.
const STEP: Duration = Duration::from_secs(10);

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(STEP, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

// ---------------------------------------------------------------------------
// Guest fixture: one WAT module per scenario, same proven ABI/frame of the
// fixtures in `edger-isolation/tests/wasm_http.rs` (request frame = the
// 16-byte LE prefix of method/URL/headers-JSON/body lengths plus the
// fields; response frame = the 12-byte LE prefix of status u16, pad u16,
// header-bytes u32, body-bytes u32 plus the JSON header pairs and the
// body). On a NEGATIVE `fetch` result the guest answers a fixed 200 whose
// 2-byte body is the i16 LE code it observed; on success it copies the
// response frame through as its own answer.
// ---------------------------------------------------------------------------

const ECHO_WAT: &str = r#"
    (module
      (import "edger_http_v1" "fetch"
        (func $fetch (param i32 i32 i32 i32) (result i64)))
      (memory (export "memory") {pages})
      {data}

      (func (export "edger_alloc") (param $len i32) (result i32)
        i32.const 20000
      )

      (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
        (local $r i64)
        (local $i i32)

        i32.const {req_ptr}
        i32.const {req_len}
        i32.const {out_ptr}
        i32.const {out_cap}
        call $fetch
        local.set $r

        local.get $r
        i64.const 13
        i64.lt_s
        if (result i64)
          ;; negative: a fixed 200 answer, headers "[]", body = the 2-byte
          ;; i16 LE code the guest observed (the out region untouched)
          i32.const 40000
          i32.const 200
          i32.store16
          i32.const 40004
          i32.const 2
          i32.store
          i32.const 40008
          i32.const 2
          i32.store
          i32.const 40012
          i32.const 91
          i32.store8
          i32.const 40013
          i32.const 93
          i32.store8
          i32.const 40014
          local.get $r
          i64.const 0xFFFF
          i64.and
          i32.wrap_i64
          i32.store16
          i32.const 40000
          i64.extend_i32_u
          i64.const 16
          i64.const 32
          i64.shl
          i64.or
        else
          i32.const 0
          local.set $i
          ;; success: copy the r-byte response frame from @ {out_ptr}
          block $copy_done
            loop $copy
              local.get $i
              (i32.wrap_i64 (local.get $r))
              i32.lt_s
              if
                i32.const 40000
                local.get $i
                i32.add
                i32.const {out_ptr}
                local.get $i
                i32.add
                i32.load8_u
                i32.store8
                local.get $i
                i32.const 1
                i32.add
                local.set $i
                br $copy
              end
              br $copy_done
            end
          end
          i32.const 40000
          i64.extend_i32_u
          local.get $r
          i64.const 32
          i64.shl
          i64.or
        end
      )
    )
"#;

/// The 16-byte LE prefix + payload of a guest request frame, built from
/// the REAL byte length of every field (no hand-counted JSON/URI lengths).
fn frame_bytes(method: &[u8], uri: &[u8], headers: &[u8], body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(16 + method.len() + uri.len() + headers.len() + body.len());
    for len in [method.len(), uri.len(), headers.len(), body.len()] {
        frame.extend_from_slice(&(len as u32).to_le_bytes());
    }
    for field in [method, uri, headers, body] {
        frame.extend_from_slice(field);
    }
    frame
}

/// A `(data ...)` segment holding the frame at `addr`, every byte WAT-
/// escaped as `\XX` so hostile bytes cannot break the string literal.
fn data_segment(addr: i32, frame: &[u8]) -> String {
    let escaped: String = frame.iter().map(|b| format!("\\{:02X}", b)).collect();
    format!("(data (i32.const {addr}) \"{escaped}\")")
}

/// One scenario module: a POST to `url` with the given header pairs (their
/// REAL serde_json length, so no hand-counting) and body; memory layout:
/// frame @ 100, the fetch out region @ 4000 (4096), the edger response @
/// 40000 (4096), the edger request write @ 20000 — all inside 1 page.
fn echo_wat(url: &str, headers: &[(String, String)], body: &[u8]) -> String {
    let headers_json = serde_json::to_string(headers).expect("fixture headers always serialize");
    let frame = frame_bytes(b"POST", url.as_bytes(), headers_json.as_bytes(), body);
    ECHO_WAT
        .replace("{data}", &data_segment(100, &frame))
        .replace("{req_ptr}", "100")
        .replace("{req_len}", &frame.len().to_string())
        .replace("{out_ptr}", "4000")
        .replace("{out_cap}", "4096")
        .replace("{pages}", "1")
}

/// The 2-byte body of a negative fixture answer: the i16 LE guest code
/// (sign-extended to the stable i64 code).
fn guest_code(res: &SerializedResponse) -> i64 {
    let body = res
        .body
        .as_ref()
        .expect("the fixture always answers with a body")
        .as_ref();
    assert_eq!(
        body.len(),
        2,
        "the fixture answers with a 2-byte code (got {} bytes)",
        body.len()
    );
    i64::from(i16::from_le_bytes([body[0], body[1]]))
}

/// One response header of the execution answer (case-insensitive).
fn response_header<'a>(res: &'a SerializedResponse, name: &str) -> Option<&'a str> {
    res.headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------------------
// The test PKI (generated once per test process; no key material committed)
// ---------------------------------------------------------------------------

struct TestPki {
    ca_der: Vec<u8>,
    server_der: Vec<u8>,
    server_key_der: Vec<u8>,
}

/// A CA (CN=edger-wasm-e2e-test) that the guard trusts as its only root,
/// and the server certificate it signs (CA:FALSE, digitalSignature,
/// serverAuth; the ONLY SAN is IP 127.0.0.1).
fn test_pki() -> &'static TestPki {
    static PKI: OnceLock<TestPki> = OnceLock::new();
    PKI.get_or_init(|| {
        let mut ca_params =
            CertificateParams::new(vec!["edger-wasm-e2e-test".to_string()]).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);

        let mut server_params =
            CertificateParams::new(vec!["edger-wasm-e2e-test-server".to_string()]).unwrap();
        server_params
            .subject_alt_names
            .push(SanType::IpAddress(IpAddr::from([127, 0, 0, 1])));
        server_params.is_ca = IsCa::ExplicitNoCa;
        server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate().unwrap();
        let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();

        TestPki {
            ca_der: ca_cert.der().to_vec(),
            server_der: server_cert.der().to_vec(),
            server_key_der: server_key.serialize_der(),
        }
    })
}

// ---------------------------------------------------------------------------
// The TLS upstream (real TCP 127.0.0.1:0 + TLS + raw HTTP/1.1 parsing)
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
    /// TCP connections accepted (before any TLS): a pre-dial refusal leaves
    /// this untouched.
    accepts: Arc<AtomicUsize>,
    /// The accept-loop handle and its per-connection tasks: the test owns
    /// and aborts/awaits exactly these at the end.
    accept_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Upstream {
    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    fn hits_to(&self, path: &str) -> Vec<Hit> {
        self.hits()
            .into_iter()
            .filter(|hit| hit.target.split('?').next() == Some(path))
            .collect()
    }

    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }

    /// Abort and await ONLY the handles this test owns (the accept loop and
    /// its per-connection tasks); the listener closes with the accept loop.
    async fn shutdown(&self) {
        let accept_task = self.accept_task.lock().unwrap().take();
        if let Some(task) = accept_task {
            task.abort();
            let _ = task.await;
        }
        let conns = std::mem::take(&mut *self.conns.lock().unwrap());
        for conn in conns {
            conn.abort();
            let _ = conn.await;
        }
    }
}

fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

async fn spawn_upstream() -> Upstream {
    let listener = within("the upstream bind", TcpListener::bind("127.0.0.1:0"))
        .await
        .expect("binds the test upstream on 127.0.0.1");
    let addr = listener.local_addr().unwrap();
    let pki = test_pki();
    // Name the ring provider directly (no process-default install): the
    // same provider the guard's client config names.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(pki.server_der.clone())],
            PrivateKeyDer::try_from(pki.server_key_der.clone()).unwrap(),
        )
        .unwrap();
    let acceptor = Arc::new(TlsAcceptor::from(Arc::new(server_config)));
    let upstream = Upstream {
        addr,
        hits: Arc::new(Mutex::new(Vec::new())),
        accepts: Arc::new(AtomicUsize::new(0)),
        accept_task: Arc::new(Mutex::new(None)),
        conns: Arc::new(Mutex::new(Vec::new())),
    };
    let shared = upstream.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            shared.accepts.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            let conn_shared = shared.clone();
            let conn = tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                serve_upstream_conn(&mut tls, &conn_shared).await;
            });
            shared.conns.lock().unwrap().push(conn);
        }
    });
    *upstream.accept_task.lock().unwrap() = Some(accept_task);
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
    let mut scratch = [httparse::EMPTY_HEADER; 64];
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
    respond_upstream(tls, &path, &body, upstream.addr.port()).await;
    let _ = tls.shutdown().await;
}

/// The fixed local routes of the fixture upstream:
/// `/ok` (200, body `ok`), `/echo` (200, the received body), `/redirect`
/// (302 -> `/ok`) and `/redirect-blocked` (302 ->
/// `https://127.0.0.2:<same port>/never`, a loopback IP the positive guard
/// did NOT release).
async fn respond_upstream(tls: &mut ServerTls, path: &str, body: &[u8], port: u16) {
    match path {
        "/ok" => {
            write_cl_response(tls, 200, &[("content-type", "text/plain")], b"ok").await;
        }
        "/echo" => {
            write_cl_response(tls, 200, &[("content-type", "text/plain")], body).await;
        }
        "/redirect" => {
            write_cl_response(tls, 302, &[("location", "/ok")], b"").await;
        }
        "/redirect-blocked" => {
            let location = format!("https://127.0.0.2:{port}/never");
            write_cl_response(tls, 302, &[("location", &location)], b"").await;
        }
        _ => {
            write_cl_response(tls, 404, &[], b"nope").await;
        }
    }
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

// ---------------------------------------------------------------------------
// The guards (WASM path: no Unix socket, no `serve` task — the guard is
// consulted directly by the bridge the factory binds)
// ---------------------------------------------------------------------------

fn local_ca_roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(test_pki().ca_der.clone()))
        .unwrap();
    roots
}

/// The positive guard: the test CA as its only root and the allow CIDR
/// releasing ONLY `127.0.0.1/32` (the test upstream); every other loopback
/// IP (`127.0.0.2`, ...) and every private range stay refused.
fn guard_allow_local(store: &OperationalStore) -> Arc<EgressGuard> {
    Arc::new(
        EgressGuard::new(EgressGuardConfig {
            block_cidrs: Vec::new(),
            allow_cidrs: vec![EgressCidr::parse("127.0.0.1").unwrap()],
            resolver: Arc::new(SystemEgressResolver),
            tls_roots: local_ca_roots(),
        })
        .with_event_store(store.clone()),
    )
}

/// The STRICT guard: the default block tables and NO allow CIDR (the
/// classifier's own refusal of the loopback), the test CA as root.
fn strict_guard(store: &OperationalStore) -> Arc<EgressGuard> {
    let config = EgressGuardConfig {
        tls_roots: local_ca_roots(),
        ..Default::default()
    };
    Arc::new(EgressGuard::new(config).with_event_store(store.clone()))
}

// ---------------------------------------------------------------------------
// The worker fixtures (the module in a TempDir worker dir; the identity the
// host loader derives; the per-call grant in the manifest)
// ---------------------------------------------------------------------------

/// The fixture worker dir: the scenario WAT as `index.wat` (the production
/// loader compiles the `.wat` — no fixture changes at load time).
fn wasm_worker_dir(wat: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.wat"), wat).unwrap();
    dir
}

/// The fixture manifest WITH the explicit egress grant: `allowNet` declared
/// (empty), the destination ports, small budgets below the 8 s guest budget
/// (guard total 6 s < guest 8 s < watchdog 10 s) and the small-but-
/// sufficient 8 kb response limit.
fn grant_manifest(name: &str, ports: &[u16]) -> WorkerManifest {
    let mut manifest = WorkerManifest {
        name: name.into(),
        kind: Some("wasm".into()),
        entrypoint: Some("index.wat".into()),
        allow_net: Some(vec![]),
        egress: Some(EgressManifest {
            enabled: Some(true),
            ports: Some(ports.to_vec()),
            connect_timeout: Some("2s".into()),
            headers_timeout: Some("5s".into()),
            idle_timeout: Some("5s".into()),
            total_timeout: Some("6s".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    manifest.version = Some("1.0.0".into());
    manifest.max_body_size = Some("8kb".into());
    manifest.timeout = Some("8s".into());
    manifest
}

/// The same fixture WITHOUT the egress grant (no explicit grant = denied).
fn no_grant_manifest(name: &str) -> WorkerManifest {
    let mut manifest = WorkerManifest {
        name: name.into(),
        kind: Some("wasm".into()),
        entrypoint: Some("index.wat".into()),
        allow_net: Some(vec![]),
        ..Default::default()
    };
    manifest.version = Some("1.0.0".into());
    manifest.max_body_size = Some("8kb".into());
    manifest.timeout = Some("8s".into());
    manifest
}

/// The host-trusted `WorkerRef` the factory consumes for a fixture: the
/// identity fields the production loader derives from the manifest (name,
/// the declared version, the namespace) and the same per-call `WorkerConfig`
/// the execution always used (`parse_worker_config` + the worker dir the
/// loader sets at load time).
fn fixture_worker_ref(
    dir: &std::path::Path,
    manifest: &WorkerManifest,
    namespace: Option<&str>,
) -> WorkerRef {
    let mut config = parse_worker_config(manifest);
    config.worker_dir = Some(dir.to_path_buf());
    WorkerRef {
        id: uuid::Uuid::new_v4(),
        name: manifest.name.clone(),
        version: manifest
            .version
            .clone()
            .expect("the fixture sets a version"),
        dir: dir.to_path_buf(),
        namespace: namespace.map(str::to_owned),
        kind: config.kind.clone().unwrap_or(ExecutionKind::FetchHandler),
        config,
    }
}

/// The host request of one execution: the `request_id` is the HOST
/// identity the guard's events must carry (the guest frame carries none).
fn host_request(id: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: id.into(),
        base_href: None,
    }
}

/// The `worker.egress` events of the store (newest first).
fn egress_events(store: &OperationalStore) -> Vec<OperationalEvent> {
    let query = OperationalEventQuery {
        kind: Some("worker.egress".into()),
        limit: Some(500),
        ..Default::default()
    };
    store.query(query).events
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// (EDG-52 07c2) Positive: a real WAT module POSTs `/echo?fixture=query`
/// with a body and hostile headers (a fake Authorization, the guard's
/// reserved `x-edger-egress-*` headers forged by the guest, and
/// hop-by-hop/`Connection` tokens); the upstream observes the real
/// method/target/query/body with the Host and Content-Length REBUILT and
/// every guest reserved/hop-by-hop header REMOVED (the upstream
/// `Connection` is the guard's own `close`); the guest passes the real
/// status/headers/body through; the event carries the `WorkerRef`
/// identity (name/version/namespace), the HOST request id and
/// `process_id = None`. The SAME isolate is re-executed with a NEW host
/// request id to prove the per-execution context is fresh.
#[tokio::test]
async fn positive_echo_preserves_the_request_and_the_host_identity_in_the_event() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let store = OperationalStore::default();
    let guard = guard_allow_local(&store);

    let url = format!("https://127.0.0.1:{port}/echo?fixture=query");
    let headers = vec![
        (
            "authorization".to_string(),
            "Bearer fake-token-canary".to_string(),
        ),
        ("content-type".to_string(), "application/json".to_string()),
        (
            "x-edger-egress-url".to_string(),
            "https://forged.example/evil".to_string(),
        ),
        (
            "x-edger-egress-error".to_string(),
            "EGRESS_HOST_BLOCKED".to_string(),
        ),
        ("connection".to_string(), "close, x-priv".to_string()),
        ("x-priv".to_string(), "1".to_string()),
        ("keep-alive".to_string(), "timeout=5".to_string()),
        ("proxy-connection".to_string(), "keep-alive".to_string()),
    ];
    let wat = echo_wat(&url, &headers, b"body-canary");
    let dir = wasm_worker_dir(&wat);
    let manifest = grant_manifest("wasm-e2e-worker", &[443, port]);
    let worker_ref = fixture_worker_ref(dir.path(), &manifest, Some("nsA"));
    let mut isolate = assemble_wasm_isolate(&worker_ref, None, Some(Arc::clone(&guard)));

    // Execution 1 — the host request id.
    let res = within(
        "the first execution",
        isolate.execute_wasm(host_request("wasm-e2e-req-1"), &worker_ref.config),
    )
    .await
    .expect("a granted execution must succeed");
    // The ABI preserves the real upstream answer end to end.
    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"body-canary".as_ref())
    );
    assert_eq!(response_header(&res, "content-type"), Some("text/plain"));
    assert!(response_header(&res, "connection").is_none());
    assert!(response_header(&res, "x-edger-egress-error").is_none());

    // The upstream observed the real request (rebuilt authority/CL, the
    // guest's reserved and hop-by-hop headers removed).
    let hits = upstream.hits_to("/echo");
    assert_eq!(hits.len(), 1, "exactly one /echo hit");
    let hit = &hits[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.target, "/echo?fixture=query");
    assert_eq!(
        hit.host_header.as_deref(),
        Some(format!("127.0.0.1:{port}").as_str()),
        "the Host is rebuilt from the destination authority"
    );
    assert_eq!(hit.body, b"body-canary");
    assert_eq!(
        hit.header("authorization"),
        Some("Bearer fake-token-canary")
    );
    assert_eq!(hit.header("content-type"), Some("application/json"));
    assert_eq!(
        hit.header("content-length"),
        Some("11"),
        "the Content-Length is rebuilt from the buffered body"
    );
    // The guest's own `Connection: close, x-priv` is never forwarded: the
    // guard strips hop-by-hop headers (and the header its value names,
    // RFC 7230 section 6.1) and names the upstream connection itself
    // (`Connection: close`, one request per upstream connection in
    // `build_upstream_request`).
    assert_eq!(
        hit.header("connection"),
        Some("close"),
        "the guard sets its own Connection on the upstream connection"
    );
    for gone in [
        "x-edger-egress-url",
        "x-edger-egress-error",
        "x-priv",
        "keep-alive",
        "proxy-connection",
    ] {
        assert!(
            hit.header(gone).is_none(),
            "{gone} must not reach the upstream"
        );
    }

    // The event carries the WorkerRef identity, the HOST request id and
    // `process_id = None` (the WASM backend has one isolate, not processes).
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("wasm-e2e-req-1"))
        .expect("one event for the host request id");
    assert_eq!(event.worker.as_deref(), Some("wasm-e2e-worker"));
    assert_eq!(event.version.as_deref(), Some("1.0.0"));
    assert_eq!(event.namespace.as_deref(), Some("nsA"));
    assert_eq!(event.process_id, None);
    assert_eq!(event.outcome.as_deref(), Some("ok"));
    assert_eq!(event.status, Some(200));
    assert_eq!(event.code, None);

    // Execution 2 — the SAME isolate with a NEW host request id: the
    // per-execution context (not a cached one) names it in its own event.
    let res = within(
        "the second execution",
        isolate.execute_wasm(host_request("wasm-e2e-req-2"), &worker_ref.config),
    )
    .await
    .expect("the second execution must succeed");
    assert_eq!(res.status, 200);
    let second = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("wasm-e2e-req-2"))
        .expect("the second execution records its own event");
    assert_eq!(second.worker.as_deref(), Some("wasm-e2e-worker"));
    assert_eq!(second.outcome.as_deref(), Some("ok"));
    assert_eq!(
        upstream.hits_to("/echo").len(),
        2,
        "each execution produced exactly one upstream hit"
    );

    upstream.shutdown().await;
}

/// (EDG-52 07c2) Default-deny: (a) the guard present but NO egress grant in
/// the worker config, and (b) the grant present but NO guard — the import
/// fails closed with `WASM_IMPORT_DENIED` before any network: zero events,
/// zero hits, zero dials in both cases.
#[tokio::test]
async fn import_fails_closed_without_the_grant_or_without_the_guard() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let url = format!("https://127.0.0.1:{port}/echo");
    let wat = echo_wat(&url, &[], b"");
    let dir = wasm_worker_dir(&wat);

    // (a) Guard present, no grant: the assembly binds no transport and the
    // import is denied before the guest runs.
    let store = OperationalStore::default();
    let guard = guard_allow_local(&store);
    let manifest = no_grant_manifest("wasm-e2e-nogrant");
    let worker_ref = fixture_worker_ref(dir.path(), &manifest, None);
    let mut isolate = assemble_wasm_isolate(&worker_ref, None, Some(Arc::clone(&guard)));
    let err = within(
        "the ungranted execution",
        isolate.execute_wasm(host_request("wasm-e2e-nogrant"), &worker_ref.config),
    )
    .await
    .expect_err("without the grant the import must be denied");
    assert_eq!(err.code, "WASM_IMPORT_DENIED");
    assert!(
        egress_events(&store).is_empty(),
        "a denied import records no egress event"
    );
    assert!(upstream.hits().is_empty());
    assert_eq!(upstream.accepts(), 0, "no dial without the grant");

    // (b) Grant present, no guard: the same default-deny (the generic warn
    // fires; no binding, so the import is denied too).
    let manifest = grant_manifest("wasm-e2e-noguard", &[443, port]);
    let worker_ref = fixture_worker_ref(dir.path(), &manifest, None);
    let mut isolate = assemble_wasm_isolate(&worker_ref, None, None);
    let err = within(
        "the guardless execution",
        isolate.execute_wasm(host_request("wasm-e2e-noguard"), &worker_ref.config),
    )
    .await
    .expect_err("without the guard the import must be denied");
    assert_eq!(err.code, "WASM_IMPORT_DENIED");
    assert!(upstream.hits().is_empty());
    assert_eq!(upstream.accepts(), 0, "no dial without the guard");

    upstream.shutdown().await;
}

/// (EDG-52 07c2) The STRICT guard (default block tables, NO allow CIDR)
/// denies the loopback by its own policy BEFORE the dial: the import is
/// still admitted (grant + guard bound — the guard refusal is NOT an import
/// failure), the guest observes the stable transport code as its fixed
/// body, the event is `blocked`/`EGRESS_HOST_BLOCKED` with the HOST
/// request id, and the upstream saw no accept and no hit.
#[tokio::test]
async fn strict_guard_denies_loopback_as_a_transport_error_before_any_dial() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let url = format!("https://127.0.0.1:{port}/ok");
    let wat = echo_wat(&url, &[], b"");
    let dir = wasm_worker_dir(&wat);
    let store = OperationalStore::default();
    let guard = strict_guard(&store);
    let manifest = grant_manifest("wasm-e2e-strict", &[443, port]);
    let worker_ref = fixture_worker_ref(dir.path(), &manifest, Some("nsStrict"));
    let mut isolate = assemble_wasm_isolate(&worker_ref, None, Some(Arc::clone(&guard)));

    let res = within(
        "the strict execution",
        isolate.execute_wasm(host_request("wasm-e2e-strict"), &worker_ref.config),
    )
    .await
    .expect(
        "the admitted import reaches the guard; the refusal is a \
         transport error for the guest, not a host error",
    );
    assert_eq!(
        res.status, 200,
        "the fixture answers 200 with the code body"
    );
    assert_eq!(
        guest_code(&res),
        WasmHttpError::TRANSPORT,
        "the guest sees the stable transport class, not a payload"
    );

    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("wasm-e2e-strict"))
        .expect("the guard records the refusal");
    assert_eq!(event.outcome.as_deref(), Some("blocked"));
    assert_eq!(event.code.as_deref(), Some("EGRESS_HOST_BLOCKED"));
    assert_eq!(event.status, Some(403));
    assert_eq!(event.worker.as_deref(), Some("wasm-e2e-strict"));
    assert_eq!(event.version.as_deref(), Some("1.0.0"));
    assert_eq!(event.namespace.as_deref(), Some("nsStrict"));
    assert_eq!(event.process_id, None);

    assert!(
        upstream.hits().is_empty(),
        "the loopback refusal happens before the dial"
    );
    assert_eq!(upstream.accepts(), 0, "no TCP dial for the refused host");

    upstream.shutdown().await;
}

/// (EDG-52 07c2) Redirects on the positive guard: `/redirect` (302 ->
/// `/ok`) is followed — the guest receives the FINAL upstream answer (200
/// `ok`) and the event counts the one hop; `/redirect-blocked` (302 ->
/// `https://127.0.0.2:<port>/never`) is refused by the guard (only
/// `127.0.0.1/32` is released) BEFORE the second dial/hit: the guest
/// receives the static transport code (never a partial or normal upstream
/// answer) and ONE event carries the policy code and the coherent hop
/// count.
#[tokio::test]
async fn redirect_is_followed_and_the_blocked_redirect_is_refused_before_the_second_hop() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    let base = format!("https://127.0.0.1:{port}");
    let store = OperationalStore::default();
    let guard = guard_allow_local(&store);

    // `/redirect` -> 302 `/ok`: followed (same origin; the 302 after a POST
    // becomes a bodyless GET); the guest sees the final answer.
    let wat = echo_wat(&format!("{base}/redirect"), &[], b"");
    let dir = wasm_worker_dir(&wat);
    let manifest = grant_manifest("wasm-e2e-redirect", &[443, port]);
    let worker_ref = fixture_worker_ref(dir.path(), &manifest, None);
    let mut isolate = assemble_wasm_isolate(&worker_ref, None, Some(Arc::clone(&guard)));
    let res = within(
        "the redirect execution",
        isolate.execute_wasm(host_request("wasm-e2e-redirect-ok"), &worker_ref.config),
    )
    .await
    .expect("the followed redirect must succeed");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.as_ref().map(|b| b.as_ref()), Some(b"ok".as_ref()));
    let ok_hits = upstream.hits_to("/ok");
    assert_eq!(ok_hits.len(), 1);
    assert_eq!(
        ok_hits[0].method, "GET",
        "a 302 after a POST becomes a bodyless GET"
    );
    assert_eq!(upstream.hits_to("/redirect").len(), 1);
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("wasm-e2e-redirect-ok"))
        .expect("one event for the redirect call");
    assert_eq!(event.outcome.as_deref(), Some("ok"));
    assert_eq!(event.status, Some(200));
    assert_eq!(event.code, None);
    assert!(
        event
            .message
            .as_deref()
            .unwrap_or("")
            .contains("redirects=1"),
        "the event counts the one followed hop"
    );

    // `/redirect-blocked` -> 302 `https://127.0.0.2:<port>/never`: the
    // second hop is refused BEFORE the dial (only 127.0.0.1/32 released).
    let wat = echo_wat(&format!("{base}/redirect-blocked"), &[], b"");
    let dir = wasm_worker_dir(&wat);
    let manifest = grant_manifest("wasm-e2e-redirect", &[443, port]);
    let worker_ref = fixture_worker_ref(dir.path(), &manifest, None);
    let mut isolate = assemble_wasm_isolate(&worker_ref, None, Some(Arc::clone(&guard)));
    let res = within(
        "the blocked-redirect execution",
        isolate.execute_wasm(
            host_request("wasm-e2e-redirect-blocked"),
            &worker_ref.config,
        ),
    )
    .await
    .expect(
        "the refused second hop is a transport error for the guest, \
         not a host error",
    );
    assert_eq!(
        res.status, 200,
        "the fixture answers 200 with the code body"
    );
    assert_eq!(guest_code(&res), WasmHttpError::TRANSPORT);
    assert_eq!(
        upstream.hits_to("/redirect-blocked").len(),
        1,
        "only the first hop reached the upstream"
    );
    assert_eq!(
        upstream.hits_to("/never").len(),
        0,
        "the refused hop never dialed (no /never hit)"
    );
    assert_eq!(
        upstream.hits().len(),
        3,
        "/redirect, /ok and /redirect-blocked — the second hop of the blocked call never dialed"
    );
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("wasm-e2e-redirect-blocked"))
        .expect("one event for the blocked call");
    assert_eq!(event.outcome.as_deref(), Some("blocked"));
    assert_eq!(event.code.as_deref(), Some("EGRESS_HOST_BLOCKED"));
    assert_eq!(event.status, Some(403));
    assert!(
        event
            .message
            .as_deref()
            .unwrap_or("")
            .contains("redirects=1"),
        "the refusal event carries the coherent hop count"
    );

    upstream.shutdown().await;
}

/// (EDG-52 07c2) Isolation: the SAME `Arc<EgressGuard>` and the SAME WAT
/// module bytes serve two workers — A (`nsA`) with the fixture port
/// granted, B (`nsB`) with `egress` granted but the fixture port ABSENT
/// (only 443). The module's request even carries headers naming A. A
/// works; B is refused `EGRESS_PORT_BLOCKED` with B's own
/// identity/namespace/settings (the guest headers naming A swap nothing),
/// and B's refusal happens before any extra dial/hit: one app does not
/// inherit grant/policy by sharing the guard or the module bytes.
#[tokio::test]
async fn sharing_the_guard_and_the_module_bytes_does_not_share_identity_or_grant() {
    let upstream = spawn_upstream().await;
    let port = upstream.addr.port();
    assert_ne!(port, 443, "worker B's grant must exclude the fixture port");
    let base = format!("https://127.0.0.1:{port}");
    let store = OperationalStore::default();
    let guard = guard_allow_local(&store);

    // ONE WAT module (the same bytes for both workers): its request carries
    // a header naming worker A — the guest tries to impersonate A.
    let headers = vec![
        (
            "authorization".to_string(),
            "Bearer fake-token-canary".to_string(),
        ),
        ("x-worker".to_string(), "wasm-e2e-a".to_string()),
    ];
    let wat = echo_wat(
        &format!("{base}/echo?fixture=query"),
        &headers,
        b"body-canary",
    );
    let dir = wasm_worker_dir(&wat);

    // A (nsA): the fixture port is granted.
    let manifest_a = grant_manifest("wasm-e2e-a", &[443, port]);
    let worker_a = fixture_worker_ref(dir.path(), &manifest_a, Some("nsA"));
    let mut isolate_a = assemble_wasm_isolate(&worker_a, None, Some(Arc::clone(&guard)));
    let res_a = within(
        "worker A's execution",
        isolate_a.execute_wasm(host_request("wasm-e2e-a"), &worker_a.config),
    )
    .await
    .expect("A's grant covers the fixture port");
    assert_eq!(res_a.status, 200);
    assert_eq!(
        res_a.body.as_ref().map(|b| b.as_ref()),
        Some(b"body-canary".as_ref())
    );

    // B (nsB): the SAME module and the SAME guard, but the fixture port is
    // ABSENT from B's grant (only 443): refused EGRESS_PORT_BLOCKED before
    // any dial, with B's own identity.
    let manifest_b = grant_manifest("wasm-e2e-b", &[443]);
    let worker_b = fixture_worker_ref(dir.path(), &manifest_b, Some("nsB"));
    let mut isolate_b = assemble_wasm_isolate(&worker_b, None, Some(Arc::clone(&guard)));
    let res_b = within(
        "worker B's execution",
        isolate_b.execute_wasm(host_request("wasm-e2e-b"), &worker_b.config),
    )
    .await
    .expect(
        "B's refusal is a transport error for the guest, \
         not a host error",
    );
    assert_eq!(
        res_b.status, 200,
        "the fixture answers 200 with the code body"
    );
    assert_eq!(guest_code(&res_b), WasmHttpError::TRANSPORT);

    let events = egress_events(&store);
    let event_a = events
        .iter()
        .find(|e| e.request_id.as_deref() == Some("wasm-e2e-a"))
        .expect("A's event");
    assert_eq!(event_a.worker.as_deref(), Some("wasm-e2e-a"));
    assert_eq!(event_a.namespace.as_deref(), Some("nsA"));
    assert_eq!(event_a.outcome.as_deref(), Some("ok"));
    let event_b = events
        .iter()
        .find(|e| e.request_id.as_deref() == Some("wasm-e2e-b"))
        .expect("B's event");
    assert_eq!(
        event_b.worker.as_deref(),
        Some("wasm-e2e-b"),
        "the guest headers naming A do not swap B's identity"
    );
    assert_eq!(event_b.namespace.as_deref(), Some("nsB"));
    assert_eq!(event_b.outcome.as_deref(), Some("blocked"));
    assert_eq!(event_b.code.as_deref(), Some("EGRESS_PORT_BLOCKED"));
    assert_eq!(event_b.status, Some(403));
    assert_eq!(event_b.process_id, None);

    // Exactly A's single hit: B never dialed (no extra hit, no extra
    // accept beyond A's one connection).
    assert_eq!(
        upstream.hits().len(),
        1,
        "B's refusal happens before any dial"
    );
    assert_eq!(upstream.accepts(), 1);
    assert_eq!(
        upstream.hits()[0].header("x-worker"),
        Some("wasm-e2e-a"),
        "A's own header is forwarded as an app header — it is not identity"
    );

    upstream.shutdown().await;
}

// ---------------------------------------------------------------------------
// (EDG-52/54 07d3b) The private database authority fixtures: the plain
// HTTP/1.1 TCP database stand-in (target/method/body/auth/namespace
// observation, hit and accept counting) and the canary vault (a counting
// resolver — only to check owner/name/count/rotation; 09 proves the real
// Vault/sqld composition).
// ---------------------------------------------------------------------------

/// One request as the database fixture saw it (the observation surface:
/// method, target, the imposed auth/namespace and the body).
#[derive(Clone, Debug)]
struct DbHit {
    method: String,
    target: String,
    authorization: Option<String>,
    namespace_bin: Option<String>,
    body: Vec<u8>,
}

/// (EDG-52/54 07d3b) The small local plain-HTTP TCP database stand-in
/// (the SQLD service — no TLS, no sqld installed): real TCP on
/// `127.0.0.1:0`, the raw HTTP/1.1 head/body parse (the SAME pattern as
/// the TLS upstream of this file), counting every TCP accept (a pre-dial
/// refusal leaves it untouched) and recording every hit (method, target,
/// the `authorization` and `x-namespace-bin` headers as received and the
/// body). Fixed routes: `/query` (200 `db-ok`), `/echo` (200, the
/// received body), `/redir` (302 -> `/elsewhere`) and the default 404 —
/// a service redirect the DB entry must refuse, never follow.
#[derive(Clone)]
struct DatabaseHttpServer {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<DbHit>>>,
    accepts: Arc<AtomicUsize>,
    accept_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl DatabaseHttpServer {
    fn hits(&self) -> Vec<DbHit> {
        self.hits.lock().unwrap().clone()
    }

    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }

    /// Abort and await ONLY the handles this test owns (the accept loop
    /// and its per-connection tasks); the listener closes with the
    /// accept loop.
    async fn shutdown(&self) {
        let accept_task = self.accept_task.lock().unwrap().take();
        if let Some(task) = accept_task {
            task.abort();
            let _ = task.await;
        }
        let conns = std::mem::take(&mut *self.conns.lock().unwrap());
        for conn in conns {
            conn.abort();
            let _ = conn.await;
        }
    }
}

async fn spawn_database_server() -> DatabaseHttpServer {
    let listener = within("the database bind", TcpListener::bind("127.0.0.1:0"))
        .await
        .expect("binds the plain-HTTP database fixture on 127.0.0.1");
    let addr = listener.local_addr().unwrap();
    let server = DatabaseHttpServer {
        addr,
        hits: Arc::new(Mutex::new(Vec::new())),
        accepts: Arc::new(AtomicUsize::new(0)),
        accept_task: Arc::new(Mutex::new(None)),
        conns: Arc::new(Mutex::new(Vec::new())),
    };
    let shared = server.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                break;
            };
            shared.accepts.fetch_add(1, Ordering::SeqCst);
            let conn_shared = shared.clone();
            let conn = tokio::spawn(async move {
                serve_db_conn(&mut tcp, &conn_shared).await;
            });
            shared.conns.lock().unwrap().push(conn);
        }
    });
    *server.accept_task.lock().unwrap() = Some(accept_task);
    server
}

/// One plain HTTP/1.1 exchange of the database fixture: read the head,
/// parse it, read the body by its `Content-Length`, record the hit and
/// answer the fixed route.
async fn serve_db_conn(tcp: &mut TcpStream, server: &DatabaseHttpServer) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let end = loop {
        if let Some(end) = head_end(&buf) {
            break end;
        }
        match tcp.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let mut scratch = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut scratch);
    let offset = match req.parse(&buf[..end]) {
        Ok(httparse::Status::Complete(offset)) => offset,
        _ => return,
    };
    let get_header = |name: &str| {
        req.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| String::from_utf8_lossy(h.value).into_owned())
    };
    let content_length = get_header("content-length")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    // Every `req` use BEFORE the mutable `buf` borrow below (`req` holds
    // slices into `buf`, so the head fields and the recorded headers are
    // copied to owned Strings first).
    let method = req.method.unwrap_or("?").to_string();
    let target = req.path.unwrap_or("/").to_string();
    let authorization = get_header("authorization");
    let namespace_bin = get_header("x-namespace-bin");
    let mut body = buf.split_off(offset);
    while body.len() < content_length {
        match tcp.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(content_length);
    server.hits.lock().unwrap().push(DbHit {
        method,
        target: target.clone(),
        authorization,
        namespace_bin,
        body: body.clone(),
    });
    // The path decides the route; the query string is never part of it.
    let path = target.split('?').next().unwrap_or("/").to_string();
    respond_db(tcp, &path, &body).await;
    let _ = tcp.shutdown().await;
}

/// The fixed local routes of the database fixture.
async fn respond_db(tcp: &mut TcpStream, path: &str, body: &[u8]) {
    match path {
        "/query" => {
            write_db_response(tcp, 200, &[("content-type", "text/plain")], b"db-ok").await;
        }
        "/echo" => {
            write_db_response(tcp, 200, &[("content-type", "text/plain")], body).await;
        }
        "/redir" => {
            write_db_response(tcp, 302, &[("location", "/elsewhere")], b"").await;
        }
        "/elsewhere" => {
            write_db_response(tcp, 200, &[], b"elsewhere").await;
        }
        _ => {
            write_db_response(tcp, 404, &[], b"nope").await;
        }
    }
}

async fn write_db_response(
    tcp: &mut TcpStream,
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
    let _ = tcp.write_all(&out).await;
}

/// (EDG-52/54 07d3b) The canary vault: a counting `AppSecretResolver`
/// that records every lookup (the count and the exact `(worker, name)`
/// arguments) and answers the queued fake values in order (a `None` once
/// exhausted — fail closed). Only to check owner/name/count/rotation:
/// 09 proves the composition with the real Vault/sqld.
struct CanaryResolver {
    calls: Arc<AtomicUsize>,
    args: Arc<Mutex<Vec<(String, String)>>>,
    values: Arc<Mutex<VecDeque<String>>>,
}

#[async_trait]
impl AppSecretResolver for CanaryResolver {
    async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.args
            .lock()
            .unwrap()
            .push((worker.to_owned(), name.to_owned()));
        self.values.lock().unwrap().get(n).cloned()
    }
}

struct CanaryVault {
    calls: Arc<AtomicUsize>,
    args: Arc<Mutex<Vec<(String, String)>>>,
    values: Arc<Mutex<VecDeque<String>>>,
}

impl CanaryVault {
    fn new(values: &[&str]) -> Self {
        let calls = Arc::new(AtomicUsize::new(0));
        let args = Arc::new(Mutex::new(Vec::new()));
        let values = Arc::new(Mutex::new(
            values
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<VecDeque<_>>(),
        ));
        Self {
            calls,
            args,
            values,
        }
    }

    fn resolver(&self) -> Arc<dyn AppSecretResolver> {
        Arc::new(CanaryResolver {
            calls: Arc::clone(&self.calls),
            args: Arc::clone(&self.args),
            values: Arc::clone(&self.values),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn args(&self) -> Vec<(String, String)> {
        self.args.lock().unwrap().clone()
    }
}

/// The fixture database grant: the owner from the `WorkerRef` identity
/// (worker/namespace — the version and process id do not participate in
/// the owner match), the plain-HTTP origin on the fixture port, the SQLD
/// namespace and the canary vault under the credential name.
fn fixture_db_grant(
    worker_ref: &WorkerRef,
    origin: &str,
    vault: &CanaryVault,
) -> Arc<EgressDatabaseGrant> {
    let owner = EgressWorkerContext {
        worker: worker_ref.name.clone(),
        version: worker_ref.version.clone(),
        namespace: worker_ref.namespace.clone(),
        process_id: String::new(),
    };
    Arc::new(
        EgressDatabaseGrant::new(
            &owner,
            origin,
            "sqlns",
            "SQLD_TOKEN",
            Some(vault.resolver()),
        )
        .expect("valid grant shape"),
    )
}

/// The grant's SQLD namespace as `x-namespace-bin` (base64
/// STANDARD_NO_PAD — the pinned SQLD `try_namespace_from_metadata`
/// codec).
fn sqlns_header() -> String {
    use base64::engine::Engine;
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(b"sqlns")
}

/// (EDG-52/54 07d3b) DB-only positive (the tight proof): the per-call
/// `config.egress` is NONE and the guard is the STRICT one (default
/// block tables, NO allow CIDR — the general egress would refuse the
/// loopback). The real WAT guest reaches the plain-HTTP database through
/// the new assembly's private service binding ONLY: the host imposes the
/// grant's own `authorization` and `x-namespace-bin` (the guest's forged
/// ones never reach the fixture), the path/query/body are preserved, the
/// credential is resolved fresh per execution (rotation), and nothing in
/// the per-call config, the `WasiConfig` or the env maps gains a value.
#[tokio::test]
async fn db_only_grant_reaches_the_service_under_the_strict_guard() {
    let db = spawn_database_server().await;
    let db_port = db.addr.port();
    let store = OperationalStore::default();
    let guard = strict_guard(&store);

    // The guest forges the credential and the SQLD namespace; the host
    // must impose the grant's own.
    let url = format!("http://127.0.0.1:{db_port}/query?fixture=db");
    let headers = vec![
        (
            "authorization".to_string(),
            "Bearer forged-canary".to_string(),
        ),
        ("x-namespace-bin".to_string(), "Zm9yZ2Vk".to_string()),
        ("content-type".to_string(), "application/sql".to_string()),
    ];
    let wat = echo_wat(&url, &headers, b"db-body-canary");
    let dir = wasm_worker_dir(&wat);
    // cfg.egress NONE: no general grant at all (DB-only).
    let manifest = no_grant_manifest("wasm-db-only");
    let worker_ref = fixture_worker_ref(dir.path(), &manifest, Some("nsDb"));
    let vault = CanaryVault::new(&["canary-token-1", "canary-token-2"]);
    let grant = fixture_db_grant(&worker_ref, &format!("http://127.0.0.1:{db_port}/"), &vault);

    let mut isolate = assemble_wasm_isolate_with_database(
        &worker_ref,
        None,
        Some(Arc::clone(&guard)),
        Some(Arc::clone(&grant)),
    );

    let res = within(
        "the db-only execution",
        isolate.execute_wasm(host_request("db-req-1"), &worker_ref.config),
    )
    .await
    .expect("the private grant + the guard must admit the import");
    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"db-ok".as_ref())
    );
    assert_eq!(response_header(&res, "content-type"), Some("text/plain"));

    // The fixture observed the real request with the HOST-imposed auth
    // and namespace (the guest's forged ones removed).
    let hits = db.hits();
    assert_eq!(hits.len(), 1, "exactly one database hit");
    let hit = &hits[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.target, "/query?fixture=db");
    assert_eq!(hit.body, b"db-body-canary");
    assert_eq!(
        hit.authorization.as_deref(),
        Some("Bearer canary-token-1"),
        "the grant's own credential, resolved fresh"
    );
    assert_eq!(
        hit.namespace_bin.as_deref(),
        Some(sqlns_header().as_str()),
        "the grant's own SQLD namespace, not the guest's"
    );

    // The credential lookup: exactly one, owner-scoped.
    assert_eq!(vault.calls(), 1);
    assert_eq!(
        vault.args(),
        vec![("wasm-db-only".to_owned(), "SQLD_TOKEN".to_owned())]
    );

    // No authority or value leaked: the config stayed DB-only (egress
    // NONE), the WasiConfig and the env maps carry no token.
    assert!(
        worker_ref.config.egress.is_none(),
        "the config stays egress-None"
    );
    assert!(!format!("{:?}", isolate.wasi_config()).contains("canary-token-1"));
    assert!(worker_ref
        .config
        .env
        .values()
        .all(|v| !v.contains("canary-token-1")));
    assert!(worker_ref
        .config
        .public_env
        .iter()
        .all(|v| !v.contains("canary-token-1")));

    // The event carries the WorkerRef identity and the HOST request id.
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("db-req-1"))
        .expect("one event for the host request id");
    assert_eq!(event.worker.as_deref(), Some("wasm-db-only"));
    assert_eq!(event.version.as_deref(), Some("1.0.0"));
    assert_eq!(event.namespace.as_deref(), Some("nsDb"));
    assert_eq!(event.process_id, None);
    assert_eq!(event.outcome.as_deref(), Some("ok"));
    assert_eq!(event.status, Some(200));

    // Rotation: the SAME isolate, a new execution — a fresh credential.
    let res = within(
        "the second db-only execution",
        isolate.execute_wasm(host_request("db-req-2"), &worker_ref.config),
    )
    .await
    .expect("the second execution must succeed");
    assert_eq!(res.status, 200);
    let hits = db.hits();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[1].authorization.as_deref(),
        Some("Bearer canary-token-2"),
        "the credential is renewed per execution"
    );
    assert_eq!(vault.calls(), 2);

    db.shutdown().await;
}

/// (EDG-52/54 07d3b) Default-deny table of the private authority:
/// (a) NO database grant (and no general one): the import is denied
/// before any lookup/hit/dial — the base assembly, no service binding;
/// (b) the grant present but NO guard: the same denial, the static warn,
/// no binding; (c) the WRONG owner (cross app, and cross namespace with
/// the SAME origin while the guest forges the other namespace): refused
/// by the DB bridge BEFORE the credential lookup or any dial; (d) a
/// DIFFERENT origin (DB-only): refused statically, no network. In
/// (c)/(d) the import IS admitted (the service binding exists) — the
/// refusal is the stable transport code, the event is `blocked`/
/// `EGRESS_DATABASE_DENIED`, and the canary vault and the fixture stay
/// untouched.
#[tokio::test]
async fn db_grant_fails_closed_without_the_guard_the_owner_or_the_origin() {
    let db = spawn_database_server().await;
    let db_port = db.addr.port();
    let store = OperationalStore::default();
    let guard = strict_guard(&store);

    let url = format!("http://127.0.0.1:{db_port}/query");
    let wat = echo_wat(&url, &[], b"");
    let dir = wasm_worker_dir(&wat);
    let manifest = no_grant_manifest("wasm-db-deny");
    let worker_ref = fixture_worker_ref(dir.path(), &manifest, Some("nsDb"));
    let vault = CanaryVault::new(&["canary-token-1"]);

    // (a) No database grant: the base assembly (no service binding) and
    // the import is denied before the guest runs.
    let mut isolate =
        assemble_wasm_isolate_with_database(&worker_ref, None, Some(Arc::clone(&guard)), None);
    let err = within(
        "the grantless execution",
        isolate.execute_wasm(host_request("db-deny-a"), &worker_ref.config),
    )
    .await
    .expect_err("without the database grant the import must be denied");
    assert_eq!(err.code, "WASM_IMPORT_DENIED");
    assert!(egress_events(&store).is_empty());
    assert!(db.hits().is_empty());
    assert_eq!(db.accepts(), 0, "no dial without the grant");
    assert_eq!(vault.calls(), 0);

    // (b) The grant present but NO guard: no service binding, the same
    // default-deny (and no lookup).
    let grant = fixture_db_grant(&worker_ref, &format!("http://127.0.0.1:{db_port}/"), &vault);
    let mut isolate =
        assemble_wasm_isolate_with_database(&worker_ref, None, None, Some(Arc::clone(&grant)));
    let err = within(
        "the guardless execution",
        isolate.execute_wasm(host_request("db-deny-b"), &worker_ref.config),
    )
    .await
    .expect_err("without the guard the import must be denied");
    assert_eq!(err.code, "WASM_IMPORT_DENIED");
    assert!(db.hits().is_empty());
    assert_eq!(db.accepts(), 0, "no dial without the guard");
    assert_eq!(vault.calls(), 0);

    // (c1) The WRONG owner — cross app, the SAME origin: refused before
    // the credential lookup or any dial.
    let other_owner = EgressWorkerContext {
        worker: "other-app".into(),
        version: "1.0.0".into(),
        namespace: Some("nsDb".into()),
        process_id: String::new(),
    };
    let foreign_grant = Arc::new(
        EgressDatabaseGrant::new(
            &other_owner,
            &format!("http://127.0.0.1:{db_port}/"),
            "sqlns",
            "SQLD_TOKEN",
            Some(vault.resolver()),
        )
        .expect("valid grant shape"),
    );
    let mut isolate = assemble_wasm_isolate_with_database(
        &worker_ref,
        None,
        Some(Arc::clone(&guard)),
        Some(foreign_grant),
    );
    let res = within(
        "the cross-app execution",
        isolate.execute_wasm(host_request("db-deny-c1"), &worker_ref.config),
    )
    .await
    .expect("the import is admitted; the refusal is a transport error");
    assert_eq!(guest_code(&res), WasmHttpError::TRANSPORT);
    assert!(db.hits().is_empty(), "no database hit for a foreign owner");
    assert_eq!(db.accepts(), 0, "no dial for a foreign owner");
    assert_eq!(
        vault.calls(),
        0,
        "the refusal is before the credential lookup"
    );
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("db-deny-c1"))
        .expect("the refusal is recorded");
    assert_eq!(event.outcome.as_deref(), Some("blocked"));
    assert_eq!(event.code.as_deref(), Some("EGRESS_DATABASE_DENIED"));
    assert_eq!(event.status, Some(403));
    assert_eq!(event.worker.as_deref(), Some("wasm-db-deny"));

    // (c2) The WRONG namespace — the SAME app and origin, the grant's
    // owner in `nsOther` while the guest forges that namespace: the
    // owner comes from the WorkerRef, never from the guest frame.
    let ns_owner = EgressWorkerContext {
        worker: "wasm-db-deny".into(),
        version: "1.0.0".into(),
        namespace: Some("nsOther".into()),
        process_id: String::new(),
    };
    let ns_grant = Arc::new(
        EgressDatabaseGrant::new(
            &ns_owner,
            &format!("http://127.0.0.1:{db_port}/"),
            "sqlns",
            "SQLD_TOKEN",
            Some(vault.resolver()),
        )
        .expect("valid grant shape"),
    );
    let mut isolate = assemble_wasm_isolate_with_database(
        &worker_ref,
        None,
        Some(Arc::clone(&guard)),
        Some(ns_grant),
    );
    let res = within(
        "the cross-namespace execution",
        isolate.execute_wasm(host_request("db-deny-c2"), &worker_ref.config),
    )
    .await
    .expect("the import is admitted; the refusal is a transport error");
    assert_eq!(guest_code(&res), WasmHttpError::TRANSPORT);
    assert!(db.hits().is_empty());
    assert_eq!(db.accepts(), 0);
    assert_eq!(vault.calls(), 0, "still no credential lookup");
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("db-deny-c2"))
        .expect("the namespace refusal is recorded");
    assert_eq!(event.code.as_deref(), Some("EGRESS_DATABASE_DENIED"));

    // (d) A DIFFERENT origin (another host — nothing listens on
    // 127.0.0.2): the static origin refusal, no network at all.
    let wat = echo_wat(&format!("http://127.0.0.2:{db_port}/query"), &[], b"");
    let dir = wasm_worker_dir(&wat);
    let manifest = no_grant_manifest("wasm-db-deny");
    let worker_ref = fixture_worker_ref(dir.path(), &manifest, Some("nsDb"));
    let grant = fixture_db_grant(&worker_ref, &format!("http://127.0.0.1:{db_port}/"), &vault);
    let mut isolate = assemble_wasm_isolate_with_database(
        &worker_ref,
        None,
        Some(Arc::clone(&guard)),
        Some(grant),
    );
    let res = within(
        "the other-origin execution",
        isolate.execute_wasm(host_request("db-deny-d"), &worker_ref.config),
    )
    .await
    .expect("the import is admitted; the refusal is a transport error");
    assert_eq!(guest_code(&res), WasmHttpError::TRANSPORT);
    assert!(
        db.hits().is_empty(),
        "the other origin never reached the fixture"
    );
    assert_eq!(db.accepts(), 0, "no dial to the other origin");
    assert_eq!(
        vault.calls(),
        0,
        "the origin refusal is before the credential lookup"
    );
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("db-deny-d"))
        .expect("the origin refusal is recorded");
    assert_eq!(event.code.as_deref(), Some("EGRESS_DATABASE_DENIED"));

    db.shutdown().await;
}

/// (EDG-52/54 07d3b) Both grants on the SAME isolate: with the general
/// egress grant (cfg.egress SOME) the private router behind the general
/// field sends the grant origin to the DB bridge and every other URL to
/// the general guard bridge (the TLS endpoint); with the SAME isolate and
/// the SAME module bytes but `config.egress` NONE the general transport
/// is NEVER selected — not even a module cache hit after a granted call
/// (the TLS URL reaches the DB bridge and is refused there: no new TLS
/// hit, no database hit, no new credential lookup) — and the DB keeps
/// working under the NONE config. `with_wasm_bytes` swaps the modules on
/// the SAME isolate (the handler and the module cache are preserved).
#[tokio::test]
async fn the_router_serves_both_transports_and_revocation_is_per_execution() {
    let upstream = spawn_upstream().await;
    let tls_port = upstream.addr.port();
    let db = spawn_database_server().await;
    let db_port = db.addr.port();
    let store = OperationalStore::default();
    let guard = guard_allow_local(&store);

    // The two modules of the SAME isolate.
    let wat_tls = echo_wat(&format!("https://127.0.0.1:{tls_port}/ok"), &[], b"");
    let dir_tls = wasm_worker_dir(&wat_tls);
    let db_headers = vec![
        (
            "authorization".to_string(),
            "Bearer forged-canary".to_string(),
        ),
        ("x-namespace-bin".to_string(), "Zm9yZ2Vk".to_string()),
    ];
    let wat_db = echo_wat(
        &format!("http://127.0.0.1:{db_port}/query?fixture=db"),
        &db_headers,
        b"db-body-canary",
    );
    let dir_db = wasm_worker_dir(&wat_db);

    // The general grant (cfg.egress SOME: 443 + the TLS port) and the
    // private database authority (the plain-HTTP fixture origin).
    let manifest = grant_manifest("wasm-both", &[443, tls_port]);
    let worker = fixture_worker_ref(dir_tls.path(), &manifest, Some("nsBoth"));
    let vault = CanaryVault::new(&["both-token-1", "both-token-2"]);
    let grant = fixture_db_grant(&worker, &format!("http://127.0.0.1:{db_port}/"), &vault);

    let mut isolate = assemble_wasm_isolate_with_database(
        &worker,
        None,
        Some(Arc::clone(&guard)),
        Some(Arc::clone(&grant)),
    );

    // 1. The TLS module under the general grant: the router sends it to
    // the general bridge (no database hit, no lookup).
    let res = within(
        "the tls execution",
        isolate.execute_wasm(host_request("both-tls-1"), &worker.config),
    )
    .await
    .expect("the general grant + guard reaches the TLS endpoint");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.as_ref().map(|b| b.as_ref()), Some(b"ok".as_ref()));
    assert_eq!(upstream.hits().len(), 1, "one TLS hit");
    assert!(
        db.hits().is_empty(),
        "the TLS URL never reached the database"
    );
    assert_eq!(vault.calls(), 0, "the TLS URL needs no database credential");

    // 2. The SAME isolate with the DB module (with_wasm_bytes): the
    // router sends the grant origin to the DB bridge.
    let db_bytes = load_wasm_from_worker_dir(dir_db.path(), "index.wat").expect("the db module");
    isolate = isolate.with_wasm_bytes(db_bytes);
    let res = within(
        "the db execution",
        isolate.execute_wasm(host_request("both-db-1"), &worker.config),
    )
    .await
    .expect("the grant origin reaches the database through the router");
    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"db-ok".as_ref())
    );
    let hits = db.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].method, "POST");
    assert_eq!(hits[0].target, "/query?fixture=db");
    assert_eq!(hits[0].body, b"db-body-canary");
    assert_eq!(
        hits[0].authorization.as_deref(),
        Some("Bearer both-token-1")
    );
    assert_eq!(
        hits[0].namespace_bin.as_deref(),
        Some(sqlns_header().as_str())
    );
    assert_eq!(vault.calls(), 1);
    assert_eq!(
        upstream.hits().len(),
        1,
        "the DB URL never reached the TLS upstream"
    );

    // 3. Revocation: the SAME TLS module bytes (the module cache hit of
    // step 1) and the SAME isolate, but config.egress NONE — the general
    // transport is NEVER selected: the TLS URL reaches the DB bridge and
    // is refused there (the https origin is not the grant's http origin)
    // — no new TLS hit, no database hit, no new lookup.
    let manifest_none = no_grant_manifest("wasm-both");
    let worker_none = fixture_worker_ref(dir_tls.path(), &manifest_none, Some("nsBoth"));
    let tls_bytes = load_wasm_from_worker_dir(dir_tls.path(), "index.wat").expect("the tls module");
    isolate = isolate.with_wasm_bytes(tls_bytes);
    let res = within(
        "the revoked tls execution",
        isolate.execute_wasm(host_request("both-tls-none"), &worker_none.config),
    )
    .await
    .expect(
        "the import stays admitted (the private binding); \
         the refusal is a transport error",
    );
    assert_eq!(guest_code(&res), WasmHttpError::TRANSPORT);
    assert_eq!(
        upstream.hits().len(),
        1,
        "the NONE config never selects the general transport (no new TLS hit)"
    );
    assert_eq!(
        db.hits().len(),
        1,
        "the refused TLS URL produced no database hit"
    );
    assert_eq!(vault.calls(), 1, "the refused TLS URL needed no new lookup");
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("both-tls-none"))
        .expect("the refusal is recorded");
    assert_eq!(event.code.as_deref(), Some("EGRESS_DATABASE_DENIED"));
    assert_eq!(event.worker.as_deref(), Some("wasm-both"));
    assert_eq!(event.namespace.as_deref(), Some("nsBoth"));

    // 4. The DB keeps working under the NONE config (the private binding
    // admits the import without the general grant) — the credential
    // resolved fresh.
    let db_bytes =
        load_wasm_from_worker_dir(dir_db.path(), "index.wat").expect("the db module again");
    isolate = isolate.with_wasm_bytes(db_bytes);
    let res = within(
        "the db execution under the none config",
        isolate.execute_wasm(host_request("both-db-none"), &worker_none.config),
    )
    .await
    .expect("the database keeps working without the general grant");
    assert_eq!(res.status, 200);
    let hits = db.hits();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[1].authorization.as_deref(),
        Some("Bearer both-token-2"),
        "the credential is resolved fresh per execution"
    );
    assert_eq!(vault.calls(), 2);

    upstream.shutdown().await;
    db.shutdown().await;
}

/// (EDG-52/54 07d3b) A database error never falls back to the general
/// transport: with both grants bound (the router on the general field),
/// the fixture's service redirect (`/redir` -> 302 `/elsewhere`) is
/// refused by the DB bridge (every 3xx refuses and closes the upstream)
/// — no second hop, no fallback to the general bridge (the TLS upstream
/// stays untouched), the guest observes the stable transport code, the
/// config is untouched and the event carries the host request id and the
/// WorkerRef context.
#[tokio::test]
async fn a_database_error_does_not_fall_back_to_the_general_transport() {
    let upstream = spawn_upstream().await;
    let db = spawn_database_server().await;
    let db_port = db.addr.port();
    let store = OperationalStore::default();
    let guard = guard_allow_local(&store);

    let wat = echo_wat(&format!("http://127.0.0.1:{db_port}/redir"), &[], b"");
    let dir = wasm_worker_dir(&wat);
    let manifest = grant_manifest("wasm-both-err", &[443, upstream.addr.port()]);
    let worker = fixture_worker_ref(dir.path(), &manifest, Some("nsBoth"));
    let vault = CanaryVault::new(&["err-token-1"]);
    let grant = fixture_db_grant(&worker, &format!("http://127.0.0.1:{db_port}/"), &vault);

    let settings_before = worker.config.egress.clone();
    let mut isolate = assemble_wasm_isolate_with_database(
        &worker,
        None,
        Some(Arc::clone(&guard)),
        Some(Arc::clone(&grant)),
    );

    let res = within(
        "the redirect execution",
        isolate.execute_wasm(host_request("both-err-1"), &worker.config),
    )
    .await
    .expect("the import is admitted; the refusal is a transport error");
    assert_eq!(guest_code(&res), WasmHttpError::TRANSPORT);

    // Only the first hop reached the database; the redirect was never
    // followed (no `/elsewhere` hit) and the general transport was never
    // consulted (the TLS upstream is untouched).
    let hits = db.hits();
    assert_eq!(hits.len(), 1, "exactly the /redir hit");
    assert_eq!(hits[0].target, "/redir");
    assert_eq!(
        hits.into_iter()
            .filter(|h| h.target.split('?').next() == Some("/elsewhere"))
            .count(),
        0,
        "the service redirect is never followed"
    );
    assert!(
        upstream.hits().is_empty(),
        "no fallback to the general transport"
    );
    assert_eq!(
        vault.calls(),
        1,
        "the credential was resolved once, before the dial"
    );

    // The config is untouched by the refusal (no secret gained, no
    // egress grant invented).
    assert_eq!(worker.config.egress.clone(), settings_before);

    // The event: the host request id and the WorkerRef context.
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("both-err-1"))
        .expect("the refusal is recorded");
    assert_eq!(event.worker.as_deref(), Some("wasm-both-err"));
    assert_eq!(event.version.as_deref(), Some("1.0.0"));
    assert_eq!(event.namespace.as_deref(), Some("nsBoth"));
    assert_eq!(event.process_id, None);
    assert_eq!(event.outcome.as_deref(), Some("blocked"));
    assert_eq!(event.code.as_deref(), Some("EGRESS_DATABASE_DENIED"));
    assert_eq!(event.status, Some(403));

    upstream.shutdown().await;
    db.shutdown().await;
}
