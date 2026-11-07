//! (EDG-52 08c) The REAL Go module over the WASM HTTP ABI: the `index.wasm`
//! compiled from `workers/examples/go-wasm-http` with the Go 1.27.3
//! toolchain runs through the shared production assembly
//! `assemble_wasm_isolate_with_database` (the four-parameter factory) and
//! the real `Isolate::execute_wasm`, with the REAL `AppSecretVault`
//! (SQLite in a tempdir, synthetic 32-byte master key) as the app-secret
//! resolver, the STRICT guard (the default block tables, NO allow CIDR) and
//! a local plain-HTTP TCP stand-in that imitates ONLY the SQLD
//! `POST /v2/pipeline` endpoint.
//!
//! The artifact is IMMUTABLE: `EDGER_GO_WASM_FIXTURE` must name the built
//! `index.wasm` (the SHA is checked by the report, never rebuilt here).
//! Without the env var, a missing file or a non-wasm artifact this test
//! PANICS with a clear message: it never skips and never returns Ok.
//!
//! The REAL `manifest.yaml` of the example is parsed against the
//! `WorkerManifest` schema BEFORE the fixture overrides (the `SQLD_URL` env
//! value, the 60 s timeout and the 4 mb body cap) — no invented config that
//! would hide a schema break. The module bytes are copied into a TempDir
//! worker dir as `index.wasm` and loaded through the production worker-dir
//! loader (a product, not WAT).
//!
//! The host grant is the exact `WorkerRef` identity (name plus RBAC
//! namespace) as owner, the fixed loopback origin, a SQLD namespace that
//! DIFFERS from the RBAC namespace, the credential reference `SQLD_TOKEN`
//! and the real vault as the resolver. The private loopback origin is
//! admitted by the grant alone (no `allow_cidrs`); the general egress stays
//! denied.
//!
//! The TCP stand-in runs NO SQL and proves nothing about a real SQLD
//! engine (that is 07d/09/20); it records the method/path/body, the
//! host-imposed `Authorization` and `x-namespace-bin`, the accepts and the
//! hits, and answers the minimal pipeline JSON.
//!
//! Run (WSL Ubuntu-26.04; the artifact is never rebuilt in this test):
//!
//!     EDGER_GO_WASM_FIXTURE=/mnt/c/Users/dj4lm/repo.git/djalmajr/edger/.soho/w16/go-wasm/go-sdk-root-index.wasm \
//!       cargo test -p edger-orchestrator --test go_wasm_http -- --ignored --test-threads=1

use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::Engine;
use bytes::Bytes;
use edger_core::{
    parse_worker_config, ExecutionKind, Isolate, SerializedRequest, WorkerManifest, WorkerRef,
};
use edger_orchestrator::observability::{
    OperationalEvent, OperationalEventQuery, OperationalStore,
};
use edger_orchestrator::{
    assemble_wasm_isolate_with_database, AppSecretVault, EgressDatabaseGrant, EgressGuard,
    EgressGuardConfig, EgressWorkerContext,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

/// The outer bound of every execution (the COLD Go module of 12 MiB
/// measured ~16 s; the guest budget is the 60 s manifest timeout, the
/// external watchdog is the 900 s `timeout` of the WSL runner).
const EXEC: Duration = Duration::from_secs(120);
/// The head/body bounds of the local TCP stand-in (a hostile head over the
/// cap is answered 431; a `Content-Length` over the cap is answered 413
/// BEFORE any body byte is read; the fixed pipeline request is far below).
const HEAD_CAP: usize = 64 * 1024;
const BODY_CAP: usize = 64 * 1024;
/// The total deadline of every accepted stand-in connection (the whole
/// head/body/write phases; after it the stream is dropped and the peer sees
/// the close — the old helper had no such bound).
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
/// The app name the vault scopes its records under (the real manifest name).
const APP: &str = "go-wasm-http";
/// The RBAC namespace of the positive isolate (distinct from the SQLD
/// namespace the grant carries in `x-namespace-bin`).
const NS_RBAC: &str = "nsGo";
/// The SQLD namespace of the grant (deliberately different from `NS_RBAC`).
const NS_SQLD: &str = "sqlns-go";
/// The fixed SQLD probe the Go module sends (never takes SQL from the
/// visitor) — the exact body the stand-in must observe.
const PIPELINE_REQUEST: &str =
    r#"{"requests":[{"type":"execute","stmt":{"sql":"SELECT 1 AS value"}},{"type":"close"}]}"#;
/// The minimal pipeline answer: results ok/execute (rows
/// `[[{"type":"integer","value":"1"}]]`)/ok close; no baton/base_url.
const PIPELINE_OK: &str = r#"{"results":[{"type":"ok","response":{"result":{"rows":[[{"type":"integer","value":"1"}]]}}},{"type":"ok"}]}"#;
/// The Go module's static bodies (from `main.go`; never an upstream body,
/// URL, token or error text).
const DB_OK_BODY: &str = r#"{"ok":true,"value":1}"#;
const DB_404_BODY: &str = r#"{"error":"not found"}"#;
const DB_502_BODY: &str = r#"{"error":"upstream error"}"#;
const DB_503_BODY: &str = r#"{"error":"configuration missing"}"#;

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(EXEC, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{what} exceeded the {EXEC:?} outer bound"),
    }
}

/// `within` with an explicit bound (the client watchdogs of the raw TCP
/// probes are shorter than the outer EXEC bound): a timeout panics with the
/// label, so an answer that arrives only after the deadline cannot pass a
/// probe that requires earliness.
async fn within_deadline<T>(what: &str, bound: Duration, fut: impl Future<Output = T>) -> T {
    match tokio::time::timeout(bound, fut).await {
        Ok(value) => value,
        Err(_) => panic!("{what} exceeded the {bound:?} bound"),
    }
}

/// One REAL timing line (best effort): beside the fixture in the scratch
/// dir, plus an eprintln for the `--nocapture` log. The merged log can
/// interleave the lines; the file is the clean record.
fn note_timing(fixture: &Path, label: &str, ms: u128) {
    let line = format!("{label} {ms} ms\n");
    if let Some(dir) = fixture.parent() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("go-real-origin-timings.txt"))
        {
            use std::io::Write;
            let _ = file.write_all(line.as_bytes());
        }
    }
    eprintln!("go-real-http: {line}");
}

/// The fixture discipline: `EDGER_GO_WASM_FIXTURE` names the REAL Go build
/// artifact. Absent env var, missing file or a non-wasm artifact is a clear
/// panic — never a skip, never an Ok.
fn fixture_wasm_path() -> std::path::PathBuf {
    let raw = std::env::var("EDGER_GO_WASM_FIXTURE").unwrap_or_else(|_| {
        panic!("EDGER_GO_WASM_FIXTURE must name the real Go build artifact (index.wasm); this test never skips")
    });
    let path = std::path::PathBuf::from(&raw);
    assert!(path.is_file(), "the fixture artifact is missing: {raw}");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("cannot read the fixture artifact {raw}: {e}"));
    assert_eq!(
        &bytes[..4],
        b"\0asm",
        "the fixture artifact is not a wasm binary (bad magic)"
    );
    assert!(
        bytes.len() > (1 << 20),
        "the fixture artifact is implausibly small ({} bytes) — not the real Go build",
        bytes.len()
    );
    path
}

/// The REAL manifest of the example, parsed against the `WorkerManifest`
/// schema with the same saphyr options the production loader uses (the
/// override happens AFTER this parses — a schema break cannot be hidden).
fn real_manifest() -> WorkerManifest {
    let example_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workers/examples/go-wasm-http");
    let path = example_dir.join("manifest.yaml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the real example manifest at {path:?}: {e}"));
    let options = serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins,
        with_snippet: false,
    };
    serde_saphyr::from_str_with_options(&text, options).unwrap_or_else(|e| {
        panic!("the real example manifest fails the WorkerManifest schema: {e}")
    })
}

/// The fixture worker dir: the REAL Go artifact copied as `index.wasm`
/// (the production loader reads the raw `.wasm` bytes — a product, not WAT).
fn go_worker_dir(fixture: &Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(fixture, dir.path().join("index.wasm"))
        .expect("copies the real Go artifact as the worker entrypoint");
    dir
}

/// The fixture overrides of the parsed manifest: the `SQLD_URL` env value
/// (the non-secret service base the Go module validates), the 60 s guest
/// budget (the COLD 12 MiB module measured ~16 s; the WAT 8 s is wrong
/// here) and the 4 mb body cap.
fn fixture_manifest(base: WorkerManifest, sqld_url: &str) -> WorkerManifest {
    let mut manifest = base;
    manifest
        .env
        .as_mut()
        .expect("the real manifest declares the env map")
        .insert("SQLD_URL".to_string(), sqld_url.to_string());
    manifest.timeout = Some("60s".to_string());
    manifest.max_body_size = Some("4mb".to_string());
    manifest
}

/// The host-trusted `WorkerRef` the factory consumes: the identity fields
/// the production loader derives from the real manifest (name, the declared
/// version, the RBAC namespace) plus the same per-call `WorkerConfig`
/// (`parse_worker_config` + the worker dir the loader sets at load time).
fn fixture_worker_ref(manifest: &WorkerManifest, dir: &Path, namespace: Option<&str>) -> WorkerRef {
    let mut config = parse_worker_config(manifest);
    config.worker_dir = Some(dir.to_path_buf());
    WorkerRef {
        id: uuid::Uuid::new_v4(),
        name: manifest.name.clone(),
        version: manifest
            .version
            .clone()
            .expect("the real manifest declares a version"),
        dir: dir.to_path_buf(),
        namespace: namespace.map(str::to_owned),
        kind: config.kind.clone().unwrap_or(ExecutionKind::FetchHandler),
        config,
    }
}

/// The host request of one `/db` execution: the `request_id` is the HOST
/// identity the guard's events must carry (the guest frame carries none).
fn host_request(id: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: "/db".into(),
        headers: vec![],
        body: None,
        request_id: id.into(),
        base_href: None,
    }
}

/// The STRICT guard: the default block tables and NO allow CIDR (the
/// classifier refuses the loopback for the GENERAL egress; only the
/// private grant admits it). No `tls_roots` change: the stand-in is
/// plain HTTP.
fn strict_guard(store: &OperationalStore) -> Arc<EgressGuard> {
    Arc::new(EgressGuard::new(EgressGuardConfig::default()).with_event_store(store.clone()))
}

/// The `worker.egress` events of the store.
fn egress_events(store: &OperationalStore) -> Vec<OperationalEvent> {
    let query = OperationalEventQuery {
        kind: Some("worker.egress".into()),
        limit: Some(500),
        ..Default::default()
    };
    store.query(query).events
}

/// One exact field of the guard's stable `key=value` telemetry
/// (`build_message`: `host=… port=… [ip=…] bytes=… redirects=…
/// [reason=…]`, whitespace-separated): each part is split on the FIRST
/// `=` and the name must equal the key. A value never matches by
/// substring, so `port=123` cannot accept the prefix of `port=1234`
/// (or of a longer field that merely starts with the key).
fn message_field(message: &Option<String>, key: &str) -> Option<String> {
    message.as_ref().and_then(|message| {
        message.split_whitespace().find_map(|part| {
            part.split_once('=')
                .filter(|(name, _)| *name == key)
                .map(|(_, value)| value.to_owned())
        })
    })
}

fn response_header(res: &edger_core::SerializedResponse, name: &str) -> Option<String> {
    res.headers
        .iter()
        .find(|(h, _)| h.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

/// One recorded exchange of the SQLD stand-in.
#[derive(Clone)]
struct DbHit {
    method: String,
    target: String,
    content_type: Option<String>,
    authorization: Option<String>,
    namespace_bin: Option<String>,
    body: Vec<u8>,
}

/// The local plain-HTTP TCP stand-in of the SQLD service: it imitates ONLY
/// `POST /v2/pipeline` (a hostile head over `HEAD_CAP` is answered 431 and
/// closed; a `Content-Length` over `BODY_CAP` is answered 413 BEFORE any
/// body byte is read, and a malformed or overflowed `Content-Length` is
/// 400 — both with no hit; the body is then read only up to the validated
/// length, never buffered beyond it; every accepted connection is dropped
/// after `CONNECTION_TIMEOUT`), runs NO SQL and proves nothing about a real
/// SQLD engine. The accept loop and its per-connection tasks are OWNED by
/// the test and aborted in `shutdown` (no orphans). The pipeline route
/// answers `pipeline_status` (the default 200 or the 500 regression) with
/// the SAME exact `PIPELINE_OK` body.
#[derive(Clone)]
struct DbServer {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<DbHit>>>,
    accepts: Arc<AtomicUsize>,
    accept_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// The status of the `POST /v2/pipeline` route ONLY (every other route
    /// stays 405); the body is always the exact `PIPELINE_OK`.
    pipeline_status: u16,
}

impl DbServer {
    fn hits(&self) -> Vec<DbHit> {
        self.hits.lock().unwrap().clone()
    }

    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }

    /// Abort and await ONLY the handles this test owns (the accept loop
    /// and its per-connection tasks); the listener closes with the accept
    /// loop.
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

async fn spawn_db_server() -> DbServer {
    spawn_db_server_with_status(200).await
}

/// The stand-in with a chosen `POST /v2/pipeline` status: the default 200
/// and the 500 regression (the body stays the exact `PIPELINE_OK`; every
/// other route stays 405).
async fn spawn_db_server_with_status(status: u16) -> DbServer {
    let listener = within("the database bind", TcpListener::bind("127.0.0.1:0"))
        .await
        .expect("binds the plain-HTTP database fixture on 127.0.0.1");
    let addr = listener.local_addr().unwrap();
    let server = DbServer {
        addr,
        hits: Arc::new(Mutex::new(Vec::new())),
        accepts: Arc::new(AtomicUsize::new(0)),
        accept_task: Arc::new(Mutex::new(None)),
        conns: Arc::new(Mutex::new(Vec::new())),
        pipeline_status: status,
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
                // The whole connection (head, body and write) is bounded:
                // after CONNECTION_TIMEOUT the stream is dropped here and
                // the peer sees the close (a stalled client cannot hold the
                // fixture hostage).
                let _ =
                    tokio::time::timeout(CONNECTION_TIMEOUT, serve_db_conn(&mut tcp, &conn_shared))
                        .await;
            });
            shared.conns.lock().unwrap().push(conn);
        }
    });
    *server.accept_task.lock().unwrap() = Some(accept_task);
    server
}

fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

/// The refusal answer of the stand-in (400/413/431): the exact status head
/// and a clean close, with NO hit recorded (the 413 happens before any body
/// byte is read; the 400 before the head is trusted).
async fn reject_and_close(tcp: &mut TcpStream, status: u16, reason: &str) {
    let head =
        format!("HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    let _ = tcp.write_all(head.as_bytes()).await;
    let _ = tcp.shutdown().await;
}

/// One plain HTTP/1.1 exchange of the stand-in: the bounded head, the
/// `Content-Length` validated BEFORE any body byte is read (absent stays 0;
/// over `BODY_CAP` is refused 413; a non-numeric or overflowed value is
/// refused 400 — both closed, both without a hit), the body read only up to
/// the validated length (never buffered beyond it and truncated after), the
/// recorded hit, the fixed route (the pipeline status from the server) and
/// a clean close.
async fn serve_db_conn(tcp: &mut TcpStream, server: &DbServer) {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        if head_end(&buf).is_some() {
            break;
        }
        match tcp.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > HEAD_CAP {
                    reject_and_close(tcp, 431, "Request Header Fields Too Large").await;
                    return;
                }
            }
        }
    }
    let end = head_end(&buf).expect("head end found");
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
    let method = req.method.unwrap_or("?").to_string();
    let target = req.path.unwrap_or("/").to_string();
    let content_type = get_header("content-type");
    let authorization = get_header("authorization");
    let namespace_bin = get_header("x-namespace-bin");
    // P3: the Content-Length is validated BEFORE the body is awaited —
    // absent stays 0; a non-numeric or overflowed value is a malformed
    // request; a length over the cap is refused before reading a single
    // body byte (no hit in either refusal).
    let content_length = match get_header("content-length") {
        None => 0,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(length) => length,
            Err(_) => {
                reject_and_close(tcp, 400, "Bad Request").await;
                return;
            }
        },
    };
    if content_length > BODY_CAP {
        reject_and_close(tcp, 413, "Payload Too Large").await;
        return;
    }
    let mut body = buf.split_off(offset);
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let want = remaining.min(chunk.len());
        match tcp.read(&mut chunk[..want]).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(content_length);
    server.hits.lock().unwrap().push(DbHit {
        method: method.clone(),
        target: target.clone(),
        content_type,
        authorization,
        namespace_bin,
        body: body.clone(),
    });
    // ONLY the fixed SQLD route is imitated; everything else is refused.
    let path = target.split('?').next().unwrap_or("/").to_string();
    let (status, payload, json) = if method == "POST" && path == "/v2/pipeline" {
        (server.pipeline_status, PIPELINE_OK.as_bytes(), true)
    } else {
        (405, b"method not allowed".as_ref(), false)
    };
    let mut out = Vec::new();
    out.extend_from_slice(format!("HTTP/1.1 {status}\r\n").as_bytes());
    if json {
        out.extend_from_slice(b"content-type: application/json\r\n");
    }
    out.extend_from_slice(format!("content-length: {}\r\n", payload.len()).as_bytes());
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(payload);
    let _ = tcp.write_all(&out).await;
    let _ = tcp.shutdown().await;
}

/// (EDG-52 08c) Positive group on the SAME isolate/module cache: the COLD
/// real Go module answers the hostile `/inspect` preserving method,
/// requestUri, path, rawPath, rawQuery, host, the single `x-example` header
/// and the binary body; the bodyless `GET /inspect?empty=1` (no body, no
/// Content-Length) answers 200 with the echoed requestUri/rawQuery and the
/// EMPTY body (the P2 A regression, warm on the same cache); the warm `/db`
/// runs the fixed SQLD probe through
/// the private grant under the STRICT guard (the host imposes the fresh
/// `Bearer` credential and the `x-namespace-bin` of the grant's SQLD
/// namespace, which DIFFERS from the RBAC namespace of the event); the
/// vault rotation A -> B is observed at the NEXT execution without
/// re-creating the isolate or the module cache; the WasiConfig Debug and
/// the per-call config carry no vault value; and `config.secrets.clear`
/// (the grant and the resolver still present) makes Go answer the static
/// 503 BEFORE any fetch (no new hit, no new accept, no new event).
#[tokio::test]
#[ignore = "requires real Go build artifact; run with --ignored"]
async fn positive_real_go_inspect_db_rotation_and_secret_removal() {
    let db = spawn_db_server().await;
    let db_port = db.addr.port();
    assert_ne!(
        db_port, 9090,
        "the SQLD admin port is refused by the grant shape"
    );
    let store = OperationalStore::default();
    let guard = strict_guard(&store);
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(
        AppSecretVault::open(
            vault_dir.path().join("app-secrets.db"),
            Zeroizing::new([7; 32]),
        )
        .expect("the real vault opens in the tempdir"),
    );
    vault
        .set(APP, "SQLD_TOKEN", "canary-A")
        .expect("sets the canary A");

    let fixture = fixture_wasm_path();
    let manifest_base = real_manifest();
    assert_eq!(
        manifest_base.name, APP,
        "the fixture uses the real manifest identity"
    );
    assert_eq!(
        manifest_base.entrypoint.as_deref(),
        Some("index.wasm"),
        "the fixture loads the real entrypoint"
    );
    let manifest = fixture_manifest(manifest_base, &format!("http://127.0.0.1:{db_port}"));
    let worker = go_worker_dir(&fixture);
    let worker_ref = fixture_worker_ref(&manifest, worker.path(), Some(NS_RBAC));
    assert!(
        worker_ref.config.egress.is_none(),
        "no general egress grant (the manifest declares none)"
    );

    let owner = EgressWorkerContext {
        worker: worker_ref.name.clone(),
        version: worker_ref.version.clone(),
        namespace: worker_ref.namespace.clone(),
        process_id: String::new(),
    };
    let grant = Arc::new(
        EgressDatabaseGrant::new(
            &owner,
            &format!("http://127.0.0.1:{db_port}/"),
            NS_SQLD,
            "SQLD_TOKEN",
            Some(vault.clone()),
        )
        .expect("valid grant shape"),
    );

    let mut isolate = assemble_wasm_isolate_with_database(
        &worker_ref,
        Some(vault.clone()),
        Some(Arc::clone(&guard)),
        Some(Arc::clone(&grant)),
    );

    // Execution 1 — COLD: the 12 MiB Go module compiles and instantiates
    // inside the 60 s guest budget, far below the 120 s outer bound.
    let t0 = std::time::Instant::now();
    let res = within(
        "the cold /inspect execution",
        isolate.execute_wasm(
            SerializedRequest {
                method: "PATCH".into(),
                uri: "/inspect/a%2Fb?x=a%2Fb&x=two&empty=".into(),
                headers: vec![
                    ("host".to_string(), "visitor.example:4141".to_string()),
                    ("x-example".to_string(), "canary-x".to_string()),
                ],
                body: Some(Bytes::from(vec![0u8, 255, 1, 10])),
                request_id: "go-req-inspect".into(),
                base_href: None,
            },
            &worker_ref.config,
        ),
    )
    .await
    .expect("the cold execution must succeed");
    let cold_ms = t0.elapsed().as_millis();
    assert_eq!(res.status, 200);
    assert!(
        response_header(&res, "content-type")
            .as_deref()
            .is_some_and(|v| v.contains("application/json")),
        "the /inspect answer is JSON"
    );
    let inspect: serde_json::Value = serde_json::from_slice(
        res.body
            .expect("the /inspect answer carries a body")
            .as_ref(),
    )
    .expect("the /inspect answer is JSON");
    assert_eq!(inspect["method"], "PATCH");
    assert_eq!(inspect["requestUri"], "/inspect/a%2Fb?x=a%2Fb&x=two&empty=");
    assert_eq!(inspect["path"], "/inspect/a/b");
    assert_eq!(inspect["rawPath"], "/inspect/a%2Fb");
    assert_eq!(inspect["rawQuery"], "x=a%2Fb&x=two&empty=");
    assert_eq!(inspect["host"], "visitor.example:4141");
    assert_eq!(inspect["xExample"], "canary-x");
    let body_bytes = base64::engine::general_purpose::STANDARD
        .decode(inspect["body"].as_str().expect("the body is base64 JSON"))
        .expect("the body decodes");
    assert_eq!(
        body_bytes,
        vec![0u8, 255, 1, 10],
        "the binary body is preserved"
    );
    note_timing(&fixture, "cold /inspect", cold_ms);

    // Execution 1b — WARM on the SAME isolate/module cache: a GET /inspect
    // with a bodyless request (body `None`, no Content-Length). The P2 A
    // regression makes the Go module rebuild the request with a nil body
    // and panic in io.LimitReader (the static 500); the expected answer is
    // 200 with the echoed requestUri/rawQuery and the EMPTY body. Inserted
    // here, before any /db and before any secret change: no prior proof is
    // weakened and no secret change is reordered.
    let t0 = std::time::Instant::now();
    let res = within(
        "the bodyless /inspect?empty=1 execution",
        isolate.execute_wasm(
            SerializedRequest {
                method: "GET".into(),
                uri: "/inspect?empty=1".into(),
                headers: vec![],
                body: None,
                request_id: "go-req-inspect-empty".into(),
                base_href: None,
            },
            &worker_ref.config,
        ),
    )
    .await
    .expect("the bodyless /inspect execution must succeed");
    let empty_ms = t0.elapsed().as_millis();
    assert_eq!(res.status, 200);
    let inspect: serde_json::Value = serde_json::from_slice(
        res.body
            .expect("the bodyless /inspect answer carries a body")
            .as_ref(),
    )
    .expect("the bodyless /inspect answer is JSON");
    assert_eq!(inspect["method"], "GET");
    assert_eq!(inspect["requestUri"], "/inspect?empty=1");
    assert_eq!(inspect["rawQuery"], "empty=1");
    assert_eq!(
        inspect["body"].as_str(),
        Some(""),
        "the empty body is the empty base64 string"
    );
    note_timing(&fixture, "bodyless /inspect?empty=1", empty_ms);

    // Execution 1c — WARM on the SAME isolate/module cache, still before
    // any /db and before any secret change: the origin-form target that
    // BEGINS with `//` (`//visitor/inspect?x=1`, the R15 P2). The guest
    // must parse the target as a SERVER request URI (url.ParseRequestURI),
    // so the whole `//visitor/inspect` stays the URL path — a host is
    // never fabricated from the path — and the app's dispatch answers its
    // static 404. A 200 means the client URL parser turned the target into
    // host `visitor` + path `/inspect` (the R15 bug); a 400 or a 500 would
    // mean the raw uri was rejected or the module panicked, against the
    // frame contract that carries it verbatim. The Host header goes to
    // req.Host only, never into the URL.
    let t0 = std::time::Instant::now();
    let res = within(
        "the origin-form //visitor/inspect?x=1 execution",
        isolate.execute_wasm(
            SerializedRequest {
                method: "GET".into(),
                uri: "//visitor/inspect?x=1".into(),
                headers: vec![("host".to_string(), "visitor.example:4141".to_string())],
                body: None,
                request_id: "go-req-inspect-origin".into(),
                base_href: None,
            },
            &worker_ref.config,
        ),
    )
    .await
    .expect("the origin-form execution must succeed");
    let origin_ms = t0.elapsed().as_millis();
    assert_eq!(res.status, 404);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_404_BODY.as_bytes()),
        "the app's static not-found JSON (not the 400 frame, not the 500 static)"
    );
    note_timing(&fixture, "origin-form //visitor/inspect?x=1", origin_ms);

    // Execution 1d — WARM on the SAME isolate/module cache, still before
    // any /db and before any secret change: an ABSOLUTE-URI target
    // (`http://absolute.example:4141/inspect?x=1`) with NO explicit Host
    // header and no body. The guest parses the target with
    // url.ParseRequestURI and the URI's own host must land on req.Host:
    // the baseline leaves req.Host "" without the fix and the echoed
    // host is lost. The 200 answer must echo host
    // `absolute.example:4141`, the exact absolute requestUri and the raw
    // query `x=1` (the dispatch still matches the URL path `/inspect`).
    let t0 = std::time::Instant::now();
    let res = within(
        "the absolute-URI /inspect execution",
        isolate.execute_wasm(
            SerializedRequest {
                method: "GET".into(),
                uri: "http://absolute.example:4141/inspect?x=1".into(),
                headers: vec![],
                body: None,
                request_id: "go-req-inspect-absolute".into(),
                base_href: None,
            },
            &worker_ref.config,
        ),
    )
    .await
    .expect("the absolute-URI execution must succeed");
    let absolute_ms = t0.elapsed().as_millis();
    assert_eq!(res.status, 200);
    let inspect: serde_json::Value = serde_json::from_slice(
        res.body
            .expect("the absolute-URI /inspect answer carries a body")
            .as_ref(),
    )
    .expect("the absolute-URI /inspect answer is JSON");
    assert_eq!(inspect["method"], "GET");
    assert_eq!(
        inspect["requestUri"], "http://absolute.example:4141/inspect?x=1",
        "the absolute requestUri is preserved exactly"
    );
    assert_eq!(inspect["rawQuery"], "x=1");
    assert_eq!(
        inspect["host"], "absolute.example:4141",
        "the absolute URI's host reaches req.Host without an explicit header"
    );
    note_timing(&fixture, "absolute-URI /inspect", absolute_ms);

    // Execution 2 — WARM /db with canary A: the guest's fetch reaches the
    // stand-in ONLY through the private grant (the strict guard denies the
    // general egress); the host imposes its own credential and SQLD
    // namespace, and the answer carries no credential.
    let t0 = std::time::Instant::now();
    let res = within(
        "the /db canary-A execution",
        isolate.execute_wasm(host_request("go-req-db-a"), &worker_ref.config),
    )
    .await
    .expect("the private grant + the strict guard admit the import");
    let warm_ms = t0.elapsed().as_millis();
    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_OK_BODY.as_bytes()),
        "the validated integer 1, without any credential"
    );
    let hits = db.hits();
    assert_eq!(hits.len(), 1, "exactly one database hit");
    let hit = &hits[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.target, "/v2/pipeline", "the stand-in route, no query");
    assert_eq!(
        hit.body,
        PIPELINE_REQUEST.as_bytes(),
        "the fixed probe: execute SELECT 1 + close, no visitor SQL"
    );
    assert_eq!(
        hit.content_type.as_deref(),
        Some("application/json"),
        "the Go module sets its own content type"
    );
    assert_eq!(
        hit.authorization.as_deref(),
        Some("Bearer canary-A"),
        "the grant's own credential, resolved fresh"
    );
    assert_eq!(
        hit.namespace_bin.as_deref(),
        Some(
            base64::engine::general_purpose::STANDARD_NO_PAD
                .encode(NS_SQLD)
                .as_str()
        ),
        "the grant's own SQLD namespace (the pinned SQLD NO_PAD codec), not the RBAC one"
    );
    // Profile proof with the pinned decoder: the header decodes with
    // SQLD's STANDARD_NO_PAD back to exactly the grant's SQLD namespace.
    assert_eq!(
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(hit.namespace_bin.as_deref().unwrap()),
        Ok(NS_SQLD.as_bytes().to_vec()),
        "the x-namespace-bin header is accepted by the pinned SQLD decoder"
    );
    assert_ne!(
        NS_SQLD, NS_RBAC,
        "the fixture keeps the namespaces distinct"
    );
    note_timing(&fixture, "warm /db", warm_ms);

    // The event carries the WorkerRef identity (the RBAC namespace), the
    // HOST request id and `process_id = None` (one isolate, not processes).
    let event = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("go-req-db-a"))
        .expect("one event for the host request id");
    assert_eq!(event.worker.as_deref(), Some(APP));
    assert_eq!(event.version.as_deref(), Some("1.0.0"));
    assert_eq!(event.namespace.as_deref(), Some(NS_RBAC));
    assert_eq!(event.process_id, None);
    assert_eq!(event.outcome.as_deref(), Some("ok"));
    assert_eq!(event.status, Some(200));
    assert_eq!(event.code, None);

    // Rotation A -> B on the SAME isolate/module cache (no re-assembly):
    // the next execution resolves the fresh credential.
    vault
        .set(APP, "SQLD_TOKEN", "canary-B")
        .expect("re-PUT rotates A -> B");
    let res = within(
        "the /db canary-B execution",
        isolate.execute_wasm(host_request("go-req-db-b"), &worker_ref.config),
    )
    .await
    .expect("the rotation applies at the next execution");
    assert_eq!(res.status, 200);
    let hits = db.hits();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[1].authorization.as_deref(),
        Some("Bearer canary-B"),
        "the credential is renewed per execution"
    );

    // Privacy through the REAL getters (no config reconstruction): the
    // WasiConfig Debug and the per-call config carry the public URL but
    // never a vault value.
    let wasi_debug = format!("{:?}", isolate.wasi_config());
    assert!(
        !wasi_debug.contains("canary-A") && !wasi_debug.contains("canary-B"),
        "the WasiConfig Debug carries no vault value"
    );
    let config_debug = format!("{:?}", worker_ref.config);
    assert!(
        !config_debug.contains("canary-A") && !config_debug.contains("canary-B"),
        "the per-call config carries no vault value"
    );
    assert!(
        worker_ref
            .config
            .public_env
            .iter()
            .all(|name| !matches!(name.as_str(), "canary-A" | "canary-B")),
        "the public env names carry no vault value"
    );

    // `config.secrets.clear` with the private grant and the credential
    // resolver still present: Go sees no SQLD_TOKEN in its WASI env and
    // answers the static 503 BEFORE any fetch — the hits, the accepts and
    // the events stay untouched (the module looked at the secret, then
    // stopped).
    let hits_before = db.hits().len();
    let accepts_before = db.accepts();
    let mut no_secrets = worker_ref.config.clone();
    no_secrets.secrets.clear();
    let res = within(
        "the /db no-secret execution",
        isolate.execute_wasm(host_request("go-req-db-nosecret"), &no_secrets),
    )
    .await
    .expect("the execution still succeeds (the static 503 is a guest answer)");
    assert_eq!(res.status, 503);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_503_BODY.as_bytes()),
        "the static 503, no token, no URL, no error text"
    );
    assert_eq!(
        db.hits().len(),
        hits_before,
        "no fetch after the secret was removed"
    );
    assert_eq!(
        db.accepts(),
        accepts_before,
        "no dial after the secret was removed"
    );
    assert!(
        egress_events(&store)
            .iter()
            .all(|e| e.request_id.as_deref() != Some("go-req-db-nosecret")),
        "no egress event for the pre-fetch refusal"
    );

    db.shutdown().await;
}

/// (EDG-52 08c) Negative group: (a) a FRESH assembly with no private grant
/// and no general grant (the guard present) refuses the module that
/// imports `edger_http_v1.fetch` with `WASM_IMPORT_DENIED` — before any
/// dial, with zero network (the declared-secret env resolution may happen
/// first; the denial is the import policy, never a credential lookup);
/// (b) app B with its OWN secret present and a grant owned by app A (the
/// SAME origin) runs the Go module, but the DB bridge refuses the foreign
/// owner: the guest sees the static 502, the event is
/// `EGRESS_DATABASE_DENIED` 403 with the ctx of B, and the server saw
/// neither an accept nor a hit; (c) a NEW WorkerRef/isolate owned
/// EXACTLY by grant_a (app A's name and NS_RBAC — version and process
/// id do not bind the owner), whose real manifest carries, BEFORE the
/// factory, SQLD_URL = the OTHER origin (server B): the SAME grant_a
/// still authorizes only origin A, so the refusal is an ORIGIN denial
/// for the matching owner. The preconditions are proven separately on
/// the pure predicates (matches_owner TRUE; origin B denied by origin
/// while origin A is admitted for the same owner), and the refusal
/// telemetry must carry the EXACT port of B (the stable `key=value`
/// message parsed field by field, the port compared as u16, never a
/// substring), with zero accepts and hits on A and B. A new URL is a
/// new factory — the WasiConfig normal env is tied to the version and
/// the private binding is not revoked by a per-call
/// `config.egress = None` — never a per-execution env edit on a live
/// isolate.
#[tokio::test]
#[ignore = "requires real Go build artifact; run with --ignored"]
async fn negative_no_grant_denied_and_cross_owner_origin_refusals() {
    let db_a = spawn_db_server().await;
    let db_b = spawn_db_server().await;
    let port_a = db_a.addr.port();
    let port_b = db_b.addr.port();
    assert_ne!(
        port_a, 9090,
        "the SQLD admin port is refused by the grant shape"
    );
    assert_ne!(
        port_b, 9090,
        "the SQLD admin port is refused by the grant shape"
    );
    assert_ne!(port_a, port_b, "the two stand-in origins differ");
    let store = OperationalStore::default();
    let guard = strict_guard(&store);
    const APP_B: &str = "go-wasm-http-b";
    const NS_B: &str = "nsB";
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(
        AppSecretVault::open(
            vault_dir.path().join("app-secrets.db"),
            Zeroizing::new([9; 32]),
        )
        .expect("the real vault opens in the tempdir"),
    );
    vault
        .set(APP, "SQLD_TOKEN", "canary-neg-a")
        .expect("app A's secret present");
    vault
        .set(APP_B, "SQLD_TOKEN", "canary-b-secret")
        .expect("app B's own secret present");

    let fixture = fixture_wasm_path();

    // (a) No private grant and no general grant (the guard present): the
    // import is denied at the policy — the env resolution of the declared
    // secret may precede it, the denial is never a credential lookup DB.
    let manifest_a = fixture_manifest(real_manifest(), &format!("http://127.0.0.1:{port_a}"));
    let worker_a = go_worker_dir(&fixture);
    let ref_a = fixture_worker_ref(&manifest_a, worker_a.path(), Some("nsNeg"));
    let mut isolate_a = assemble_wasm_isolate_with_database(
        &ref_a,
        Some(vault.clone()),
        Some(Arc::clone(&guard)),
        None,
    );
    let err = within(
        "the no-grant execution",
        isolate_a.execute_wasm(host_request("go-req-neg-a"), &ref_a.config),
    )
    .await
    .expect_err("without any grant the import must be denied");
    assert_eq!(err.code, "WASM_IMPORT_DENIED");
    assert!(
        egress_events(&store).is_empty(),
        "no event before the denial"
    );
    assert!(db_a.hits().is_empty(), "no database hit");
    assert_eq!(db_a.accepts(), 0, "no dial without the grant");

    // (b) App B: its own secret present, a grant owned by app A (the SAME
    // origin): the import is admitted, the Go module runs and its fetch is
    // refused by the DB bridge (the foreign owner) — before the credential
    // lookup and any dial.
    let mut manifest_b = fixture_manifest(real_manifest(), &format!("http://127.0.0.1:{port_a}"));
    manifest_b.name = APP_B.to_string();
    let worker_b = go_worker_dir(&fixture);
    let ref_b = fixture_worker_ref(&manifest_b, worker_b.path(), Some(NS_B));
    let owner_a = EgressWorkerContext {
        worker: APP.to_string(),
        version: "1.0.0".to_string(),
        namespace: Some(NS_RBAC.to_string()),
        process_id: String::new(),
    };
    let grant_a = Arc::new(
        EgressDatabaseGrant::new(
            &owner_a,
            &format!("http://127.0.0.1:{port_a}/"),
            NS_SQLD,
            "SQLD_TOKEN",
            Some(vault.clone()),
        )
        .expect("valid grant shape"),
    );
    let mut isolate_b = assemble_wasm_isolate_with_database(
        &ref_b,
        Some(vault.clone()),
        Some(Arc::clone(&guard)),
        Some(Arc::clone(&grant_a)),
    );
    let t0 = std::time::Instant::now();
    let res = within(
        "the cross-owner execution",
        isolate_b.execute_wasm(host_request("go-req-b-1"), &ref_b.config),
    )
    .await
    .expect("the guest runs; the refusal is at the bridge, not the import");
    let cross_ms = t0.elapsed().as_millis();
    assert_eq!(res.status, 502);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_502_BODY.as_bytes()),
        "the static 502, no upstream body, no URL, no token"
    );
    note_timing(&fixture, "cold cross-owner /db", cross_ms);
    let denied = egress_events(&store)
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("go-req-b-1"))
        .expect("the bridge refusal records one event");
    assert_eq!(denied.code.as_deref(), Some("EGRESS_DATABASE_DENIED"));
    assert_eq!(denied.status, Some(403));
    assert_eq!(denied.outcome.as_deref(), Some("blocked"));
    assert_eq!(denied.worker.as_deref(), Some(APP_B));
    assert_eq!(
        denied.namespace.as_deref(),
        Some(NS_B),
        "the ctx of B, not the grant's"
    );
    assert_eq!(denied.process_id, None);
    // The owner-denial telemetry names the destination exactly: the
    // port of origin A, parsed field by field like the case (c) one.
    let denied_port: u16 = message_field(&denied.message, "port")
        .expect("the owner-denial telemetry carries the destination port")
        .parse()
        .expect("the port telemetry is a plain number");
    assert_eq!(
        denied_port, port_a,
        "the owner-denial telemetry names origin A's exact port"
    );
    assert!(
        db_a.hits().is_empty(),
        "no database hit for a foreign owner"
    );
    assert_eq!(db_a.accepts(), 0, "no dial for a foreign owner");

    // (c) A NEW WorkerRef/isolate owned EXACTLY by grant_a (app A's
    // name and NS_RBAC — version and process id do not bind the owner).
    // The real manifest parsed BEFORE the factory carries SQLD_URL =
    // the OTHER origin (server B): a new URL is a new factory, never a
    // per-execution env edit on a live isolate (the WasiConfig normal
    // env is tied to the version; the private binding is not revoked by
    // a per-call `config.egress = None`). The execution below passes
    // the UNMODIFIED `ref_c.config`.
    let manifest_c = fixture_manifest(real_manifest(), &format!("http://127.0.0.1:{port_b}"));
    let worker_c = go_worker_dir(&fixture);
    let ref_c = fixture_worker_ref(&manifest_c, worker_c.path(), Some(NS_RBAC));
    let ctx_c = EgressWorkerContext {
        worker: APP.to_string(),
        version: ref_c.version.clone(),
        namespace: ref_c.namespace.clone(),
        process_id: String::new(),
    };
    // Preconditions on the pure predicates (no network, no secret):
    // the grant's origin is exactly A, the owner MATCHES (so the
    // refusal below cannot be an owner denial), and origin B is denied
    // BY ORIGIN while origin A is admitted for the very same owner
    // (the control).
    assert_eq!(
        grant_a.origin().port_or_known_default(),
        Some(port_a),
        "the grant's origin is exactly server A"
    );
    assert!(
        grant_a.matches_owner(&ctx_c),
        "the new isolate is the grant's owner"
    );
    assert!(
        !grant_a.allows_request(&ctx_c, &format!("http://127.0.0.1:{port_b}/v2/pipeline")),
        "origin B is denied by origin for the matching owner"
    );
    assert!(
        grant_a.allows_request(&ctx_c, &format!("http://127.0.0.1:{port_a}/v2/pipeline")),
        "control: the same owner is admitted for origin A"
    );
    let mut isolate_c = assemble_wasm_isolate_with_database(
        &ref_c,
        Some(vault.clone()),
        Some(Arc::clone(&guard)),
        Some(Arc::clone(&grant_a)),
    );
    let t0 = std::time::Instant::now();
    let res = within(
        "the new-origin execution",
        isolate_c.execute_wasm(host_request("go-req-c-1"), &ref_c.config),
    )
    .await
    .expect("the guest runs; the refusal is at the bridge, by origin");
    let new_origin_ms = t0.elapsed().as_millis();
    assert_eq!(res.status, 502);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_502_BODY.as_bytes()),
        "the static 502, no upstream body, no URL, no token"
    );
    note_timing(&fixture, "cold new-origin /db", new_origin_ms);
    assert_eq!(db_a.accepts(), 0, "origin A is never dialed");
    assert!(db_a.hits().is_empty(), "origin A saw nothing");
    assert_eq!(db_b.accepts(), 0, "origin B is never dialed");
    assert!(db_b.hits().is_empty(), "origin B saw nothing");
    let denied_c = egress_events(&store)
        .into_iter()
        .filter(|e| e.request_id.as_deref() == Some("go-req-c-1"))
        .collect::<Vec<_>>();
    assert_eq!(
        denied_c.len(),
        1,
        "exactly one DBDENIED event for the new request id"
    );
    let denied_c = &denied_c[0];
    assert_eq!(denied_c.code.as_deref(), Some("EGRESS_DATABASE_DENIED"));
    assert_eq!(denied_c.status, Some(403));
    assert_eq!(denied_c.outcome.as_deref(), Some("blocked"));
    assert_eq!(denied_c.worker.as_deref(), Some(APP));
    assert_eq!(
        denied_c.namespace.as_deref(),
        Some(NS_RBAC),
        "the ctx of the new isolate (the grant's owner), not B's"
    );
    assert_eq!(denied_c.version.as_deref(), Some("1.0.0"));
    assert_eq!(denied_c.process_id, None);
    // The telemetry proves Go read origin B from the NEW factory's env
    // and really attempted it (no network): the stable `key=value`
    // message of the guard's `build_message` (host/port/bytes/redirects
    // /reason), parsed field by field — the port compared as u16, never
    // a substring that could accept a prefix.
    assert_eq!(
        message_field(&denied_c.message, "host").as_deref(),
        Some("127.0.0.1"),
        "the destination host telemetry"
    );
    let port_text = message_field(&denied_c.message, "port")
        .expect("the refusal telemetry carries the destination port");
    let port: u16 = port_text
        .parse()
        .expect("the port telemetry is a plain number");
    assert_eq!(port, port_b, "Go fetched origin B, not A");
    assert_eq!(
        message_field(&denied_c.message, "reason").as_deref(),
        Some("database_denied"),
        "the refusal reason telemetry"
    );

    db_a.shutdown().await;
    db_b.shutdown().await;
}

/// One RAW `POST /v2/pipeline` probe to the stand-in (no guest, no grant):
/// it proves the fixture's own status and body independently of the module
/// — the precondition of the P2 B regression (the 500 with the exact
/// `PIPELINE_OK` body).
async fn raw_pipeline_probe(addr: SocketAddr) -> (u16, Vec<u8>) {
    let mut tcp = within(
        "the stand-in precondition connect",
        TcpStream::connect(addr),
    )
    .await
    .expect("connects the plain-HTTP database fixture");
    let head = format!(
        "POST /v2/pipeline HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        PIPELINE_REQUEST.len()
    );
    tcp.write_all(head.as_bytes())
        .await
        .expect("sends the precondition head");
    tcp.write_all(PIPELINE_REQUEST.as_bytes())
        .await
        .expect("sends the exact probe body");
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match tcp.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let end = head_end(&buf).expect("the probe answer carries a complete head");
    let status = std::str::from_utf8(&buf[9..end])
        .expect("the head is ascii")
        .split_whitespace()
        .next()
        .expect("the status code field")
        .parse()
        .expect("a numeric status");
    (status, buf[end..].to_vec())
}

/// The status code of the stand-in answer head within the client
/// `watchdog` (the P3 probes send headers only and never a body): the
/// watchdog is a test bound — an answer that arrives only after the
/// `CONNECTION_TIMEOUT` deadline times it out and fails the probe, which is
/// what proves the refusal is early.
async fn probe_status_line(tcp: &mut TcpStream, watchdog: Duration, label: &str) -> u16 {
    let head = within_deadline(label, watchdog, async {
        let mut buf: Vec<u8> = Vec::with_capacity(4096);
        let mut chunk = [0u8; 4096];
        loop {
            if head_end(&buf).is_some() || buf.len() > HEAD_CAP {
                return buf;
            }
            match tcp.read(&mut chunk).await {
                Ok(0) | Err(_) => return buf,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    })
    .await;
    let end =
        head_end(&head).unwrap_or_else(|| panic!("{label}: the refusal carries no complete head"));
    std::str::from_utf8(&head[9..end])
        .expect("the refusal head is ascii")
        .split_whitespace()
        .next()
        .and_then(|field| field.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("{label}: the refusal head has no numeric status"))
}

/// (EDG-52 08P3) P3 security bound: the stand-in refuses a
/// `Content-Length` above `BODY_CAP` BEFORE reading any body byte. The
/// probe sends ONLY the `POST /v2/pipeline` headers (the body is never
/// written) and the 413 must arrive inside the 3 s client watchdog — below
/// the 5 s connection deadline, so a late refusal cannot pass — after which
/// the connection closes and no hit is recorded. The overflowed
/// `Content-Length` is refused 400 the same way (a subcase, not the
/// trigger). No guest, no grant: raw TCP against the fixture itself.
#[tokio::test]
async fn tcp_fixture_rejects_oversized_body_before_reading() {
    let db = spawn_db_server().await;
    let watchdog = Duration::from_secs(3);

    // Probe 1 — the trigger: Content-Length = BODY_CAP + 1, header only.
    let mut tcp = within("the oversized probe connect", TcpStream::connect(db.addr))
        .await
        .expect("connects the stand-in");
    tcp.write_all(
        format!(
            "POST /v2/pipeline HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            BODY_CAP + 1
        )
        .as_bytes(),
    )
    .await
    .expect("sends the header-only oversized probe (no body)");
    let status = probe_status_line(&mut tcp, watchdog, "the oversized probe refusal").await;
    assert_eq!(
        status, 413,
        "the over-cap Content-Length is refused before the body"
    );
    let mut chunk = [0u8; 64];
    assert!(
        matches!(
            within_deadline("the oversized probe close", watchdog, tcp.read(&mut chunk)).await,
            Ok(0)
        ),
        "the stand-in closes the refused connection"
    );
    let _ = tcp.shutdown().await;
    assert!(db.hits().is_empty(), "no hit for a refused body");
    assert_eq!(db.accepts(), 1, "the probe opened exactly one connection");

    // Probe 2 — subcase: an overflowed (non-numeric) Content-Length is
    // refused 400 the same way (header only, no body, no hit).
    let mut tcp = within("the overflowed probe connect", TcpStream::connect(db.addr))
        .await
        .expect("connects the stand-in");
    tcp.write_all(
        b"POST /v2/pipeline HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: 99999999999999999999999999\r\n\r\n",
    )
    .await
    .expect("sends the header-only overflowed probe (no body)");
    let status = probe_status_line(&mut tcp, watchdog, "the overflowed probe refusal").await;
    assert_eq!(
        status, 400,
        "the overflowed Content-Length is a malformed request"
    );
    assert!(
        matches!(
            within_deadline("the overflowed probe close", watchdog, tcp.read(&mut chunk)).await,
            Ok(0)
        ),
        "the stand-in closes the 400 connection"
    );
    let _ = tcp.shutdown().await;
    assert!(db.hits().is_empty(), "no hit for a malformed head");
    assert_eq!(db.accepts(), 2, "one connection per probe");
    db.shutdown().await;
}

/// (EDG-52 08P3) P3 security bound: the stand-in drops a stalled connection
/// at `CONNECTION_TIMEOUT` instead of holding it for the whole client
/// budget. (a) a client that never completes the head and (b) a client with
/// a valid head (`Content-Length: 1`) but no body both see the stream
/// closed (EOF or reset) inside `CONNECTION_TIMEOUT + 2 s` — on the helper
/// without the deadline this read would never end and the test's own bound
/// would fire. No hit is recorded for either; every resource is closed and
/// awaited. No guest, no grant: raw TCP against the fixture itself.
#[tokio::test]
async fn tcp_fixture_closes_stalled_connection_at_deadline() {
    let db = spawn_db_server().await;
    let bound = CONNECTION_TIMEOUT + Duration::from_secs(2);

    // (a) A head that never completes: the connection must be dropped at
    // the deadline (the helper without it would hold it open forever).
    let mut tcp = within("the stalled-head connect", TcpStream::connect(db.addr))
        .await
        .expect("connects the stand-in");
    tcp.write_all(b"POST /v2/pipeline HTTP/1.1\r\nhost: 127.0.0.1")
        .await
        .expect("sends an incomplete head (never finished)");
    let mut chunk = [0u8; 64];
    let closed = within_deadline("the stalled-head deadline", bound, tcp.read(&mut chunk)).await;
    assert!(
        matches!(&closed, Ok(0))
            || matches!(&closed, Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset),
        "the stalled head is dropped at the deadline (EOF or reset): {closed:?}"
    );
    let _ = tcp.shutdown().await;

    // (b) A complete valid head with Content-Length: 1 and NO body: the
    // body read must also end at the deadline.
    let mut tcp = within("the stalled-body connect", TcpStream::connect(db.addr))
        .await
        .expect("connects the stand-in");
    tcp.write_all(
        b"POST /v2/pipeline HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\ncontent-length: 1\r\n\r\n",
    )
    .await
    .expect("sends the valid head without its body");
    let closed = within_deadline("the stalled-body deadline", bound, tcp.read(&mut chunk)).await;
    assert!(
        matches!(&closed, Ok(0))
            || matches!(&closed, Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset),
        "the stalled body is dropped at the deadline (EOF or reset): {closed:?}"
    );
    let _ = tcp.shutdown().await;

    assert!(db.hits().is_empty(), "a dropped connection records no hit");
    assert_eq!(db.accepts(), 2, "one connection per stalled client");
    db.shutdown().await;
}

/// (EDG-52 08P2) P2 B regression: the Go module never checks the upstream
/// `resp.StatusCode`, so a valid `PIPELINE_OK` body served with a 500
/// status makes it answer 200 — the expected guest answer is the static
/// 502 (never the upstream body, URL or token). Same real artifact/manifest
/// (the fixture overrides included), REAL vault, STRICT guard and the
/// four-parameter assembly; the owner/origin/secret/grant are all CORRECT,
/// so the one authenticated hit is the exact fixed probe (the JSON that
/// already produces 200 in the positive). The precondition proves the
/// fixture itself answers 500 with the exact `PIPELINE_OK` (raw probe, no
/// guest). The guard forwards the upstream status as a NORMAL response: the
/// event registers the UPSTREAM 500 (outcome `ok`, no code), never the
/// guest's static 502 (a guest answer, not a guard error). The result is
/// recorded and this test's own handles are closed BEFORE the final
/// asserts; a failed assert never leaves this test's server open. Stand-in
/// without SQL, nothing about an engine. 60 s guest / 120 s outer.
#[tokio::test]
#[ignore = "requires real Go build artifact; run with --ignored"]
async fn negative_upstream_http_errors_with_valid_json_are_static_502() {
    let db = spawn_db_server_with_status(500).await;
    let db_port = db.addr.port();
    assert_ne!(
        db_port, 9090,
        "the SQLD admin port is refused by the grant shape"
    );

    // Precondition on the fixture itself (no guest involved): the stand-in
    // answers the EXACT `PIPELINE_OK` body with status 500 on its only
    // route — the 500 the guest must reject by status, not by body.
    let (probe_status, probe_body) = within(
        "the stand-in 500 precondition probe",
        raw_pipeline_probe(db.addr),
    )
    .await;
    assert_eq!(
        probe_status, 500,
        "the fixture answers 500 on the pipeline route"
    );
    assert_eq!(
        probe_body,
        PIPELINE_OK.as_bytes(),
        "the fixture body stays the exact PIPELINE_OK (valid JSON)"
    );

    let store = OperationalStore::default();
    let guard = strict_guard(&store);
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Arc::new(
        AppSecretVault::open(
            vault_dir.path().join("app-secrets.db"),
            Zeroizing::new([11; 32]),
        )
        .expect("the real vault opens in the tempdir"),
    );
    vault
        .set(APP, "SQLD_TOKEN", "canary-up500")
        .expect("sets the canary");

    let fixture = fixture_wasm_path();
    let manifest_base = real_manifest();
    assert_eq!(
        manifest_base.name, APP,
        "the fixture uses the real manifest identity"
    );
    assert_eq!(
        manifest_base.entrypoint.as_deref(),
        Some("index.wasm"),
        "the fixture loads the real entrypoint"
    );
    let manifest = fixture_manifest(manifest_base, &format!("http://127.0.0.1:{db_port}"));
    let worker = go_worker_dir(&fixture);
    let worker_ref = fixture_worker_ref(&manifest, worker.path(), Some(NS_RBAC));
    assert!(
        worker_ref.config.egress.is_none(),
        "no general egress grant (the manifest declares none)"
    );

    let owner = EgressWorkerContext {
        worker: worker_ref.name.clone(),
        version: worker_ref.version.clone(),
        namespace: worker_ref.namespace.clone(),
        process_id: String::new(),
    };
    let grant = Arc::new(
        EgressDatabaseGrant::new(
            &owner,
            &format!("http://127.0.0.1:{db_port}/"),
            NS_SQLD,
            "SQLD_TOKEN",
            Some(vault.clone()),
        )
        .expect("valid grant shape"),
    );

    let mut isolate = assemble_wasm_isolate_with_database(
        &worker_ref,
        Some(vault.clone()),
        Some(Arc::clone(&guard)),
        Some(Arc::clone(&grant)),
    );

    // The COLD real Go module (the 60 s guest budget, the 120 s outer
    // bound): the correct owner/origin/secret/grant admit the fetch. The
    // result and the events are captured BEFORE this test's own handles
    // close (the final asserts run after the shutdown).
    let t0 = std::time::Instant::now();
    let res = within(
        "the upstream-500 /db execution",
        isolate.execute_wasm(host_request("go-req-db-up500"), &worker_ref.config),
    )
    .await
    .expect("the private grant + the strict guard admit the import");
    let cold_ms = t0.elapsed().as_millis();
    let hits = db.hits();
    let events = egress_events(&store);

    db.shutdown().await;

    // Final asserts on the recorded result: the guest answers the static
    // 502 on the upstream 500 (never 200 on the valid body), and the only
    // authenticated hit is the exact fixed probe.
    assert_eq!(res.status, 502);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(DB_502_BODY.as_bytes()),
        "the static 502: no URL, no token, no upstream body"
    );
    let authenticated: Vec<&DbHit> = hits
        .iter()
        .filter(|hit| hit.authorization.is_some())
        .collect();
    assert_eq!(
        authenticated.len(),
        1,
        "exactly one authenticated POST /v2/pipeline hit (the precondition probe is unauthenticated)"
    );
    let hit = authenticated[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.target, "/v2/pipeline", "the stand-in route, no query");
    assert_eq!(
        hit.body,
        PIPELINE_REQUEST.as_bytes(),
        "the fixed probe JSON that already produces 200 in the positive"
    );
    assert_eq!(
        hit.authorization.as_deref(),
        Some("Bearer canary-up500"),
        "the grant's own credential, resolved fresh"
    );
    assert_eq!(
        hit.namespace_bin.as_deref(),
        Some(
            base64::engine::general_purpose::STANDARD_NO_PAD
                .encode(NS_SQLD)
                .as_str()
        ),
        "the grant's own SQLD namespace"
    );
    note_timing(&fixture, "cold upstream-500 /db", cold_ms);
    // The guard forwarded the upstream 500 as a NORMAL response: the event
    // registers the UPSTREAM status (500), never the guest's static 502
    // (the guest answer is not a guard error and carries no code).
    let event = events
        .into_iter()
        .find(|e| e.request_id.as_deref() == Some("go-req-db-up500"))
        .expect("one event for the host request id");
    assert_eq!(
        event.status,
        Some(500),
        "the upstream status, not the guest's 502"
    );
    assert_eq!(
        event.outcome.as_deref(),
        Some("ok"),
        "the guard forwarded the response"
    );
    assert_eq!(event.code, None, "no guard error code");
}
