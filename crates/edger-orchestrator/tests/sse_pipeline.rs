//! EDG-55 S3: the SSE data plane through the real pipeline.
//!
//! A scripted isolate (no Deno, no WASM engine) plays the guest and a real
//! TCP server carries the HTTP, so the tests cover what a browser sees: the
//! subscription is held by the HOST after the guest finished, publish routes
//! deliver after a clean 2xx, `x-edger-sse-*` never crosses either border
//! and ordinary requests are untouched.
//!
//! The guest is programmed by request headers (`x-sub`, `x-pub-json`,
//! `x-status`, `x-body-size`, ...) so every case is one HTTP request. The
//! real-binary proof with Deno and Go/WASM is slice S4.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use bytes::Bytes;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, StreamedResponse, WorkerConfig,
    WorkerRef, WorkerResponse,
};
use edger_isolation::{DenoProcessIsolate, StreamDetachBudget};
use edger_orchestrator::sse_broker::{SseBroker, SseLimits};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const STEP: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(300);

// ---------------------------------------------------------------------------
// The scripted guest
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Seen {
    headers: Vec<(String, String)>,
    method: String,
    request_id: Option<String>,
    streamed: bool,
    uri: String,
    worker: String,
}

type SeenLog = Arc<Mutex<Vec<Seen>>>;

fn header_of<'a>(req: &'a SerializedRequest, name: &str) -> Option<&'a str> {
    req.headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn headers_of<'a>(req: &'a SerializedRequest, name: &str) -> Vec<&'a str> {
    req.headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect()
}

/// The guest. `/events` authenticates (`authorization: Bearer ok`) and
/// subscribes to `x-sub` (default `room.1`); every other path is fully
/// programmed by the request headers.
fn guest(req: &SerializedRequest) -> SerializedResponse {
    let head = req.uri.split('?').next().unwrap_or("");
    // A vhost dispatch hands the guest an absolute uri.
    let path = match head.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |at| &rest[at..]),
        None => head,
    };
    let mut headers: Vec<(String, String)> = vec![("x-custom".into(), "kept".into())];
    let mut status: u16 = header_of(req, "x-status")
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let mut body = Bytes::from_static(br#"{"ok":true}"#);

    if path == "/events" {
        if header_of(req, "authorization") != Some("Bearer ok") {
            return SerializedResponse {
                status: 401,
                headers: vec![("content-type".into(), "application/json".into())],
                body: Some(Bytes::from_static(br#"{"error":"unauthorized"}"#)),
            };
        }
        status = 200;
        headers.push((
            "x-edger-sse-subscribe".into(),
            header_of(req, "x-sub").unwrap_or("room.1").to_string(),
        ));
        headers.push(("set-cookie".into(), "sid=guest; Path=/".into()));
        headers.push(("etag".into(), "\"v1\"".into()));
        headers.push(("cache-control".into(), "public, max-age=60".into()));
        headers.push(("content-type".into(), "application/json".into()));
    } else {
        if let Some(topics) = header_of(req, "x-sub") {
            headers.push(("x-edger-sse-subscribe".into(), topics.to_string()));
        }
        if let Some(topics) = header_of(req, "x-sub2") {
            headers.push(("x-edger-sse-subscribe".into(), topics.to_string()));
        }
    }
    // `x-pad: N:SIZE` adds N response headers of SIZE bytes (a big head).
    if let Some((count, size)) = header_of(req, "x-pad").and_then(|value| value.split_once(':')) {
        let (count, size): (usize, usize) = (count.parse().unwrap(), size.parse().unwrap());
        for index in 0..count {
            headers.push((format!("x-pad-{index}"), "p".repeat(size)));
        }
    }
    for json in headers_of(req, "x-pub-json") {
        headers.push(("x-edger-sse-publish".into(), URL_SAFE_NO_PAD.encode(json)));
    }
    for raw in headers_of(req, "x-pub-raw") {
        headers.push(("x-edger-sse-publish".into(), raw.to_string()));
    }
    if let Some(size) = header_of(req, "x-body-size").and_then(|value| value.parse().ok()) {
        body = Bytes::from(vec![b'x'; size]);
    }
    SerializedResponse {
        status,
        headers,
        body: Some(body),
    }
}

struct ScriptIsolate {
    seen: SeenLog,
    worker: String,
}

impl ScriptIsolate {
    /// Record one execution of the guest. `x-fail-first: N` makes the first
    /// N executions of the same request id (a retry reuses it) fail with a
    /// transport error the pool treats as retryable.
    fn begin(&self, req: &SerializedRequest, streamed: bool) -> Result<(), IsolationError> {
        let request_id = header_of(req, "x-request-id").map(str::to_owned);
        let mut seen = self.seen.lock().unwrap();
        let earlier = seen
            .iter()
            .filter(|prior| prior.worker == self.worker && prior.request_id == request_id)
            .count();
        seen.push(Seen {
            headers: req.headers.clone(),
            method: req.method.clone(),
            request_id,
            streamed,
            uri: req.uri.clone(),
            worker: self.worker.clone(),
        });
        let fail_first: usize = header_of(req, "x-fail-first")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        if earlier < fail_first {
            return Err(IsolationError::new("UDS_IO", "scripted transport failure"));
        }
        Ok(())
    }

    fn answer(&self, req: SerializedRequest) -> Result<SerializedResponse, IsolationError> {
        self.begin(&req, false)?;
        Ok(guest(&req))
    }

    /// `x-stream` delivers the guest's head with the body as three chunks;
    /// `x-stream-fail` fails the stream after two chunks. Without either
    /// header the answer is the ordinary buffered one.
    fn stream_answer(&self, req: SerializedRequest) -> Result<WorkerResponse, IsolationError> {
        let fails = header_of(&req, "x-stream-fail").is_some();
        if !fails && header_of(&req, "x-stream").is_none() {
            return self.answer(req).map(WorkerResponse::Buffered);
        }
        self.begin(&req, true)?;
        let head = guest(&req);
        let tail: Result<Bytes, IsolationError> = if fails {
            Err(IsolationError::new(
                "STREAM_BROKEN",
                "scripted stream error",
            ))
        } else {
            Ok(Bytes::from_static(b"gamma"))
        };
        let chunks = vec![
            Ok(Bytes::from_static(b"alpha-")),
            Ok(Bytes::from_static(b"beta-")),
            tail,
        ];
        Ok(WorkerResponse::Streamed(StreamedResponse {
            status: head.status,
            headers: head.headers,
            body: Box::pin(futures_util::stream::iter(chunks)),
            completed: None,
            production_complete: None,
            max_duration_elapsed_ms: None,
            frame_timeout_elapsed_ms: None,
        }))
    }
}

#[async_trait]
impl Isolate for ScriptIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.answer(req)
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.answer(req)
    }

    async fn execute_fetch_stream(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.stream_answer(req)
    }

    async fn execute_routes_stream(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.stream_answer(req)
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new("TEST", "no static spa in this fixture"))
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.answer(req)
    }
}

struct ScriptFactory {
    seen: SeenLog,
}

impl IsolateFactory for ScriptFactory {
    fn create_isolate(&self, worker: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(ScriptIsolate {
            seen: Arc::clone(&self.seen),
            worker: worker.name.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Fixture: workers on disk + a real TCP server
// ---------------------------------------------------------------------------

const SSE_BLOCK: &str = "sse:\n  subscribe: [\"/events\", \"/open\", \"/lobby/*\"]\n  publish: [\"/publish\", \"/rooms/*\"]\n";

fn fetch_manifest(name: &str, extra: &str) -> String {
    format!(
        "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n{extra}{SSE_BLOCK}"
    )
}

struct Files {
    dir: &'static str,
    files: Vec<(&'static str, String)>,
}

fn fetch_worker(name: &'static str, extra: &str) -> Files {
    Files {
        dir: name,
        files: vec![
            ("manifest.yaml", fetch_manifest(name, extra)),
            (
                "index.ts",
                "export default () => new Response('x');\n".into(),
            ),
        ],
    }
}

fn plain_worker() -> Files {
    Files {
        dir: "plain",
        files: vec![
            (
                "manifest.yaml",
                "name: plain\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n".into(),
            ),
            (
                "index.ts",
                "export default () => new Response('x');\n".into(),
            ),
        ],
    }
}

fn wasm_worker() -> Files {
    Files {
        dir: "wasmhub",
        files: vec![
            (
                "manifest.yaml",
                "name: wasmhub\nversion: \"1.0.0\"\nkind: fullstack\nadapter: wasm\nclientDir: dist\nwasmBackend:\n  entrypoint: backend/app.wasm\n  apiPrefix: /api\nsse:\n  subscribe: [\"/api/events\"]\n  publish: [\"/api/publish\"]\n".into(),
            ),
            ("dist/index.html", "<html>wasm spa</html>".into()),
            ("backend/app.wasm", "not a real module".into()),
        ],
    }
}

fn fullstack_js_worker() -> Files {
    Files {
        dir: "fshub",
        files: vec![
            (
                "manifest.yaml",
                "name: fshub\nversion: \"1.0.0\"\nkind: fullstack\nadapter: hono\nssrEntrypoint: server.ts\nclientDir: static\nassetPrefixes: [\"/static/\"]\nsse:\n  subscribe: [\"/events\", \"/static/*\"]\n  publish: [\"/publish\"]\n".into(),
            ),
            ("server.ts", "export default () => new Response('x');\n".into()),
            ("static/static/app.js", "console.log('asset');\n".into()),
        ],
    }
}

fn quiet_limits(max_connections: usize) -> SseLimits {
    SseLimits {
        max_connections,
        max_connections_per_app: 256,
        heartbeat: Duration::from_secs(3600),
    }
}

enum Broker {
    Absent,
    With(SseLimits),
}

struct Env {
    addr: SocketAddr,
    seen: SeenLog,
    server: ServerState,
    serve: tokio::task::JoinHandle<()>,
    _root: tempfile::TempDir,
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(broker) = self.server.sse_broker() {
            broker.shutdown();
        }
        self.serve.abort();
    }
}

async fn start(workers: Vec<Files>, broker: Broker) -> Env {
    start_with(workers, broker, |_, seen| Arc::new(ScriptFactory { seen })).await
}

/// `start` with the isolate factory of the caller (it gets the workers root
/// and the guest log).
async fn start_with(
    workers: Vec<Files>,
    broker: Broker,
    make_factory: impl FnOnce(&std::path::Path, SeenLog) -> Arc<dyn IsolateFactory>,
) -> Env {
    let root = tempfile::tempdir().unwrap();
    for worker in &workers {
        for (relative, content) in &worker.files {
            let path = root.path().join(worker.dir).join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
    }
    let seen: SeenLog = Arc::default();
    let server = ServerState::new_unready();
    if let Broker::With(limits) = broker {
        server.set_sse_broker(SseBroker::new(limits));
    }
    let factory = make_factory(root.path(), Arc::clone(&seen));
    let pool = WorkerPool::with_factory(PoolConfig::default(), factory);
    server.mark_ready(pool.clone());
    let state = OrchestratorState {
        server: server.clone(),
        pool,
        index: load_manifests_from_dirs(&[root.path().to_path_buf()]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    };
    let app = build_pipeline(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Env {
        addr,
        seen,
        server,
        serve,
        _root: root,
    }
}

async fn standard_env() -> Env {
    start(
        vec![
            fetch_worker("hub", "hosts: [\"hub.example.test\"]\nnameRoute: true\n"),
            fetch_worker("other", ""),
            fetch_worker("tiny", "sse_max: 1\n").with_max_connections(1),
            fetch_worker("small", "maxBodySize: 1kb\n"),
            plain_worker(),
            wasm_worker(),
            fullstack_js_worker(),
        ],
        Broker::With(quiet_limits(1024)),
    )
    .await
}

impl Files {
    /// Replace the generated `sse:` block with one that caps the app.
    fn with_max_connections(mut self, max: u32) -> Self {
        for (name, content) in &mut self.files {
            if *name == "manifest.yaml" {
                *content = content.replace("sse_max: 1\n", "");
                content.push_str(&format!("  maxConnections: {max}\n"));
            }
        }
        self
    }
}

// ---------------------------------------------------------------------------
// A minimal raw HTTP/1.1 client (the bodies must be observed unbuffered)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Raw {
    body: Vec<u8>,
    headers: Vec<(String, String)>,
    status: u16,
}

impl Raw {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn headers_named(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("json body: {}", self.text()))
    }

    fn assert_no_sse_headers(&self) {
        assert!(
            self.headers
                .iter()
                .all(|(name, _)| !name.to_ascii_lowercase().starts_with("x-edger-sse-")),
            "x-edger-sse-* reached the client: {:?}",
            self.headers
        );
    }
}

fn render_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    keep_open: bool,
) -> Vec<u8> {
    let mut text = format!("{method} {path} HTTP/1.1\r\n");
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        text.push_str(&format!("Host: {addr}\r\n"));
    }
    if !keep_open {
        text.push_str("Connection: close\r\n");
    }
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text.push_str("Content-Length: 0\r\n\r\n");
    text.into_bytes()
}

fn parse_head(raw: &[u8]) -> (u16, Vec<(String, String)>, usize) {
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("head terminator");
    let head = String::from_utf8_lossy(&raw[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status line");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect();
    (status, headers, end + 4)
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = raw.windows(2).position(|window| window == b"\r\n") {
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&raw[..line_end])
                .split(';')
                .next()
                .unwrap()
                .trim(),
            16,
        )
        .unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = line_end + 2;
        out.extend_from_slice(&raw[start..start + size]);
        raw = &raw[start + size + 2..];
    }
    out
}

async fn request(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)]) -> Raw {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(&render_request(addr, method, path, headers, false))
        .await
        .unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(STEP, stream.read_to_end(&mut raw))
        .await
        .expect("response within the step budget")
        .unwrap();
    let (status, headers, body_start) = parse_head(&raw);
    let chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked")
    });
    let body = if chunked {
        dechunk(&raw[body_start..])
    } else {
        raw[body_start..].to_vec()
    };
    Raw {
        body,
        headers,
        status,
    }
}

/// A held SSE connection: the head is parsed, the body accumulates.
struct SseClient {
    body: Vec<u8>,
    head: Raw,
    stream: TcpStream,
}

impl SseClient {
    /// Everything the server wrote after the head, chunk framing included.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    async fn wait_for(&mut self, needle: &str) {
        let deadline = tokio::time::Instant::now() + STEP;
        while !self.text().contains(needle) {
            let mut buffer = [0u8; 4096];
            let read = tokio::time::timeout_at(deadline, self.stream.read(&mut buffer))
                .await
                .unwrap_or_else(|_| {
                    panic!("timed out waiting for {needle:?}; got {:?}", self.text())
                })
                .unwrap();
            assert!(
                read > 0,
                "connection closed waiting for {needle:?}; got {:?}",
                self.text()
            );
            self.body.extend_from_slice(&buffer[..read]);
        }
    }

    /// Read for a short while and return only what arrived meanwhile.
    async fn drain_quiet(&mut self) -> String {
        let before = self.body.len();
        let deadline = tokio::time::Instant::now() + QUIET;
        loop {
            let mut buffer = [0u8; 4096];
            match tokio::time::timeout_at(deadline, self.stream.read(&mut buffer)).await {
                Ok(Ok(read)) if read > 0 => self.body.extend_from_slice(&buffer[..read]),
                _ => break,
            }
        }
        String::from_utf8_lossy(&self.body[before..]).into_owned()
    }
}

async fn open_sse(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> SseClient {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(&render_request(addr, "GET", path, headers, true))
        .await
        .unwrap();
    let mut raw = Vec::new();
    let head_end = loop {
        if let Some(position) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let mut buffer = [0u8; 4096];
        let read = tokio::time::timeout(STEP, stream.read(&mut buffer))
            .await
            .expect("head within the step budget")
            .unwrap();
        assert!(read > 0, "connection closed before the head: {raw:?}");
        raw.extend_from_slice(&buffer[..read]);
    };
    let (status, headers, _) = parse_head(&raw);
    let body = raw[head_end..].to_vec();
    SseClient {
        body,
        head: Raw {
            body: Vec::new(),
            headers,
            status,
        },
        stream,
    }
}

const AUTH: (&str, &str) = ("authorization", "Bearer ok");

async fn subscribe(env: &Env, path: &str) -> SseClient {
    let client = open_sse(env.addr, path, &[AUTH]).await;
    assert_eq!(client.head.status, 200, "{:?}", client.head.headers);
    assert_eq!(
        client.head.header("content-type"),
        Some("text/event-stream")
    );
    client
}

fn event_json(topic: &str, data: &str) -> String {
    format!(r#"{{"topic":"{topic}","data":"{data}"}}"#)
}

async fn publish(env: &Env, path: &str, directives: &[String]) -> Raw {
    let mut headers: Vec<(&str, &str)> = Vec::new();
    for json in directives {
        headers.push(("x-pub-json", json.as_str()));
    }
    request(env.addr, "POST", path, &headers).await
}

fn stats(env: &Env) -> edger_orchestrator::sse_broker::SseStats {
    env.server.sse_broker().expect("broker").snapshot()
}

fn guest_requests(env: &Env, worker: &str) -> Vec<Seen> {
    env.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|seen| seen.worker == worker)
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Subscription and delivery
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_authorized_subscription_receives_an_event_published_by_another_request() {
    let env = standard_env().await;
    let mut client = subscribe(&env, "/hub/events").await;

    // The response is exactly the five contract headers plus the guest's
    // other headers, with its cookie; nothing the guest could use to make
    // the browser cache or compress the stream.
    let head = &client.head;
    assert_eq!(head.header("cache-control"), Some("no-store"));
    assert_eq!(head.header("x-accel-buffering"), Some("no"));
    assert_eq!(head.header("x-custom"), Some("kept"));
    assert_eq!(head.headers_named("set-cookie"), vec!["sid=guest; Path=/"]);
    assert_eq!(head.header("etag"), None);
    assert_eq!(head.header("content-length"), None);
    assert_eq!(head.header("content-encoding"), None);
    assert_eq!(head.header("last-modified"), None);
    head.assert_no_sse_headers();

    let published = publish(
        &env,
        "/hub/publish",
        &[r#"{"topic":"room.1","data":"hello\nworld","event":"msg","id":"7"}"#.to_string()],
    )
    .await;
    assert_eq!(published.status, 200);
    published.assert_no_sse_headers();
    assert_eq!(published.json(), serde_json::json!({"ok": true}));

    client
        .wait_for("event: msg\nid: 7\ndata: hello\ndata: world\n\n")
        .await;
    let snapshot = stats(&env);
    assert_eq!(snapshot.workers["hub"].connections, 1);
    assert_eq!(snapshot.workers["hub"].published, 1);

    // The guest ran once for the subscription and is gone: only the
    // publisher's execution happened since.
    let runs = guest_requests(&env, "hub");
    assert_eq!(runs.iter().filter(|r| r.uri == "/events").count(), 1);
    assert_eq!(runs.iter().filter(|r| r.uri == "/publish").count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_topic_the_subscriber_did_not_authorize_is_not_delivered() {
    let env = standard_env().await;
    let mut client = subscribe(&env, "/hub/events").await;
    publish(&env, "/hub/publish", &[event_json("room.2", "nope")]).await;
    publish(&env, "/hub/publish", &[event_json("room.1", "yes")]).await;
    client.wait_for("data: yes\n\n").await;
    assert!(!client.text().contains("nope"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscription_works_on_a_fullstack_wasm_worker_under_the_api_prefix() {
    let env = standard_env().await;
    let mut client = subscribe(&env, "/wasmhub/api/events").await;
    let published = publish(
        &env,
        "/wasmhub/api/publish",
        &[event_json("room.1", "from-wasm")],
    )
    .await;
    assert_eq!(published.status, 200);
    published.assert_no_sse_headers();
    client.wait_for("data: from-wasm\n\n").await;

    // The guest saw the prefix-stripped path, as always.
    let runs = guest_requests(&env, "wasmhub");
    assert!(runs.iter().any(|run| run.uri == "/events"));

    // The SPA outside the prefix is untouched.
    let index = request(env.addr, "GET", "/wasmhub/", &[]).await;
    assert_eq!(index.status, 200);
    assert!(index.text().contains("wasm spa"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_name_route_a_version_pin_and_the_vhost_reach_the_same_scope() {
    let env = standard_env().await;
    let mut by_name = subscribe(&env, "/hub/events").await;
    let mut by_version = subscribe(&env, "/hub@1.0.0/events").await;
    let mut by_host = open_sse(env.addr, "/events", &[AUTH, ("host", "hub.example.test")]).await;
    assert_eq!(by_host.head.status, 200, "{:?}", by_host.head.headers);

    // Publishing through the vhost reaches the subscribers of the other two.
    let published = request(
        env.addr,
        "POST",
        "/publish",
        &[
            ("host", "hub.example.test"),
            ("x-pub-json", &event_json("room.1", "same-scope")),
        ],
    )
    .await;
    assert_eq!(published.status, 200, "{}", published.text());
    for client in [&mut by_name, &mut by_version, &mut by_host] {
        client.wait_for("data: same-scope\n\n").await;
    }
    assert_eq!(stats(&env).workers["hub"].connections, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_app_never_receives_an_event_with_the_same_topic() {
    let env = standard_env().await;
    let mut hub = subscribe(&env, "/hub/events").await;
    let mut other = subscribe(&env, "/other/events").await;

    publish(&env, "/hub/publish", &[event_json("room.1", "for-hub")]).await;
    hub.wait_for("data: for-hub\n\n").await;
    assert!(!other.drain_quiet().await.contains("for-hub"));

    publish(&env, "/other/publish", &[event_json("room.1", "for-other")]).await;
    other.wait_for("data: for-other\n\n").await;
    assert!(!hub.drain_quiet().await.contains("for-other"));
}

// ---------------------------------------------------------------------------
// What is not a subscription
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_401_comes_back_as_401_without_a_subscription() {
    let env = standard_env().await;
    let response = request(env.addr, "GET", "/hub/events", &[]).await;
    assert_eq!(response.status, 401);
    assert_eq!(
        response.json(),
        serde_json::json!({"error": "unauthorized"})
    );
    response.assert_no_sse_headers();
    assert_eq!(stats(&env).total_connections, 0);
    assert!(stats(&env)
        .workers
        .get("hub")
        .is_none_or(|w| w.connections == 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_200_without_the_header_and_other_statuses_are_common_responses() {
    let env = standard_env().await;
    let plain = request(env.addr, "GET", "/hub/open", &[]).await;
    assert_eq!(plain.status, 200);
    assert_eq!(plain.json(), serde_json::json!({"ok": true}));
    assert_eq!(plain.header("content-type"), None);
    assert_eq!(plain.header("x-custom"), Some("kept"));

    // A redirect or a 403 that carries a valid header is NOT a subscription.
    for status in ["302", "403", "204"] {
        let response = request(
            env.addr,
            "GET",
            "/hub/open",
            &[("x-sub", "room.1"), ("x-status", status)],
        )
        .await;
        assert_eq!(response.status.to_string(), status);
        response.assert_no_sse_headers();
    }
    assert_eq!(stats(&env).total_connections, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn head_and_post_on_a_subscribe_route_never_subscribe() {
    let env = standard_env().await;
    for method in ["HEAD", "POST", "PUT", "DELETE"] {
        let response = request(env.addr, method, "/hub/events", &[AUTH]).await;
        assert_eq!(response.status, 200, "{method}");
        assert_ne!(
            response.header("content-type"),
            Some("text/event-stream"),
            "{method}"
        );
        response.assert_no_sse_headers();
    }
    assert_eq!(stats(&env).total_connections, 0);
    // HEAD reached the guest as a common request (no broker involvement).
    assert!(guest_requests(&env, "hub")
        .iter()
        .any(|run| run.method == "HEAD" && run.uri == "/events"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_control_plane_invoke_neither_subscribes_nor_publishes() {
    let env = standard_env().await;
    let mut watcher = subscribe(&env, "/hub/events").await;
    let invoke = |path: &'static str, extra: Vec<(&'static str, String)>| {
        let addr = env.addr;
        async move {
            let mut headers: Vec<(&str, &str)> =
                vec![("x-edger-control-authorization", "Bearer test-root")];
            for (name, value) in &extra {
                headers.push((name, value.as_str()));
            }
            request(addr, "GET", path, &headers).await
        }
    };

    let subscribed = invoke(
        "/api/admin/workers/hub/invoke/open",
        vec![("x-sub", "room.1".into())],
    )
    .await;
    assert_eq!(subscribed.status, 200, "{}", subscribed.text());
    assert_ne!(subscribed.header("content-type"), Some("text/event-stream"));
    subscribed.assert_no_sse_headers();
    assert_eq!(stats(&env).total_connections, 1, "only the watcher");

    let published = invoke(
        "/api/admin/workers/hub/invoke/rooms/a",
        vec![("x-pub-json", event_json("room.1", "from-invoke"))],
    )
    .await;
    assert_eq!(published.status, 200, "{}", published.text());
    published.assert_no_sse_headers();
    assert!(!watcher.drain_quiet().await.contains("from-invoke"));
    assert_eq!(stats(&env).workers["hub"].published, 0);
}

// ---------------------------------------------------------------------------
// Directives: validation, status, borders
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_publish_directive_is_a_502_and_nothing_is_published() {
    let env = standard_env().await;
    let mut client = subscribe(&env, "/hub/events").await;

    // Only the SECOND of two is invalid: the first must not be delivered.
    let response = publish(
        &env,
        "/hub/publish",
        &[
            event_json("room.1", "first-valid"),
            r#"{"topic":"room.1","data":"x","worker":"other"}"#.to_string(),
        ],
    )
    .await;
    assert_eq!(response.status, 502);
    assert_eq!(response.json()["code"], "SSE_INVALID_DIRECTIVE");
    response.assert_no_sse_headers();
    assert!(!response.text().contains("other"));

    // Not base64url, bad topic, more than eight.
    for raw in ["not base64!", "e30", "%%%"] {
        let response = request(env.addr, "POST", "/hub/publish", &[("x-pub-raw", raw)]).await;
        assert_eq!(response.status, 502, "{raw}");
        assert_eq!(response.json()["code"], "SSE_INVALID_DIRECTIVE");
    }
    let nine: Vec<String> = (0..9).map(|_| event_json("room.1", "x")).collect();
    assert_eq!(publish(&env, "/hub/publish", &nine).await.status, 502);

    assert!(!client.drain_quiet().await.contains("data:"));
    assert_eq!(stats(&env).workers["hub"].published, 0);

    // A later valid publish still works: nothing was left half done.
    publish(&env, "/hub/publish", &[event_json("room.1", "fine")]).await;
    client.wait_for("data: fine\n\n").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_positional_array_directive_is_a_502_and_nothing_is_published() {
    let env = standard_env().await;
    let mut client = subscribe(&env, "/hub/events").await;

    // Field order of the derived struct: data, event, id, topic.
    let array = r#"["hello",null,null,"room.1"]"#.to_string();
    for directives in [
        vec![array.clone()],
        // A valid directive earlier in the same batch must not be delivered.
        vec![event_json("room.1", "first-valid"), array.clone()],
        vec![array.clone(), event_json("room.1", "last-valid")],
    ] {
        let response = publish(&env, "/hub/publish", &directives).await;
        assert_eq!(response.status, 502, "{directives:?}");
        assert_eq!(response.json()["code"], "SSE_INVALID_DIRECTIVE");
        response.assert_no_sse_headers();
    }

    let quiet = client.drain_quiet().await;
    assert!(!quiet.contains("data:"), "{quiet:?}");
    assert_eq!(stats(&env).workers["hub"].published, 0);

    // Control: the same event as an object is delivered.
    publish(&env, "/hub/publish", &[event_json("room.1", "hello")]).await;
    client.wait_for("data: hello\n\n").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_head_over_the_limits_with_a_directive_is_a_502_and_one_without_is_untouched() {
    let env = standard_env().await;
    let mut client = subscribe(&env, "/hub/events").await;

    // Publish: every directive is 6144 JSON bytes, so its header value is
    // 8192 bytes; eight of them are 8 * (19 + 8192) > 64 KiB of head.
    let big = |label: &str| {
        event_json(
            "room.1",
            &format!("{label}{}", "y".repeat(6116 - label.len())),
        )
    };
    let eight: Vec<String> = (0..8).map(|index| big(&format!("BIG{index}-"))).collect();
    let response = publish(&env, "/hub/publish", &eight).await;
    assert_eq!(response.status, 502, "{}", response.text());
    assert_eq!(response.json()["code"], "SSE_INVALID_DIRECTIVE");
    response.assert_no_sse_headers();
    assert!(!client.drain_quiet().await.contains("data:"));
    assert_eq!(stats(&env).workers["hub"].published, 0);
    // Control: one directive of that size (8 KiB of head) is delivered.
    let response = publish(&env, "/hub/publish", &eight[..1]).await;
    assert_eq!(response.status, 200, "{}", response.text());
    client.wait_for("data: BIG0-").await;
    assert_eq!(stats(&env).workers["hub"].published, 1);

    // Subscribe: the same rule on the declared subscribe route.
    let response = request(env.addr, "GET", "/hub/events", &[AUTH, ("x-pad", "9:8192")]).await;
    assert_eq!(response.status, 502, "{}", response.text());
    assert_eq!(response.json()["code"], "SSE_INVALID_DIRECTIVE");
    response.assert_no_sse_headers();
    let before = stats(&env).total_connections;
    let control = open_sse(env.addr, "/hub/events", &[AUTH, ("x-pad", "2:100")]).await;
    assert_eq!(control.head.status, 200);
    assert_eq!(stats(&env).total_connections, before + 1);

    // A declared route WITHOUT a directive keeps its head as before: a big
    // head within the limits (7 * 8 KiB) passes untouched. (A head over the
    // limits without a directive is not an SSE matter: the response
    // conversion in `wire.rs` already answers it 400, as it did before.)
    for (method, path) in [("POST", "/hub/publish"), ("GET", "/hub/plain")] {
        let response = request(env.addr, method, path, &[("x-pad", "7:8192")]).await;
        assert_eq!(response.status, 200, "{method} {path}: {}", response.text());
        assert_eq!(
            response
                .headers
                .iter()
                .filter(|(name, _)| name.starts_with("x-pad-"))
                .count(),
            7,
            "{method} {path}"
        );
    }
    // A directive with that same big head still within the limits is fine.
    let held = open_sse(
        env.addr,
        "/hub/lobby/a",
        &[("x-sub", "room.1"), ("x-pad", "7:8192")],
    )
    .await;
    assert_eq!(held.head.status, 200);
    drop(held);
    drop(control);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_2xx_publish_response_publishes_nothing_and_is_not_an_error() {
    let env = standard_env().await;
    let mut client = subscribe(&env, "/hub/events").await;
    for status in ["400", "404", "500", "302"] {
        let response = request(
            env.addr,
            "POST",
            "/hub/publish",
            &[
                ("x-status", status),
                ("x-pub-json", &event_json("room.1", "dropped")),
                // Invalid directives are discarded too, without a 502.
                ("x-pub-raw", "garbage!"),
            ],
        )
        .await;
        assert_eq!(response.status.to_string(), status);
        response.assert_no_sse_headers();
    }
    assert!(!client.drain_quiet().await.contains("data:"));
    assert_eq!(stats(&env).workers["hub"].published, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_or_repeated_subscribe_directive_is_a_502() {
    let env = standard_env().await;
    for (case, headers) in [
        (
            "bad topic",
            vec![("authorization", "Bearer ok"), ("x-sub", "a b")],
        ),
        ("empty", vec![("authorization", "Bearer ok"), ("x-sub", "")]),
        (
            "nine topics",
            vec![
                ("authorization", "Bearer ok"),
                ("x-sub", "a,b,c,d,e,f,g,h,i"),
            ],
        ),
    ] {
        let response = request(env.addr, "GET", "/hub/events", &headers).await;
        assert_eq!(response.status, 502, "{case}");
        assert_eq!(response.json()["code"], "SSE_INVALID_DIRECTIVE", "{case}");
        response.assert_no_sse_headers();
    }
    let repeated = request(
        env.addr,
        "GET",
        "/hub/open",
        &[("x-sub", "room.1"), ("x-sub2", "room.2")],
    )
    .await;
    assert_eq!(repeated.status, 502);
    assert_eq!(repeated.json()["code"], "SSE_INVALID_DIRECTIVE");
    assert_eq!(stats(&env).total_connections, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscription_body_over_64_kib_is_a_502_and_one_at_the_limit_is_discarded() {
    let env = standard_env().await;
    let over = request(
        env.addr,
        "GET",
        "/hub/open",
        &[("x-sub", "room.1"), ("x-body-size", "65537")],
    )
    .await;
    assert_eq!(over.status, 502);
    assert_eq!(over.json()["code"], "SSE_INVALID_DIRECTIVE");

    let mut exact = open_sse(
        env.addr,
        "/hub/open",
        &[("x-sub", "room.1"), ("x-body-size", "65536")],
    )
    .await;
    assert_eq!(exact.head.status, 200);
    assert_eq!(exact.head.header("content-type"), Some("text/event-stream"));
    assert!(
        !exact.drain_quiet().await.contains('x'),
        "the guest body is discarded"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_publish_body_over_the_worker_limit_is_a_502_response_too_large() {
    let env = standard_env().await;
    let response = request(
        env.addr,
        "POST",
        "/small/publish",
        &[
            ("x-body-size", "2048"),
            ("x-pub-json", &event_json("room.1", "never")),
        ],
    )
    .await;
    assert_eq!(response.status, 502);
    assert_eq!(response.json()["code"], "SSE_RESPONSE_TOO_LARGE");
    assert_eq!(
        stats(&env).workers.get("small").map_or(0, |w| w.published),
        0
    );

    let within = request(
        env.addr,
        "POST",
        "/small/publish",
        &[("x-body-size", "1024")],
    )
    .await;
    assert_eq!(within.status, 200);
    assert_eq!(within.body.len(), 1024, "the guest body is the response");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_headers_never_reach_the_client_on_any_route_of_a_worker_with_sse() {
    let env = standard_env().await;
    let mut watcher = subscribe(&env, "/hub/events").await;
    let directive = event_json("room.1", "must-not-publish");
    let headers: Vec<(&str, &str)> = vec![
        ("x-sub", "room.1"),
        ("x-pub-json", &directive),
        ("x-pub-raw", "garbage!"),
    ];

    // Undeclared routes, any method: ignored and stripped.
    for (method, path) in [
        ("GET", "/hub/plain"),
        ("POST", "/hub/plain"),
        ("GET", "/hub/lobby"),
        ("GET", "/hub/lobbyx/a"),
        ("GET", "/hub/events%2F"),
    ] {
        let response = request(env.addr, method, path, &headers).await;
        response.assert_no_sse_headers();
        assert_eq!(response.status, 200, "{method} {path}");
    }
    assert!(!watcher.drain_quiet().await.contains("data:"));
    assert_eq!(stats(&env).workers["hub"].published, 0);
    assert_eq!(stats(&env).total_connections, 1, "only the watcher");
}

fn runs_of_case(env: &Env, worker: &str, case: &str) -> Vec<Seen> {
    guest_requests(env, worker)
        .into_iter()
        .filter(|seen| {
            seen.headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("x-case") && value == case)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_streamed_head_on_an_undeclared_route_loses_its_sse_headers_and_keeps_its_body() {
    let env = standard_env().await;
    let mut watcher = subscribe(&env, "/hub/events").await;
    let directive = event_json("room.1", "must-not-publish");
    let headers: Vec<(&str, &str)> = vec![
        ("x-case", "streamed-head"),
        ("x-stream", "1"),
        ("x-sub", "room.1"),
        ("x-pub-json", &directive),
        ("x-pub-raw", "garbage!"),
    ];

    for method in ["GET", "POST"] {
        let response = request(env.addr, method, "/hub/plain", &headers).await;
        assert_eq!(response.status, 200, "{method}");
        response.assert_no_sse_headers();
        assert_eq!(response.header("x-custom"), Some("kept"), "{method}");
        assert_eq!(response.text(), "alpha-beta-gamma", "{method}");
    }

    // The guest really answered with a stream (not a buffered fallback).
    let runs = runs_of_case(&env, "hub", "streamed-head");
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run.streamed), "{runs:?}");

    assert!(!watcher.drain_quiet().await.contains("data:"));
    assert_eq!(stats(&env).workers["hub"].published, 0);
    assert_eq!(stats(&env).total_connections, 1, "only the watcher");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_publish_stream_that_fails_after_the_head_publishes_nothing() {
    let env = standard_env().await;
    let mut watcher = subscribe(&env, "/hub/events").await;
    let directive = event_json("room.1", "from-the-broken-stream");

    // Reference: what the pipeline already answers for a worker failure on
    // the same route (a transport error on a non-idempotent request).
    let reference = request(
        env.addr,
        "POST",
        "/hub/publish",
        &[
            ("x-case", "reference"),
            ("x-fail-first", "1"),
            ("x-pub-json", &directive),
        ],
    )
    .await;
    assert_eq!(reference.status, 500, "{}", reference.text());

    for method in ["POST", "GET"] {
        let case = format!("broken-{method}");
        let response = request(
            env.addr,
            method,
            "/hub/publish",
            &[
                ("x-case", &case),
                ("x-stream-fail", "1"),
                ("x-pub-json", &directive),
            ],
        )
        .await;
        assert_eq!(response.status, reference.status, "{method}");
        assert_eq!(
            response.json()["code"],
            reference.json()["code"],
            "{method}"
        );
        assert!(!response.text().contains("alpha"), "{}", response.text());
        response.assert_no_sse_headers();
        // A failed stream is not an idempotent transport error: no retry.
        let runs = runs_of_case(&env, "hub", &case);
        assert_eq!(runs.len(), 1, "{method}: {runs:?}");
        assert!(runs[0].streamed);
    }

    assert!(!watcher.drain_quiet().await.contains("data:"));
    assert_eq!(stats(&env).workers["hub"].published, 0);

    // Control: the same directive over a stream that ends cleanly is
    // delivered, once, and the body reaches the publisher intact.
    let response = request(
        env.addr,
        "POST",
        "/hub/publish",
        &[
            ("x-case", "clean-stream"),
            ("x-stream", "1"),
            ("x-pub-json", &directive),
        ],
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.text(), "alpha-beta-gamma");
    response.assert_no_sse_headers();
    watcher.wait_for("data: from-the-broken-stream\n\n").await;
    let extra = watcher.drain_quiet().await;
    assert!(!extra.contains("data:"), "{extra:?}");
    assert_eq!(stats(&env).workers["hub"].published, 1);
    assert!(runs_of_case(&env, "hub", "clean-stream")[0].streamed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transport_retry_on_a_publish_route_delivers_once_from_the_run_that_completed() {
    let env = standard_env().await;
    let mut watcher = subscribe(&env, "/hub/events").await;

    // GET is idempotent, so the common path's one retry applies: the first
    // run fails, the second completes and is the only one that publishes.
    let once = event_json("room.1", "once");
    let response = request(
        env.addr,
        "GET",
        "/hub/publish",
        &[
            ("x-case", "retry-ok"),
            ("x-fail-first", "1"),
            ("x-pub-json", &once),
        ],
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.text());
    response.assert_no_sse_headers();
    assert_eq!(runs_of_case(&env, "hub", "retry-ok").len(), 2);
    watcher.wait_for("data: once\n\n").await;
    let extra = watcher.drain_quiet().await;
    assert!(!extra.contains("data:"), "delivered twice: {extra:?}");
    assert_eq!(watcher.text().matches("data: once").count(), 1);
    assert_eq!(stats(&env).workers["hub"].published, 1);

    // Both runs fail: at most the two the common path allows, no delivery.
    let never = event_json("room.1", "never");
    let response = request(
        env.addr,
        "GET",
        "/hub/publish",
        &[
            ("x-case", "retry-exhausted"),
            ("x-fail-first", "2"),
            ("x-pub-json", &never),
        ],
    )
    .await;
    assert_eq!(response.status, 500, "{}", response.text());
    assert_eq!(runs_of_case(&env, "hub", "retry-exhausted").len(), 2);

    // POST is not idempotent: a single run, no retry, no delivery.
    let response = request(
        env.addr,
        "POST",
        "/hub/publish",
        &[
            ("x-case", "no-retry"),
            ("x-fail-first", "1"),
            ("x-pub-json", &never),
        ],
    )
    .await;
    assert_eq!(response.status, 500, "{}", response.text());
    assert_eq!(runs_of_case(&env, "hub", "no-retry").len(), 1);

    assert!(!watcher.drain_quiet().await.contains("data:"));
    assert_eq!(stats(&env).workers["hub"].published, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn visitor_sse_headers_never_reach_the_guest_and_a_worker_without_sse_is_unchanged() {
    let env = standard_env().await;
    let forged = [
        ("x-edger-sse-subscribe", "room.1"),
        ("X-Edger-SSE-Publish", "e30"),
        ("x-edger-sse-whatever", "1"),
    ];

    // Every route kind of a worker with `sse`.
    for (method, path) in [
        ("GET", "/hub/events"),
        ("POST", "/hub/publish"),
        ("GET", "/hub/plain"),
    ] {
        // No credential on /events: the guest answers 401, which keeps the
        // request a finite one while still showing what the guest saw.
        let headers = forged.to_vec();
        request(env.addr, method, path, &headers).await;
    }
    for run in guest_requests(&env, "hub") {
        assert!(
            run.headers
                .iter()
                .all(|(name, _)| !name.to_ascii_lowercase().starts_with("x-edger-sse-")),
            "{} {} saw {:?}",
            run.method,
            run.uri,
            run.headers
        );
    }
    assert_eq!(guest_requests(&env, "hub").len(), 3);

    // A worker WITHOUT `sse`: zero change, in both directions.
    let mut headers = forged.to_vec();
    headers.push(("x-sub", "room.1"));
    let response = request(env.addr, "GET", "/plain/open", &headers).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("x-edger-sse-subscribe"), Some("room.1"));
    let runs = guest_requests(&env, "plain");
    assert_eq!(runs.len(), 1);
    for (name, _) in forged {
        assert!(
            runs[0]
                .headers
                .iter()
                .any(|(seen, _)| seen.eq_ignore_ascii_case(name)),
            "{name} must still reach a worker without sse"
        );
    }
    assert_eq!(stats(&env).total_connections, 0);
}

// ---------------------------------------------------------------------------
// Capacity, availability, compression
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_app_answers_503_capacity_and_the_existing_subscriber_is_intact() {
    let env = standard_env().await;
    let mut first = subscribe(&env, "/tiny/events").await;

    let refused = request(env.addr, "GET", "/tiny/events", &[AUTH]).await;
    assert_eq!(refused.status, 503);
    assert_eq!(refused.json()["code"], "SSE_CAPACITY");
    assert_eq!(refused.header("retry-after"), Some("5"));
    refused.assert_no_sse_headers();
    assert_eq!(stats(&env).workers["tiny"].rejected_capacity, 1);

    publish(&env, "/tiny/publish", &[event_json("room.1", "still-here")]).await;
    first.wait_for("data: still-here\n\n").await;

    // Another app is not charged to this one.
    let _hub = subscribe(&env, "/hub/events").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_broker_answers_unavailable_and_publish_routes_discard_directives() {
    let disabled = start(vec![fetch_worker("hub", "")], Broker::With(quiet_limits(0))).await;
    let absent = start(vec![fetch_worker("hub", "")], Broker::Absent).await;
    for env in [&disabled, &absent] {
        let response = request(env.addr, "GET", "/hub/events", &[AUTH]).await;
        assert_eq!(response.status, 503);
        assert_eq!(response.json()["code"], "SSE_UNAVAILABLE");
        assert_eq!(response.header("retry-after"), None);
        response.assert_no_sse_headers();

        // The publish route still delivers the normal response; even an
        // invalid directive is discarded, never a 502.
        for directive in [
            ("x-pub-json", event_json("room.1", "x")),
            ("x-pub-raw", "garbage!".to_string()),
        ] {
            let response = request(
                env.addr,
                "POST",
                "/hub/publish",
                &[(directive.0, directive.1.as_str())],
            )
            .await;
            assert_eq!(response.status, 200, "{}", response.text());
            assert_eq!(response.json(), serde_json::json!({"ok": true}));
            response.assert_no_sse_headers();
        }
    }
    let snapshot = stats(&disabled);
    assert_eq!(snapshot.workers["hub"].rejected_unavailable, 1);
    assert_eq!(snapshot.total_connections, 0);
    assert!(absent.server.sse_broker().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_subscription_is_never_compressed_and_the_neighbors_still_are_ordinary() {
    let env = standard_env().await;
    let mut client = open_sse(
        env.addr,
        "/hub/events",
        &[AUTH, ("accept-encoding", "br, gzip")],
    )
    .await;
    assert_eq!(client.head.status, 200);
    assert_eq!(client.head.header("content-encoding"), None);
    assert_eq!(
        client.head.header("content-type"),
        Some("text/event-stream")
    );
    publish(&env, "/hub/publish", &[event_json("room.1", "plain-text")]).await;
    // The frame is readable as is: no encoding in the way.
    client.wait_for("data: plain-text\n\n").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fullstack_asset_on_a_declared_route_is_served_by_the_host_without_the_guest() {
    let env = standard_env().await;
    let asset = request(env.addr, "GET", "/fshub/static/app.js", &[AUTH]).await;
    assert_eq!(asset.status, 200, "{}", asset.text());
    assert_eq!(asset.text(), "console.log('asset');\n");
    asset.assert_no_sse_headers();
    assert!(
        guest_requests(&env, "fshub").is_empty(),
        "the host served the asset: no guest ran"
    );
    assert_eq!(stats(&env).total_connections, 0);

    // The guest-backed route of the same worker still subscribes.
    let mut client = subscribe(&env, "/fshub/events").await;
    publish(&env, "/fshub/publish", &[event_json("room.1", "ssr")]).await;
    client.wait_for("data: ssr\n\n").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_requests_to_a_worker_with_sse_are_unchanged() {
    let env = standard_env().await;
    let _held = [
        subscribe(&env, "/hub/events").await,
        subscribe(&env, "/hub/events").await,
        subscribe(&env, "/hub/events").await,
    ];
    // Three subscriptions open: an ordinary request still answers at once,
    // so no worker slot is held per browser.
    let response = request(env.addr, "GET", "/hub/anything", &[("x-body-size", "10")]).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"xxxxxxxxxx");
    assert_eq!(response.header("x-custom"), Some("kept"));
}

// ---------------------------------------------------------------------------
// /metrics
// ---------------------------------------------------------------------------

async fn metrics(env: &Env) -> String {
    let response = request(
        env.addr,
        "GET",
        "/metrics",
        &[("authorization", "Bearer test-root")],
    )
    .await;
    assert_eq!(response.status, 200, "{}", response.text());
    response.text()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_expose_the_four_sse_series_with_the_expected_values() {
    let env = standard_env().await;
    let first = subscribe(&env, "/hub/events").await;
    let mut second = subscribe(&env, "/hub/events").await;
    let _tiny = subscribe(&env, "/tiny/events").await;
    assert_eq!(
        request(env.addr, "GET", "/tiny/events", &[AUTH])
            .await
            .status,
        503
    );
    for n in 0..3 {
        publish(
            &env,
            "/hub/publish",
            &[event_json("room.1", &format!("e{n}"))],
        )
        .await;
    }
    second.wait_for("data: e2\n\n").await;

    let text = metrics(&env).await;
    for expected in [
        "edger_sse_connections{worker=\"hub\"} 2",
        "edger_sse_connections{worker=\"tiny\"} 1",
        "edger_sse_events_published_total{worker=\"hub\"} 3",
        "edger_sse_disconnects_total{worker=\"hub\",reason=\"client\"} 0",
        "edger_sse_disconnects_total{worker=\"hub\",reason=\"ttl\"} 0",
        "edger_sse_disconnects_total{worker=\"hub\",reason=\"lagged\"} 0",
        "edger_sse_disconnects_total{worker=\"hub\",reason=\"shutdown\"} 0",
        "edger_sse_rejected_total{worker=\"tiny\",reason=\"capacity\"} 1",
        "edger_sse_rejected_total{worker=\"tiny\",reason=\"unavailable\"} 0",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    for header in [
        "# TYPE edger_sse_connections gauge",
        "# TYPE edger_sse_events_published_total counter",
        "# TYPE edger_sse_disconnects_total counter",
        "# TYPE edger_sse_rejected_total counter",
    ] {
        assert!(text.contains(header), "missing {header:?}");
    }

    // A client that goes away is counted once its stream is dropped.
    drop(first);
    let deadline = tokio::time::Instant::now() + STEP;
    loop {
        let text = metrics(&env).await;
        if text.contains("edger_sse_connections{worker=\"hub\"} 1")
            && text.contains("edger_sse_disconnects_total{worker=\"hub\",reason=\"client\"} 1")
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the dropped client was not counted:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_broker_metrics_have_no_sse_line() {
    let env = start(vec![fetch_worker("hub", "")], Broker::Absent).await;
    let text = metrics(&env).await;
    assert!(!text.contains("edger_sse_"), "{text}");
}

// ---------------------------------------------------------------------------
// A real Deno guest whose response is cut at EDGER_STREAM_MAX_BYTES
// ---------------------------------------------------------------------------

/// The real persistent Deno process isolate, with the stream detach pipeline
/// on or off (`EDGER_STREAM_DETACH_MAX_BYTES=0` is the legacy reader).
struct DenoFactory {
    cache: std::path::PathBuf,
    detach: bool,
}

impl IsolateFactory for DenoFactory {
    fn create_isolate(&self, _worker: &WorkerRef) -> Box<dyn Isolate> {
        let isolate = DenoProcessIsolate::new().with_per_worker_deno_cache_root(self.cache.clone());
        Box::new(if self.detach {
            isolate.with_stream_detach(1 << 20, Arc::new(StreamDetachBudget::new(1 << 24)))
        } else {
            isolate
        })
    }
}

/// A body the harness cuts at the byte cap is a clean end for an ordinary
/// client, but it is not a complete execution: neither a subscription nor a
/// publish may take effect from it. A response under the cap is the control.
/// `ephemeral` runs the worker with `ttl: 0`: its answer is buffered by the
/// pool (no stream), a branch with its own truncation mark.
async fn a_cut_response_never_authorizes_or_publishes(detach: bool, ephemeral: bool) {
    let cap = 2048;
    let publish_value = URL_SAFE_NO_PAD.encode(r#"{"topic":"room.1","data":"from-deno"}"#);
    let source = format!(
        r#"Deno.serve((req) => {{
  const url = new URL(req.url);
  const size = url.searchParams.get("big") === "1" ? 8192 : 16;
  const headers = new Headers({{ "content-type": "application/octet-stream" }});
  if (url.pathname === "/events") headers.set("x-edger-sse-subscribe", "room.1");
  if (url.pathname === "/publish") headers.set("x-edger-sse-publish", "{publish_value}");
  const body = new ReadableStream({{
    start(c) {{ c.enqueue(new Uint8Array(size).fill(120)); c.close(); }},
  }});
  return new Response(body, {{ status: 200, headers }});
}});
"#
    );
    let worker = Files {
        dir: "cut",
        files: vec![
            (
                "manifest.yaml",
                fetch_manifest(
                    "cut",
                    &format!(
                        "{}env:\n  EDGER_STREAM_MAX_BYTES: \"{cap}\"\n",
                        if ephemeral { "ttl: 0\n" } else { "" }
                    ),
                ),
            ),
            ("index.ts", source),
        ],
    };
    let env = start_with(vec![worker], Broker::With(quiet_limits(1024)), |root, _| {
        Arc::new(DenoFactory {
            cache: root.join(".edger-test-deno-cache"),
            detach,
        })
    })
    .await;

    // Control under the cap: the subscription opens and a publish is delivered.
    let mut client = open_sse(env.addr, "/cut/events", &[]).await;
    assert_eq!(client.head.status, 200, "{:?}", client.head.headers);
    assert_eq!(
        client.head.header("content-type"),
        Some("text/event-stream")
    );
    let response = request(env.addr, "POST", "/cut/publish", &[]).await;
    assert_eq!(response.status, 200, "{}", response.text());
    response.assert_no_sse_headers();
    client.wait_for("data: from-deno\n\n").await;
    assert_eq!(stats(&env).workers["cut"].published, 1);
    assert_eq!(stats(&env).total_connections, 1);

    // A cut subscribe response does not subscribe.
    let response = request(env.addr, "GET", "/cut/events?big=1", &[]).await;
    assert!(
        response.status >= 500,
        "{}: {}",
        response.status,
        response.text()
    );
    assert_eq!(response.json()["code"], "WORKER_ERROR");
    response.assert_no_sse_headers();
    assert_eq!(stats(&env).total_connections, 1, "a connection was opened");

    // A cut publish response publishes nothing.
    let response = request(env.addr, "POST", "/cut/publish?big=1", &[]).await;
    assert!(
        response.status >= 500,
        "{}: {}",
        response.status,
        response.text()
    );
    assert_eq!(response.json()["code"], "WORKER_ERROR");
    response.assert_no_sse_headers();
    assert!(!client.drain_quiet().await.contains("data:"));
    assert_eq!(stats(&env).workers["cut"].published, 1, "an event leaked");

    // The worker still serves the controls afterwards.
    let again = open_sse(env.addr, "/cut/events", &[]).await;
    assert_eq!(again.head.status, 200);
    let response = request(env.addr, "POST", "/cut/publish", &[]).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(stats(&env).workers["cut"].published, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH (EDGER_DENO_BIN); run with --ignored"]
async fn a_response_cut_at_the_stream_cap_never_authorizes_or_publishes_with_detach() {
    a_cut_response_never_authorizes_or_publishes(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH (EDGER_DENO_BIN); run with --ignored"]
async fn a_response_cut_at_the_stream_cap_never_authorizes_or_publishes_without_detach() {
    a_cut_response_never_authorizes_or_publishes(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH (EDGER_DENO_BIN); run with --ignored"]
async fn a_response_cut_at_the_stream_cap_never_authorizes_or_publishes_ephemeral_with_detach() {
    a_cut_response_never_authorizes_or_publishes(true, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH (EDGER_DENO_BIN); run with --ignored"]
async fn a_response_cut_at_the_stream_cap_never_authorizes_or_publishes_ephemeral_without_detach() {
    a_cut_response_never_authorizes_or_publishes(false, true).await;
}
