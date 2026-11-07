//! Managed database seam of the Deno process backend (EDG-54 / 15a).
//!
//! What this proves: the private socket, the origin handed by the HOST, and
//! the bootstrap `fetch` forwarding, against a REAL Deno process and a SPY
//! sink on the guard socket. The spy is NOT the product `EgressGuard`: it
//! proves neither real SQLD traffic nor credential injection (the guard that
//! imposes namespace/token is a later slice).
//!
//! The Deno tests are `#[ignore]` (they need `deno`; point `EDGER_DENO_BIN`
//! at it) and FAIL, not skip, when `deno` is missing and `--ignored` is
//! asked for.

#![cfg(all(feature = "multiproc", unix))]

use std::collections::HashMap;
use std::future::Future;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::time::Duration;

use edger_core::{
    parse_worker_config, Isolate, SerializedRequest, SerializedResponse, WorkerConfig,
    WorkerManifest,
};
use edger_isolation::egress::default_egress_limits;
use edger_isolation::{
    DenoProcessIsolate, EgressListenerContext, EgressListenerSink, EgressServeFuture,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Spy sink: a tiny HTTP/1.1 server on the guard socket
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Record {
    method: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Record {
    fn header_all(&self, name: &str) -> Vec<String> {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .collect()
    }
    fn target(&self) -> String {
        let all = self.header_all("x-edger-egress-url");
        assert_eq!(
            all.len(),
            1,
            "exactly one target header: {:?}",
            self.headers
        );
        all[0].clone()
    }
}

/// The connection tasks of ONE served socket: aborted when the serve future
/// is dropped (process ended) and, in the failure path, aborted + awaited by
/// the test before the child is terminated.
#[derive(Default)]
struct TaskBag(Mutex<Vec<JoinHandle<()>>>);

impl TaskBag {
    fn push(&self, handle: JoinHandle<()>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handle);
    }
    async fn abort_and_await(&self) {
        let handles: Vec<_> =
            std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        for handle in &handles {
            handle.abort();
        }
        for handle in handles {
            let _ = handle.await;
        }
    }
}

impl Drop for TaskBag {
    fn drop(&mut self) {
        for handle in self.0.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            handle.abort();
        }
    }
}

#[derive(Default)]
struct Spy {
    live: AtomicUsize,
    serves: AtomicUsize,
    aborted: AtomicUsize,
    records: Mutex<Vec<Record>>,
    contexts: Mutex<Vec<EgressListenerContext>>,
    socket_paths: Mutex<Vec<PathBuf>>,
    bags: Mutex<Vec<Arc<TaskBag>>>,
}

struct LiveToken(Arc<Spy>);

impl Drop for LiveToken {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Spy {
    fn records(&self) -> Vec<Record> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    async fn abort_all_tasks(&self) {
        let bags: Vec<_> = self
            .bags
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for bag in bags {
            bag.abort_and_await().await;
        }
    }
}

struct SpySink(Arc<Spy>);

impl EgressListenerSink for SpySink {
    fn serve(&self, listener: UnixListener, context: EgressListenerContext) -> EgressServeFuture {
        let spy = Arc::clone(&self.0);
        spy.serves.fetch_add(1, Ordering::SeqCst);
        spy.live.fetch_add(1, Ordering::SeqCst);
        let token = LiveToken(Arc::clone(&spy));
        spy.contexts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(context);
        if let Ok(addr) = listener.local_addr() {
            if let Some(path) = addr.as_pathname() {
                spy.socket_paths
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(path.to_path_buf());
            }
        }
        let bag = Arc::new(TaskBag::default());
        spy.bags
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&bag));
        Box::pin(async move {
            let _token = token;
            while let Ok((stream, _)) = listener.accept().await {
                let spy = Arc::clone(&spy);
                bag.push(tokio::spawn(
                    async move { handle_connection(spy, stream).await },
                ));
            }
        })
    }
}

async fn read_until(stream: &mut UnixStream, buf: &mut Vec<u8>, needle: &[u8]) -> Option<usize> {
    loop {
        if let Some(pos) = buf.windows(needle.len()).position(|w| w == needle) {
            return Some(pos);
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn fill(stream: &mut UnixStream, buf: &mut Vec<u8>, len: usize) -> Option<()> {
    while buf.len() < len {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(())
}

async fn read_request(stream: &mut UnixStream, buf: &mut Vec<u8>) -> Option<Record> {
    let head_end = read_until(stream, buf, b"\r\n\r\n").await?;
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    buf.drain(..head_end + 4);
    let mut lines = head.split("\r\n");
    let method = lines.next()?.split(' ').next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let find = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let mut body = Vec::new();
    if find("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        loop {
            let end = read_until(stream, buf, b"\r\n").await?;
            let size =
                usize::from_str_radix(String::from_utf8_lossy(&buf[..end]).trim(), 16).ok()?;
            buf.drain(..end + 2);
            fill(stream, buf, size + 2).await?;
            body.extend_from_slice(&buf[..size]);
            buf.drain(..size + 2);
            if size == 0 {
                break;
            }
        }
    } else if let Some(len) = find("content-length").and_then(|v| v.parse::<usize>().ok()) {
        fill(stream, buf, len).await?;
        body = buf.drain(..len).collect();
    }
    Some(Record {
        method,
        headers,
        body,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn handle_connection(spy: Arc<Spy>, mut stream: UnixStream) {
    let mut buf = Vec::new();
    while let Some(record) = read_request(&mut stream, &mut buf).await {
        let target = record
            .header_all("x-edger-egress-url")
            .first()
            .cloned()
            .unwrap_or_default();
        spy.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(record.clone());
        let path = target
            .split_once("//")
            .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_string()))
            .unwrap_or_default();
        match path.as_str() {
            "/slow" => {
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n3\r\none\r\n")
                    .await;
                tokio::time::sleep(Duration::from_millis(600)).await;
                let _ = stream.write_all(b"3\r\ntwo\r\n0\r\n\r\n").await;
            }
            "/hang" => {
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n2\r\nhi\r\n")
                    .await;
                // The client abort closes the connection: a read returns 0.
                let mut sink = [0u8; 64];
                loop {
                    match stream.read(&mut sink).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
                spy.aborted.fetch_add(1, Ordering::SeqCst);
                return;
            }
            "/deny" => {
                let _ = stream
                    .write_all(b"HTTP/1.1 403 Forbidden\r\nx-edger-egress-error: blocked\r\ncontent-length: 6\r\n\r\ndenied")
                    .await;
            }
            "/redir" => {
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nlocation: /elsewhere\r\ncontent-length: 0\r\n\r\n",
                    )
                    .await;
            }
            _ => {
                let echo = serde_json::json!({
                    "method": record.method,
                    "target": target,
                    "bodyHex": hex(&record.body),
                    "bodyText": String::from_utf8_lossy(&record.body),
                })
                .to_string();
                let out = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{echo}",
                    echo.len()
                );
                let _ = stream.write_all(out.as_bytes()).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

const WORKER: &str = r#"
const db = Deno.env.get("DB_ORIGIN") ?? Deno.env.get("SQLD_URL");
const other = Deno.env.get("OTHER_PORT");
const enc = new TextEncoder();
async function out(r) {
  return { status: r.status, redirected: r.redirected, err: r.headers.get("x-edger-egress-error"), text: await r.text() };
}
async function failure(f) {
  try { await f(); return "no-error"; } catch (e) { return e.name; }
}
Deno.serve(async (req) => {
  const p = new URL(req.url).pathname;
  switch (p) {
    case "/string":
      return Response.json(await out(await fetch(db + "/s?x=1#frag", {
        method: "POST",
        headers: { "x-edger-egress-url": "http://evil.example/", "x-app": "a" },
        body: new Uint8Array([0, 1, 2, 253, 254, 255]),
      })));
    case "/url":
      return Response.json(await out(await fetch(new URL(db + "/u"), { method: "PUT", body: "abc" })));
    case "/request": {
      const a = await fetch(new Request(db + "/r", { method: "POST", body: new Uint8Array([9, 8, 7]), headers: { "x-req": "1" } }), { headers: { "x-init": "2" } });
      const b = await fetch(new Request(db + "/r2", { method: "PUT", body: "hello", headers: { "x-req": "1" } }));
      return Response.json([await out(a), await out(b)]);
    }
    case "/norm": {
      const u = new URL(db);
      return Response.json(await out(await fetch("HTTP://" + u.host.toUpperCase() + "//dir/./a/../b")));
    }
    case "/stream-body": {
      const body = new ReadableStream({ start(c) { for (const s of ["aa", "bb", "cc"]) c.enqueue(enc.encode(s)); c.close(); } });
      return Response.json(await out(await fetch(db + "/sb", { method: "POST", body, duplex: "half" })));
    }
    case "/resp-stream": {
      const r = await fetch(db + "/slow");
      const reader = r.body.getReader();
      const first = await reader.read();
      const t1 = performance.now();
      const second = await reader.read();
      const t2 = performance.now();
      const done = await reader.read();
      const dec = new TextDecoder();
      return Response.json({ first: dec.decode(first.value), second: dec.decode(second.value), done: done.done, gap: t2 - t1 });
    }
    case "/abort": {
      const ac = new AbortController();
      const r = await fetch(db + "/hang", { signal: ac.signal });
      const pending = r.body.getReader().read().then(() => "read", (e) => e.name);
      ac.abort();
      return Response.json({ name: await pending, signalAborted: ac.signal.aborted });
    }
    case "/client": {
      const c = Deno.createHttpClient({});
      return Response.json(await out(await fetch(db + "/c", { client: c })));
    }
    case "/redir":
      return Response.json(await out(await fetch(db + "/redir")));
    case "/deny":
      return Response.json(await out(await fetch(db + "/deny")));
    case "/nondb": {
      const u = new URL(db);
      return Response.json({
        otherPort: await failure(() => fetch("http://" + u.hostname + ":" + other + "/")),
        otherScheme: await failure(() => fetch("https://" + u.host + "/")),
        otherHost: await failure(() => fetch("http://localhost:" + u.port + "/")),
        external: await failure(() => fetch("https://example.com/")),
        connect: await failure(() => Deno.connect({ hostname: u.hostname, port: Number(u.port) })),
        connectOther: await failure(() => Deno.connect({ hostname: u.hostname, port: Number(other) })),
      });
    }
    case "/nondb-request": {
      const target = "http://127.0.0.1:" + other + "/";
      const observe = async (make, init) => {
        const rq = make();
        let name = "no-error";
        let message = "";
        try { await (init === undefined ? fetch(rq) : fetch(rq, init)); } catch (e) { name = e.name; message = String(e.message); }
        return { name, message, bodyUsed: rq.bodyUsed, method: rq.method, url: rq.url };
      };
      const enc2 = new TextEncoder();
      return Response.json({
        plain: await observe(() => new Request(target, { method: "POST", body: "abc" })),
        merged: await observe(() => new Request(target, { method: "POST", body: "abc", headers: { "x-req": "1" } }), { headers: { "x-init": "2" }, body: "zzz", method: "POST" }),
        initOnly: await observe(() => new Request(target, { method: "PUT", headers: { "x-req": "1" } }), { headers: { "x-init": "2" } }),
        streamed: await observe(() => new Request(target, { method: "POST", duplex: "half", body: new ReadableStream({ start(c) { c.enqueue(enc2.encode("s")); c.close(); } }) })),
      });
    }
    case "/direct":
      return Response.json({ name: await failure(() => fetch(db + "/direct")) });
    case "/ping":
      return Response.json(await out(await fetch(db + "/ping")));
    default:
      return new Response("not found", { status: 404 });
  }
});
"#;

fn require_deno() {
    let exe = std::env::var("EDGER_DENO_BIN").unwrap_or_else(|_| "deno".into());
    let out = std::process::Command::new(&exe)
        .arg("--version")
        .output()
        .unwrap_or_else(|err| panic!("real Deno is required (set EDGER_DENO_BIN): {exe}: {err}"));
    assert!(out.status.success(), "`{exe} --version` failed");
}

struct Fixture {
    dir: tempfile::TempDir,
    cache: tempfile::TempDir,
    spy: Arc<Spy>,
    isolate: DenoProcessIsolate,
    config: WorkerConfig,
    db_port: u16,
    other_port: u16,
    /// Held so nothing else can take the ports; nothing may connect.
    db_tcp: TcpListener,
    other_tcp: TcpListener,
}

impl Fixture {
    async fn new(allow_net: Option<Vec<String>>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.ts"), WORKER).unwrap();
        let db_tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other_tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let db_port = db_tcp.local_addr().unwrap().port();
        let other_port = other_tcp.local_addr().unwrap().port();
        let mut config = parse_worker_config(&WorkerManifest {
            name: "managed-worker".into(),
            entrypoint: Some("index.ts".into()),
            allow_net,
            ..WorkerManifest::default()
        });
        config.worker_dir = Some(dir.path().to_path_buf());
        config.timeout_ms = 20_000;
        config.env = HashMap::from([
            (
                "DB_ORIGIN".to_string(),
                format!("http://127.0.0.1:{db_port}"),
            ),
            ("OTHER_PORT".to_string(), other_port.to_string()),
        ]);
        let isolate = DenoProcessIsolate::new().with_per_worker_deno_cache_root(cache.path());
        Self {
            dir,
            cache,
            spy: Arc::new(Spy::default()),
            isolate,
            config,
            db_port,
            other_port,
            db_tcp,
            other_tcp,
        }
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.db_port)
    }

    fn arm(&mut self) {
        let sink: Arc<dyn EgressListenerSink> = Arc::new(SpySink(Arc::clone(&self.spy)));
        let origin = self.origin();
        self.isolate
            .set_managed_database_egress(
                sink,
                "managed-worker",
                "1.0.0",
                Some("ns1".into()),
                origin,
            )
            .expect("arm managed database");
    }

    async fn get(&mut self, path: &str) -> SerializedResponse {
        let request = SerializedRequest {
            method: "GET".into(),
            uri: path.into(),
            headers: vec![],
            body: None,
            request_id: "managed-test".into(),
            base_href: None,
        };
        let config = self.config.clone();
        self.isolate
            .execute_fetch(request, &config)
            .await
            .unwrap_or_else(|err| panic!("{path}: {err:?}"))
    }

    async fn json(&mut self, path: &str) -> serde_json::Value {
        let response = self.get(path).await;
        assert_eq!(response.status, 200, "{path}");
        serde_json::from_slice(response.body.as_deref().unwrap_or_default())
            .unwrap_or_else(|err| panic!("{path}: not json ({err}): {:?}", response.body))
    }

    /// No TCP connection ever reached the database or the other port.
    async fn assert_no_tcp(&self) {
        for listener in [&self.db_tcp, &self.other_tcp] {
            assert!(
                tokio::time::timeout(Duration::from_millis(150), listener.accept())
                    .await
                    .is_err(),
                "a direct TCP connection reached the target"
            );
        }
    }
}

type Scenario = for<'a> fn(&'a mut Fixture) -> Pin<Box<dyn Future<Output = ()> + 'a>>;

/// Poll a future so a panic inside it is CAUGHT, not unwound through the
/// test: the cleanup below still runs.
struct CatchPanic<F>(Pin<Box<F>>);

impl<F: Future<Output = ()>> Future for CatchPanic<F> {
    type Output = std::thread::Result<()>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match catch_unwind(AssertUnwindSafe(|| self.0.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(())) => Poll::Ready(Ok(())),
            Err(payload) => Poll::Ready(Err(payload)),
        }
    }
}

/// Runs a scenario with a deadline and a catch. Whatever happens (pass,
/// panic, timeout) the spy's own tasks are aborted and AWAITED first, then
/// the child is terminated; only then is the failure re-raised. Nothing is
/// left detached.
async fn run_guarded(fx: &mut Fixture, scenario: Scenario) {
    let outcome =
        tokio::time::timeout(Duration::from_secs(120), CatchPanic(Box::pin(scenario(fx)))).await;
    fx.spy.abort_all_tasks().await;
    let terminated = fx.isolate.terminate().await;
    // The workdirs of the fixture are removed with it (`dir`, `cache`).
    let _ = (&fx.dir, &fx.cache);
    match outcome {
        Err(_) => panic!("scenario timed out after 120s"),
        Ok(Err(payload)) => resume_unwind(payload),
        Ok(Ok(())) => terminated.expect("terminate"),
    }
}

async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn pid_of(process_id: &str) -> u32 {
    process_id.split('-').next().unwrap().parse().unwrap()
}

// ---------------------------------------------------------------------------
// No Deno needed: refusals happen before any spawn
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_spawn_without_empty_allow_net_is_refused_before_spawn() {
    let general = default_egress_limits();
    for allow in [None, Some(vec!["example.com".to_string()])] {
        for declared_egress in [false, true] {
            let mut fx = Fixture::new(allow.clone()).await;
            if declared_egress {
                fx.config.egress = Some(general.clone());
            }
            fx.arm();
            let request = SerializedRequest {
                method: "GET".into(),
                uri: "/ping".into(),
                headers: vec![],
                body: None,
                request_id: "refuse".into(),
                base_href: None,
            };
            let config = fx.config.clone();
            let err = fx
                .isolate
                .execute_fetch(request, &config)
                .await
                .expect_err("refused");
            assert_eq!(
                err.code, "UDS_MANAGED_DATABASE_NET",
                "{allow:?} egress={declared_egress}"
            );
            assert!(!fx.isolate.has_live_process());
            assert_eq!(
                fx.spy.serves.load(Ordering::SeqCst),
                0,
                "no socket was served"
            );
            fx.isolate.terminate().await.unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_state_is_private_set_clear_and_validated() {
    let mut fx = Fixture::new(Some(vec![])).await;
    assert!(!fx.isolate.has_live_process());
    let sink: Arc<dyn EgressListenerSink> = Arc::new(SpySink(Arc::clone(&fx.spy)));
    for bad in ["", "http://a b", "http://a\nb"] {
        assert!(fx
            .isolate
            .set_managed_database_egress(Arc::clone(&sink), "w", "1", None, bad)
            .is_err());
    }
    fx.arm();
    fx.isolate.clear_managed_database_egress().unwrap();
    // Cleared: a declared-allowNet worker with no managed state spawns the
    // old way (this one is refused only by Deno being absent or not).
    fx.arm();
    fx.isolate.terminate().await.unwrap();
}

// ---------------------------------------------------------------------------
// Real Deno + spy
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs real deno (EDGER_DENO_BIN); run explicitly with --ignored"]
async fn managed_fetch_forms_streams_and_negatives_through_the_private_socket() {
    require_deno();
    let mut fx = Fixture::new(Some(vec![])).await;
    fx.arm();
    run_guarded(&mut fx, |fx| Box::pin(forms_scenario(fx))).await;
}

async fn forms_scenario(fx: &mut Fixture) {
    let origin = fx.origin();

    // string + app-supplied target header + fragment + binary body
    let v = fx.json("/string").await;
    assert_eq!(v["status"], 200, "{v}");
    let echo: serde_json::Value = serde_json::from_str(v["text"].as_str().unwrap()).unwrap();
    assert_eq!(echo["method"], "POST");
    assert_eq!(echo["bodyHex"], "000102fdfeff", "binary body is byte exact");
    assert_eq!(
        echo["target"],
        format!("{origin}/s?x=1"),
        "real URL wins, no fragment"
    );
    let rec = fx.spy.records().last().cloned().unwrap();
    assert_eq!(rec.target(), format!("{origin}/s?x=1"));
    assert_eq!(rec.header_all("x-app"), ["a"]);
    assert!(!rec.target().contains("evil"));

    // URL object
    let v = fx.json("/url").await;
    let echo: serde_json::Value = serde_json::from_str(v["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        (echo["method"].as_str(), echo["bodyText"].as_str()),
        (Some("PUT"), Some("abc"))
    );
    assert_eq!(echo["target"], format!("{origin}/u"));

    // Request + init, and a bare Request
    let v = fx.json("/request").await;
    let a: serde_json::Value = serde_json::from_str(v[0]["text"].as_str().unwrap()).unwrap();
    let b: serde_json::Value = serde_json::from_str(v[1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        (a["method"].as_str(), a["bodyHex"].as_str()),
        (Some("POST"), Some("090807"))
    );
    assert_eq!(a["target"], format!("{origin}/r"));
    assert_eq!(
        (b["method"].as_str(), b["bodyText"].as_str()),
        (Some("PUT"), Some("hello"))
    );
    let records = fx.spy.records();
    let ra = records.iter().find(|r| r.target().ends_with("/r")).unwrap();
    let rb = records
        .iter()
        .find(|r| r.target().ends_with("/r2"))
        .unwrap();
    assert_eq!(ra.header_all("x-init"), ["2"], "init headers are kept");
    assert_eq!(rb.header_all("x-req"), ["1"], "Request headers are kept");

    // normalized origin (scheme/host case, dot segments) is still the database
    let v = fx.json("/norm").await;
    let echo: serde_json::Value = serde_json::from_str(v["text"].as_str().unwrap()).unwrap();
    assert_eq!(echo["target"], format!("{origin}//dir/b"));

    // ReadableStream request body
    let v = fx.json("/stream-body").await;
    let echo: serde_json::Value = serde_json::from_str(v["text"].as_str().unwrap()).unwrap();
    assert_eq!(echo["bodyText"], "aabbcc");

    // response streaming: the first chunk arrives before the spy ends it
    let v = fx.json("/resp-stream").await;
    assert_eq!(
        (
            v["first"].as_str(),
            v["second"].as_str(),
            v["done"].as_bool()
        ),
        (Some("one"), Some("two"), Some(true))
    );
    assert!(
        v["gap"].as_f64().unwrap() >= 300.0,
        "the response was buffered, not streamed: {v}"
    );

    // abort signal closes the upstream connection
    let v = fx.json("/abort").await;
    assert_eq!(v["name"], "AbortError", "{v}");
    wait_until("the spy sees the client disconnect", || {
        fx.spy.aborted.load(Ordering::SeqCst) == 1
    })
    .await;

    // an app-made client cannot bypass the private one
    let before = fx.spy.records().len();
    let v = fx.json("/client").await;
    assert_eq!(v["status"], 200, "{v}");
    assert_eq!(fx.spy.records().len(), before + 1);

    // guard refusals stay refusals; redirects are not followed
    let v = fx.json("/deny").await;
    assert_eq!(
        (v["status"].as_i64(), v["err"].as_str()),
        (Some(403), Some("blocked"))
    );
    let before = fx.spy.records().len();
    let v = fx.json("/redir").await;
    assert_eq!(
        (v["status"].as_i64(), v["redirected"].as_bool()),
        (Some(302), Some(false))
    );
    assert_eq!(
        fx.spy.records().len(),
        before + 1,
        "the redirect was not followed"
    );

    // non-database fetch and a direct connect stay NotCapable under allowNet []
    let before = fx.spy.records().len();
    let v = fx.json("/nondb").await;
    for key in [
        "otherPort",
        "otherScheme",
        "otherHost",
        "external",
        "connect",
        "connectOther",
    ] {
        assert_eq!(v[key], "NotCapable", "{key}: {v}");
    }
    assert_eq!(
        fx.spy.records().len(),
        before,
        "non-database calls never reached the guard"
    );
    fx.assert_no_tcp().await;

    // identity, no authority, finite limits (spy, not the product guard)
    let contexts = fx.spy.contexts.lock().unwrap().clone();
    assert_eq!(contexts.len(), 1, "one process, one socket");
    let ctx = &contexts[0];
    assert_eq!(
        (
            ctx.worker.as_str(),
            ctx.version.as_str(),
            ctx.namespace.as_deref()
        ),
        ("managed-worker", "1.0.0", Some("ns1"))
    );
    assert!(
        ctx.settings.is_none(),
        "no egress declared: limits are not a grant"
    );
    assert_eq!(ctx.limits, default_egress_limits());
    assert!(ctx.limits.max_concurrent > 0 && ctx.limits.total_timeout_ms > 0);
    assert!(!ctx.process_id.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs real deno (EDGER_DENO_BIN); run explicitly with --ignored"]
async fn guest_env_or_headers_never_arm_the_seam() {
    require_deno();
    // No host setter: SQLD_URL in the manifest env and an `x-egress-url`
    // header arm nothing, so the database fetch is just a denied native one.
    let mut fx = Fixture::new(Some(vec![])).await;
    let origin = fx.origin();
    fx.config.env.remove("DB_ORIGIN");
    fx.config.env.insert("SQLD_URL".into(), origin);
    run_guarded(&mut fx, |fx| {
        Box::pin(async move {
            let request = SerializedRequest {
                method: "GET".into(),
                uri: "/direct".into(),
                headers: vec![("x-egress-url".into(), "http://sqld".into())],
                body: None,
                request_id: "unarmed".into(),
                base_href: None,
            };
            let config = fx.config.clone();
            let response = fx.isolate.execute_fetch(request, &config).await.unwrap();
            let v: serde_json::Value =
                serde_json::from_slice(response.body.as_deref().unwrap()).unwrap();
            assert_eq!(v["name"], "NotCapable", "{v}");
            assert_eq!(fx.spy.serves.load(Ordering::SeqCst), 0);
            assert!(fx.spy.records().is_empty());
            // Without the 4th argument there is no guard socket variable.
            fx.assert_no_tcp().await;
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs real deno (EDGER_DENO_BIN); run explicitly with --ignored"]
async fn invalid_origin_fails_closed_without_echoing_it() {
    require_deno();
    let mut fx = Fixture::new(Some(vec![])).await;
    let sink: Arc<dyn EgressListenerSink> = Arc::new(SpySink(Arc::clone(&fx.spy)));
    fx.isolate
        .set_managed_database_egress(sink, "managed-worker", "1.0.0", None, "zz-secret-origin")
        .unwrap();
    run_guarded(&mut fx, |fx| {
        Box::pin(async move {
            let request = SerializedRequest {
                method: "GET".into(),
                uri: "/ping".into(),
                headers: vec![],
                body: None,
                request_id: "bad-origin".into(),
                base_href: None,
            };
            let config = fx.config.clone();
            let err = fx
                .isolate
                .execute_fetch(request, &config)
                .await
                .expect_err("fails closed");
            let text = format!("{err:?}");
            assert!(
                text.contains("managed database origin is invalid"),
                "{text}"
            );
            assert!(
                !text.contains("zz-secret-origin"),
                "the input is echoed: {text}"
            );
            assert!(!fx.isolate.has_live_process());
            assert!(fx.spy.records().is_empty());
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs real deno (EDGER_DENO_BIN); run explicitly with --ignored"]
async fn lifecycle_prepare_spawn_recycle_terminate_leaves_nothing_behind() {
    require_deno();
    let mut fx = Fixture::new(Some(vec![])).await;
    fx.arm();
    run_guarded(&mut fx, |fx| Box::pin(lifecycle_scenario(fx))).await;
    // The scenario's last process is gone too (terminated by run_guarded).
    assert_eq!(
        fx.spy.live.load(Ordering::SeqCst),
        0,
        "no guard task survives"
    );
}

async fn lifecycle_scenario(fx: &mut Fixture) {
    let config = fx.config.clone();
    // `prepare` spawns the process (and its guard socket) up front.
    fx.isolate.prepare(&config).await.expect("prepare");
    assert!(fx.isolate.has_live_process(), "prepare spawned the process");
    assert_eq!(fx.spy.serves.load(Ordering::SeqCst), 1);

    assert_eq!(fx.json("/ping").await["status"], 200);
    assert_eq!(fx.spy.live.load(Ordering::SeqCst), 1);
    let first_pid = pid_of(&fx.spy.contexts.lock().unwrap()[0].process_id);
    let first_socket = fx.spy.socket_paths.lock().unwrap()[0].clone();
    assert!(first_socket.exists());

    // Setters do not touch the process in flight: refused, and it still works.
    let sink: Arc<dyn EgressListenerSink> = Arc::new(SpySink(Arc::clone(&fx.spy)));
    assert!(fx.isolate.clear_managed_database_egress().is_err());
    assert!(fx
        .isolate
        .set_managed_database_egress(sink, "x", "9", None, "http://other:1")
        .is_err());
    assert_eq!(fx.json("/ping").await["status"], 200);
    assert_eq!(
        fx.spy.serves.load(Ordering::SeqCst),
        1,
        "same process, same socket"
    );

    // Recycle: terminate drops the process, the guard task and the socket.
    fx.isolate.terminate().await.expect("terminate");
    assert!(!fx.isolate.has_live_process());
    wait_until("the guard task ends", || {
        fx.spy.live.load(Ordering::SeqCst) == 0
    })
    .await;
    wait_until("the socket file is removed", || !first_socket.exists()).await;
    wait_until("the Deno process exits", || {
        !PathBuf::from(format!("/proc/{first_pid}")).exists()
    })
    .await;

    // Setters work again; the next spawn is a new process with a new socket
    // and the same private state.
    fx.isolate.clear_managed_database_egress().expect("clear");
    fx.arm();
    assert_eq!(fx.json("/ping").await["status"], 200);
    assert_eq!(fx.spy.serves.load(Ordering::SeqCst), 2);
    let second_pid = pid_of(&fx.spy.contexts.lock().unwrap()[1].process_id);
    assert_ne!(first_pid, second_pid);
    // After a clear the next spawn has no managed state: the same
    // worker cannot reach the database any more.
    fx.isolate.terminate().await.expect("terminate");
    fx.isolate.clear_managed_database_egress().expect("clear");
    assert_eq!(fx.json("/direct").await["name"], "NotCapable");
    assert_eq!(
        fx.spy.serves.load(Ordering::SeqCst),
        2,
        "cleared: no third socket"
    );
}

/// The failure path itself: a panicking scenario is caught, its spy tasks
/// are aborted and awaited, the child is terminated, and only then does the
/// panic surface (observed here with `catch_unwind`, not by `should_panic`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs real deno (EDGER_DENO_BIN); run explicitly with --ignored"]
async fn a_failing_scenario_still_aborts_tasks_and_terminates_the_child() {
    require_deno();
    let mut fx = Fixture::new(Some(vec![])).await;
    fx.arm();
    let outcome = CatchPanic(Box::pin(run_guarded(&mut fx, |fx| {
        Box::pin(async move {
            assert_eq!(fx.json("/ping").await["status"], 200);
            panic!("forced scenario failure");
        })
    })))
    .await;
    let payload = outcome.expect_err("the panic is re-raised after cleanup");
    let message = payload.downcast_ref::<&str>().copied().unwrap_or_default();
    assert_eq!(message, "forced scenario failure");
    assert!(!fx.isolate.has_live_process(), "child terminated");
    wait_until("guard task gone", || {
        fx.spy.live.load(Ordering::SeqCst) == 0
    })
    .await;
    for bag in fx.spy.bags.lock().unwrap().iter() {
        assert!(
            bag.0.lock().unwrap().is_empty(),
            "connection tasks were awaited"
        );
    }
    let _ = fx.other_port;
}

/// Managed + a manifest that DECLARES general egress: the context the sink
/// receives carries that declaration as authority (`Some`) with its limits;
/// allowNet stays `[]` (no TCP), and the config is not rewritten. The spy
/// only shows the context: serving general egress is a later sink's job.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs real deno (EDGER_DENO_BIN); run explicitly with --ignored"]
async fn declared_general_egress_reaches_the_sink_context_as_some_with_its_limits() {
    require_deno();
    let mut fx = Fixture::new(Some(vec![])).await;
    let mut declared = default_egress_limits();
    declared.ports = vec![443, 8443];
    declared.max_concurrent = 3;
    declared.total_timeout_ms = 12_345;
    fx.config.egress = Some(declared.clone());
    fx.arm();
    run_guarded(&mut fx, |fx| {
        Box::pin(async move {
            let declared = fx.config.egress.clone().expect("declared in the fixture");
            assert_eq!(fx.json("/ping").await["status"], 200);
            let contexts = fx.spy.contexts.lock().unwrap().clone();
            assert_eq!(contexts.len(), 1);
            assert_eq!(
                contexts[0].settings,
                Some(declared.clone()),
                "declared => Some"
            );
            assert_eq!(contexts[0].limits, declared);
            assert_ne!(contexts[0].limits, default_egress_limits());
            assert_eq!(fx.config.egress, Some(declared.clone()), "config untouched");
            assert_eq!(fx.config.allow_net, Some(vec![]), "still no TCP");
            // The database path still works and raw TCP is still refused.
            let v = fx.json("/nondb").await;
            for key in [
                "otherPort",
                "otherScheme",
                "otherHost",
                "external",
                "connect",
                "connectOther",
            ] {
                assert_eq!(v[key], "NotCapable", "{key}: {v}");
            }
            fx.assert_no_tcp().await;
        })
    })
    .await;
}

/// Paired control: the same non-database Request calls (body, Request+init
/// merge, stream) under `allowNet: []` in a baseline WITHOUT managed state
/// (native Deno semantics) and WITH it (the wrapper installed) give the
/// same observables, and neither touches the spy or TCP. This is a
/// behavioural proof of transparency for these shapes; that the wrapper
/// returns `nativeFetch(input, init)` before building anything is a static
/// reading of the early return, not covered by this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs real deno (EDGER_DENO_BIN); run explicitly with --ignored"]
async fn non_database_request_with_body_is_transparent_paired_with_a_baseline() {
    require_deno();
    // Baseline: no managed state, no wrapper.
    let mut base = Fixture::new(Some(vec![])).await;
    run_guarded(&mut base, |fx| {
        Box::pin(async move {
            let v = fx.json("/nondb-request").await;
            fx.assert_no_tcp().await;
            assert_eq!(fx.spy.serves.load(Ordering::SeqCst), 0);
            *BASELINE.lock().unwrap() = Some(v);
        })
    })
    .await;
    let baseline = BASELINE.lock().unwrap().take().expect("baseline observed");
    for key in ["plain", "merged", "initOnly", "streamed"] {
        assert_eq!(
            baseline[key]["name"], "NotCapable",
            "baseline {key}: {baseline}"
        );
    }

    let mut managed = Fixture::new(Some(vec![])).await;
    managed.arm();
    // Same call, same ports are NOT shared between fixtures: compare with
    // the port normalized out of the URL/message.
    let (other_base, other_managed) = (base.other_port, managed.other_port);
    run_guarded(&mut managed, |fx| {
        Box::pin(async move {
            let v = fx.json("/nondb-request").await;
            fx.assert_no_tcp().await;
            assert_eq!(fx.spy.serves.load(Ordering::SeqCst), 1, "socket exists");
            assert!(fx.spy.records().is_empty(), "no call reached the spy");
            *MANAGED.lock().unwrap() = Some(v);
        })
    })
    .await;
    let managed_obs = MANAGED.lock().unwrap().take().expect("managed observed");
    let norm = |v: &serde_json::Value, port: u16| v.to_string().replace(&port.to_string(), "PORT");
    assert_eq!(
        norm(&managed_obs, other_managed),
        norm(&baseline, other_base),
        "wrapper changed an observable of a non-database call"
    );
}

static BASELINE: Mutex<Option<serde_json::Value>> = Mutex::new(None);
static MANAGED: Mutex<Option<serde_json::Value>> = Mutex::new(None);
