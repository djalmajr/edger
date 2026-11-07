#![cfg(unix)]
//! (EDG-54 / 14c) Host provisioning service: controlled-TCP failure matrix
//! plus the opt-in REAL SQLD 0.24.32 flow.
//!
//! Two kinds of proof live here and the report keeps them apart:
//!
//! * "fake" tests drive the production service (`SqldProvisionService`) over
//!   loopback TCP against a controlled HTTP server that can fail between
//!   steps (500, dropped connection after remote creation, 400, hang, wrong
//!   quota, foreign keys, a gated dump). Durable reconstruction (`rebuild`)
//!   reopens the SAME ledger/vault files with new objects; it is NOT a
//!   process crash.
//! * the `#[ignore]` real test uses `EDGER_SQLD_BIN` and never skips:
//!
//! ```text
//! EDGER_SQLD_BIN=/root/.local/share/edger-go-wasm/runtimes/sqld-0.24.32/bin/sqld \
//!   timeout --kill-after=5 300 \
//!   cargo test -p edger-orchestrator --test sqld_provision -- --ignored --nocapture
//! ```
//!
//! There is no HTTP/CLI/runtime binding here (slice 15).

use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use edger_core::secrets::AppSecretResolver;
use edger_orchestrator::app_secrets::AppSecretVault;
use edger_orchestrator::sqld_admin::{SqldAdminClient, SqldAdminError};
use edger_orchestrator::sqld_backup::{SqldBackupError, SqldBackupStore};
use edger_orchestrator::sqld_provision::{
    SqldClock, SqldOwner, SqldProvisionConfig, SqldProvisionError, SqldProvisionService,
    SqldSystemClock, SQLD_TOKEN_SECRET,
};
use edger_orchestrator::sqld_registry::{
    SqldProvisionRegistry, SqldRegistryOwner, SqldRegistryState, MIN_QUOTA_BYTES,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use zeroize::Zeroizing;

const ADMIN_KEY: &str = "canary-admin-sqld-provision";
const LEDGER_MASTER: [u8; 32] = [0x42; 32];
const VAULT_MASTER: [u8; 32] = [0x24; 32];
const QUOTA: u64 = MIN_QUOTA_BYTES; // 16 MiB
const BUDGET: u64 = 8 * MIN_QUOTA_BYTES;
const T0: i64 = 1_800_000_000;
const HOUR: i64 = 3600;

// ───────────────────────── controlled clock ─────────────────────────

/// Controlled clock. `hook_on_call(n, f)` runs `f` INSIDE the n-th
/// `now_unix()` call (1-based), which the service makes right before its CAS
/// write: that is how a manual PUT is interleaved inside the read→CAS window.
type ClockHook = (usize, Box<dyn FnOnce() + Send>);

struct TestClock {
    now: AtomicI64,
    calls: AtomicUsize,
    hook: Mutex<Option<ClockHook>>,
}

impl TestClock {
    fn new(now: i64) -> Arc<Self> {
        Arc::new(Self {
            now: AtomicI64::new(now),
            calls: AtomicUsize::new(0),
            hook: Mutex::new(None),
        })
    }
    fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }
    fn hook_on_call(&self, n: usize, f: impl FnOnce() + Send + 'static) {
        self.calls.store(0, Ordering::SeqCst);
        *self.hook.lock().unwrap() = Some((n, Box::new(f)));
    }
}

impl SqldClock for TestClock {
    fn now_unix(&self) -> i64 {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let fire = {
            let mut slot = self.hook.lock().unwrap();
            match slot.take() {
                Some((n, f)) if n == call => Some(f),
                other => {
                    *slot = other;
                    None
                }
            }
        };
        if let Some(f) = fire {
            f();
        }
        self.now.load(Ordering::SeqCst)
    }
}

// ───────────────────────── controlled HTTP server ─────────────────────────

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    auth: Option<String>,
    ns_bin: Option<String>,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
enum CreateAct {
    Ok,
    /// 400 and the namespace is NOT registered.
    Reject400,
    /// 400 but the namespace exists (registered with the body key).
    Reject400Registered,
    Status(u16),
    /// Registers the namespace then closes the socket without a response.
    RegisterThenDrop,
}

#[derive(Debug, Clone)]
enum PragmaMode {
    Honest,
    Wrong(i64),
    Hrana200Error,
    TextCell,
    Redirect,
    Hang,
    Huge,
    HugeChunked,
}

#[derive(Debug, Clone)]
enum DumpMode {
    Ok,
    Status(u16),
}

struct Ns {
    jwt_key: Value,
    max_pages: i64,
}

struct FakeState {
    namespaces: HashMap<String, Ns>,
    log: Vec<Req>,
    create_script: VecDeque<CreateAct>,
    pragma: PragmaMode,
    dump: DumpMode,
    dump_gate: Option<Arc<tokio::sync::Semaphore>>,
}

struct Fake {
    state: Arc<Mutex<FakeState>>,
    admin_base: String,
    data_base: String,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Fake {
    async fn start() -> Self {
        let state = Arc::new(Mutex::new(FakeState {
            namespaces: HashMap::new(),
            log: Vec::new(),
            create_script: VecDeque::new(),
            pragma: PragmaMode::Honest,
            dump: DumpMode::Ok,
            dump_gate: None,
        }));
        let mut tasks = Vec::new();
        let mut bases = Vec::new();
        for _ in 0..2 {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            bases.push(format!(
                "http://127.0.0.1:{}",
                listener.local_addr().unwrap().port()
            ));
            let state = Arc::clone(&state);
            tasks.push(tokio::spawn(async move {
                let mut conns = tokio::task::JoinSet::new();
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let state = Arc::clone(&state);
                    conns.spawn(async move {
                        let _ = handle_conn(stream, state).await;
                    });
                }
            }));
        }
        Self {
            state,
            admin_base: bases[0].clone(),
            data_base: bases[1].clone(),
            tasks,
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut FakeState) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }

    fn count(&self, method: &str, path_part: &str) -> usize {
        self.with(|s| {
            s.log
                .iter()
                .filter(|r| r.method == method && r.path.contains(path_part))
                .count()
        })
    }

    fn total(&self) -> usize {
        self.with(|s| s.log.len())
    }

    fn requests(&self) -> Vec<Req> {
        self.with(|s| s.log.clone())
    }

    /// Registers `ns` the way the real server would after a CREATE.
    fn register(&self, ns: &str, jwt_key: Value, quota: u64) {
        self.with(|s| {
            s.namespaces.insert(
                ns.to_owned(),
                Ns {
                    jwt_key,
                    max_pages: (quota / 4096) as i64,
                },
            );
        });
    }

    async fn wait_for(&self, what: &str, mut cond: impl FnMut(&FakeState) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if self.with(|s| cond(s)) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Abort + await every server task (a panicking test still drops them).
    async fn stop(mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 256 * 1024 {
            return None;
        }
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let mut auth = None;
    let mut ns_bin = None;
    let mut len = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().to_owned();
            match k.to_ascii_lowercase().as_str() {
                "authorization" => auth = Some(v),
                "x-namespace-bin" => ns_bin = Some(v),
                "content-length" => len = v.parse().ok()?,
                _ => {}
            }
        }
    }
    while buf.len() < header_end + len {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(Req {
        method,
        path,
        auth,
        ns_bin,
        body: buf[header_end..header_end + len].to_vec(),
    })
}

async fn respond(stream: &mut TcpStream, status: u16, ctype: Option<&str>, body: &[u8]) {
    let mut head = format!(
        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    if let Some(ct) = ctype {
        head.push_str(&format!("content-type: {ct}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.shutdown().await;
}

enum Act {
    Json(u16, Value),
    Empty(u16),
    Dump(DumpMode, Option<Arc<tokio::sync::Semaphore>>),
    Drop,
    Hang,
    Redirect,
    Huge(bool),
}

fn ns_from_path(path: &str) -> Option<&str> {
    path.strip_prefix("/v1/namespaces/")?.split('/').next()
}

async fn handle_conn(mut stream: TcpStream, state: Arc<Mutex<FakeState>>) -> Option<()> {
    let req = read_request(&mut stream).await?;
    let act = {
        let mut st = state.lock().unwrap();
        st.log.push(req.clone());
        let authed = req.auth.as_deref() == Some(&format!("Basic {ADMIN_KEY}"));
        if req.path.starts_with("/v1/namespaces/") {
            if !authed {
                Act::Empty(401)
            } else if req.method == "GET" && req.path.ends_with("/config") {
                match st.namespaces.get(ns_from_path(&req.path)?) {
                    None => Act::Empty(404),
                    Some(ns) => Act::Json(
                        200,
                        json!({"block_reads": false, "block_writes": false, "jwt_key": ns.jwt_key,
                               "max_db_size": "16mb", "allow_attach": false}),
                    ),
                }
            } else if req.method == "POST" && req.path.ends_with("/create") {
                let ns = ns_from_path(&req.path)?.to_owned();
                let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                let pages = body["max_db_size"].as_u64().unwrap_or(0) / 4096;
                let register = |st: &mut FakeState| {
                    st.namespaces.insert(
                        ns.clone(),
                        Ns {
                            jwt_key: body["jwt_key"].clone(),
                            max_pages: pages as i64,
                        },
                    );
                };
                match st.create_script.pop_front().unwrap_or(CreateAct::Ok) {
                    CreateAct::Ok => {
                        register(&mut st);
                        Act::Empty(200)
                    }
                    CreateAct::Reject400 => Act::Json(400, json!({"error": "nope"})),
                    CreateAct::Reject400Registered => {
                        register(&mut st);
                        Act::Json(400, json!({"error": "exists"}))
                    }
                    CreateAct::Status(code) => Act::Empty(code),
                    CreateAct::RegisterThenDrop => {
                        register(&mut st);
                        Act::Drop
                    }
                }
            } else {
                Act::Empty(404)
            }
        } else if req.path == "/v2/pipeline" {
            let pages = req
                .ns_bin
                .as_deref()
                .and_then(|h| STANDARD_NO_PAD.decode(h).ok())
                .and_then(|b| String::from_utf8(b).ok())
                .and_then(|ns| st.namespaces.get(&ns).map(|n| n.max_pages));
            let cell = |kind: &str, v: Value| {
                json!({"results": [{"type": "ok", "response": {"type": "execute", "result": {
                    "rows": [[{"type": kind, "value": v}]]}}}, {"type": "ok", "response": {"type": "close"}}]})
            };
            match (&st.pragma, pages) {
                (_, None) => Act::Empty(404),
                (PragmaMode::Honest, Some(p)) => {
                    Act::Json(200, cell("integer", json!(p.to_string())))
                }
                (PragmaMode::Wrong(p), _) => Act::Json(200, cell("integer", json!(p.to_string()))),
                (PragmaMode::Hrana200Error, _) => Act::Json(
                    200,
                    json!({"results": [{"type": "error", "error": {"message": "SECRET-BODY-ECHO", "code": "SQLITE_FULL"}}]}),
                ),
                (PragmaMode::TextCell, _) => Act::Json(200, cell("text", json!("16mb"))),
                (PragmaMode::Redirect, _) => Act::Redirect,
                (PragmaMode::Hang, _) => Act::Hang,
                (PragmaMode::Huge, _) => Act::Huge(false),
                (PragmaMode::HugeChunked, _) => Act::Huge(true),
            }
        } else if req.path == "/dump" {
            Act::Dump(st.dump.clone(), st.dump_gate.clone())
        } else {
            Act::Empty(404)
        }
    };
    match act {
        Act::Json(status, value) => {
            respond(
                &mut stream,
                status,
                Some("application/json"),
                value.to_string().as_bytes(),
            )
            .await
        }
        Act::Empty(status) => respond(&mut stream, status, None, b"").await,
        Act::Dump(mode, gate) => {
            if let Some(gate) = gate {
                // blocks until the test adds a permit (or the task is aborted)
                let permit = gate.acquire().await.ok()?;
                permit.forget();
            }
            match mode {
                DumpMode::Ok => {
                    respond(&mut stream, 200, None, b"BEGIN TRANSACTION;\nCOMMIT;\n").await
                }
                DumpMode::Status(code) => respond(&mut stream, code, None, b"{}").await,
            }
        }
        Act::Huge(chunked) => {
            let body = vec![b' '; 70 * 1024];
            if chunked {
                let head = "HTTP/1.1 200 X\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";
                let _ = stream.write_all(head.as_bytes()).await;
                let size_line = format!("{:x}\r\n", body.len());
                let _ = stream.write_all(size_line.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.write_all(b"\r\n0\r\n\r\n").await;
                let _ = stream.shutdown().await;
            } else {
                respond(&mut stream, 200, Some("application/json"), &body).await;
            }
        }
        Act::Drop => drop(stream),
        Act::Hang => std::future::pending::<()>().await,
        Act::Redirect => {
            let head = "HTTP/1.1 307 X\r\nlocation: http://127.0.0.1:1/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
            let _ = stream.write_all(head.as_bytes()).await;
        }
    }
    Some(())
}

// ───────────────────────── fixture ─────────────────────────

fn private_dir(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

struct Parts {
    registry: Arc<SqldProvisionRegistry>,
    vault: Arc<AppSecretVault>,
    svc: SqldProvisionService,
}

fn build_parts(
    root: &Path,
    admin_base: &str,
    data_base: &str,
    clock: Arc<dyn SqldClock>,
    cfg: SqldProvisionConfig,
    vault_available: bool,
    budget: u64,
) -> Parts {
    build_parts_with_master(
        root,
        admin_base,
        data_base,
        clock,
        cfg,
        vault_available,
        budget,
        VAULT_MASTER,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_parts_with_master(
    root: &Path,
    admin_base: &str,
    data_base: &str,
    clock: Arc<dyn SqldClock>,
    cfg: SqldProvisionConfig,
    vault_available: bool,
    budget: u64,
    vault_master: [u8; 32],
) -> Parts {
    let registry = Arc::new(
        SqldProvisionRegistry::open(
            &root.join("ledger").join("ledger.db"),
            Zeroizing::new(LEDGER_MASTER),
            budget,
        )
        .expect("ledger opens"),
    );
    let vault = Arc::new(if vault_available {
        AppSecretVault::open(root.join("vault.db"), Zeroizing::new(vault_master)).expect("vault")
    } else {
        AppSecretVault::unavailable()
    });
    let admin = SqldAdminClient::new(admin_base, Zeroizing::new(ADMIN_KEY.to_owned())).unwrap();
    let backup = SqldBackupStore::new(data_base, root.join("backups")).unwrap();
    let svc = SqldProvisionService::with_config(
        Arc::clone(&registry),
        Arc::clone(&vault),
        admin,
        backup,
        data_base,
        clock,
        cfg,
    )
    .expect("service builds");
    Parts {
        registry,
        vault,
        svc,
    }
}

struct Env {
    dir: tempfile::TempDir,
    fake: Fake,
    clock: Arc<TestClock>,
    cfg: SqldProvisionConfig,
    parts: Option<Parts>,
}

impl Env {
    async fn new() -> Self {
        Self::with(SqldProvisionConfig::default(), true).await
    }

    async fn with(cfg: SqldProvisionConfig, vault_available: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        private_dir(dir.path(), "ledger");
        private_dir(dir.path(), "backups");
        let fake = Fake::start().await;
        let clock = TestClock::new(T0);
        let parts = build_parts(
            dir.path(),
            &fake.admin_base,
            &fake.data_base,
            clock.clone(),
            cfg,
            vault_available,
            BUDGET,
        );
        Self {
            dir,
            fake,
            clock,
            cfg,
            parts: Some(parts),
        }
    }

    fn p(&self) -> &Parts {
        self.parts.as_ref().unwrap()
    }
    fn svc(&self) -> SqldProvisionService {
        self.p().svc.clone()
    }
    fn vault(&self) -> Arc<AppSecretVault> {
        Arc::clone(&self.p().vault)
    }
    fn registry(&self) -> Arc<SqldProvisionRegistry> {
        Arc::clone(&self.p().registry)
    }

    /// Durable reconstruction: drops every object and reopens the SAME ledger
    /// and vault files. Not a process crash.
    fn rebuild(&mut self) {
        self.parts = None;
        self.parts = Some(build_parts(
            self.dir.path(),
            &self.fake.admin_base,
            &self.fake.data_base,
            self.clock.clone(),
            self.cfg,
            true,
            BUDGET,
        ));
    }

    /// Like `rebuild`, but the vault reopens the SAME store file under
    /// another (still available) master key.
    fn rebuild_with_vault_master(&mut self, vault_master: [u8; 32]) {
        self.parts = None;
        self.parts = Some(build_parts_with_master(
            self.dir.path(),
            &self.fake.admin_base,
            &self.fake.data_base,
            self.clock.clone(),
            self.cfg,
            true,
            BUDGET,
            vault_master,
        ));
    }

    fn vault_path(&self) -> PathBuf {
        self.dir.path().join("vault.db")
    }

    fn state(&self, owner: &SqldOwner) -> Option<SqldRegistryState> {
        self.registry()
            .get(&reg_owner(owner))
            .unwrap()
            .map(|r| r.state)
    }

    fn token(&self, owner: &SqldOwner) -> Option<String> {
        self.vault()
            .get(owner.worker(), SQLD_TOKEN_SECRET)
            .map(|v| (*v).clone())
    }

    fn backups_of(&self, ns: &str) -> usize {
        std::fs::read_dir(self.dir.path().join("backups"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with(ns) && n.ends_with(".sql")
            })
            .count()
    }

    fn ns(&self, owner: &SqldOwner) -> String {
        derive_ns(owner)
    }

    async fn finish(self) {
        let Env {
            fake, parts, dir, ..
        } = self;
        drop(parts);
        fake.stop().await;
        drop(dir);
    }
}

fn app(name: &str) -> SqldOwner {
    SqldOwner::new(name, Some("@t".to_owned()))
}

fn reg_owner(owner: &SqldOwner) -> SqldRegistryOwner<'_> {
    // test owners are built by `app()`: RBAC namespace "@t"
    SqldRegistryOwner {
        worker: owner.worker(),
        rbac_namespace: Some("@t"),
    }
}

/// The derived SQL namespace is deterministic (independent of the key and the
/// master): a scratch in-memory ledger yields it.
fn derive_ns(owner: &SqldOwner) -> String {
    let scratch = SqldProvisionRegistry::in_memory(Zeroizing::new([1u8; 32]), BUDGET).unwrap();
    scratch
        .ensure_intent(&reg_owner(owner), QUOTA)
        .unwrap()
        .sql_namespace
}

fn pub_b64(key: [u8; 32]) -> Value {
    json!(URL_SAFE_NO_PAD.encode(key))
}

fn own_pubkey(env: &Env, owner: &SqldOwner) -> [u8; 32] {
    env.registry()
        .get(&reg_owner(owner))
        .unwrap()
        .unwrap()
        .public_key
}

fn claims_of(jwt: &str) -> Value {
    let payload = jwt.split('.').nth(1).expect("jwt payload");
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
}

// ───────────────────────── tests: preconditions ─────────────────────────

#[tokio::test]
async fn vault_unavailable_means_unavailable_with_no_remote_call_and_no_reserve() {
    let env = Env::with(SqldProvisionConfig::default(), false).await;
    let o = app("a");
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::Unavailable
    );
    assert_eq!(env.fake.total(), 0);
    assert_eq!(env.state(&o), None);
    env.finish().await;
}

#[tokio::test]
async fn backup_with_an_unavailable_vault_makes_no_remote_call() {
    let env = Env::with(SqldProvisionConfig::default(), false).await;
    let o = app("a");
    assert_eq!(
        env.svc().backup(&o).await.unwrap_err(),
        SqldProvisionError::Unavailable
    );
    assert_eq!(env.fake.total(), 0);
    assert_eq!(env.backups_of(&env.ns(&o)), 0);
    env.finish().await;
}

#[tokio::test]
async fn existing_token_under_another_vault_master_is_refused_early_and_preserved() {
    // The row exists in the REAL local store but does not decrypt under the
    // reopened (available) vault: presence must not depend on decryption.
    for value in ["manual-token-value", ""] {
        let mut env = Env::new().await;
        let o = app("a");
        env.vault()
            .set(o.worker(), SQLD_TOKEN_SECRET, value)
            .unwrap();
        env.rebuild_with_vault_master([0x77; 32]);
        assert!(env.vault().is_available());
        assert!(
            env.vault().get(o.worker(), SQLD_TOKEN_SECRET).is_none(),
            "undecryptable reads as None"
        );
        assert_eq!(
            env.svc().ensure(&o, QUOTA).await.unwrap_err(),
            SqldProvisionError::TokenConflict,
            "value {value:?}"
        );
        assert_eq!(env.fake.total(), 0, "no probe/create for value {value:?}");
        assert_eq!(env.state(&o), None, "no registry record or reservation");
        env.rebuild_with_vault_master(VAULT_MASTER);
        assert_eq!(
            env.token(&o).as_deref(),
            Some(value),
            "ciphertext preserved: master A reads it back"
        );
        env.finish().await;
    }
}

#[tokio::test]
async fn presence_listing_failure_fails_closed_before_any_mutation() {
    let env = Env::new().await;
    let o = app("a");
    // controlled REAL failure of the vault's name listing: the table the
    // `SELECT ... FROM app_secrets` reads is renamed through a second
    // connection to the same store file.
    let raw = rusqlite::Connection::open(env.vault_path()).unwrap();
    raw.execute_batch("ALTER TABLE app_secrets RENAME TO app_secrets_gone;")
        .unwrap();
    assert!(
        env.vault().list(o.worker()).is_err(),
        "the listing really fails"
    );
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::Vault
    );
    assert_eq!(
        env.fake.total(),
        0,
        "no probe/create when presence is unknown"
    );
    assert_eq!(env.state(&o), None, "no registry record or reservation");
    // convergence: once the store is back, ensure completes normally
    raw.execute_batch("ALTER TABLE app_secrets_gone RENAME TO app_secrets;")
        .unwrap();
    drop(raw);
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap().metadata.state,
        SqldRegistryState::Ready
    );
    env.finish().await;
}

#[tokio::test]
async fn invalid_quota_is_refused_before_any_remote_call() {
    let env = Env::new().await;
    for q in [0, MIN_QUOTA_BYTES - 4096, MIN_QUOTA_BYTES + 1, u64::MAX] {
        assert_eq!(
            env.svc().ensure(&app("a"), q).await.unwrap_err(),
            SqldProvisionError::InvalidQuota,
            "quota {q}"
        );
    }
    assert_eq!(env.fake.total(), 0);
    assert_eq!(env.state(&app("a")), None);
    env.finish().await;
}

#[tokio::test]
async fn pre_existing_manual_token_conflicts_before_probe_reserve_or_create() {
    for value in ["manual-token-value", ""] {
        let env = Env::new().await;
        let o = app("a");
        env.vault()
            .set(o.worker(), SQLD_TOKEN_SECRET, value)
            .unwrap();
        assert_eq!(
            env.svc().ensure(&o, QUOTA).await.unwrap_err(),
            SqldProvisionError::TokenConflict
        );
        assert_eq!(env.fake.total(), 0, "no probe/create for value {value:?}");
        assert_eq!(env.state(&o), None, "no reservation");
        assert_eq!(
            env.token(&o).as_deref(),
            Some(value),
            "manual value preserved byte-exact"
        );
        env.finish().await;
    }
}

#[tokio::test]
async fn admin_api_open_or_unreachable_blocks_every_mutation() {
    let env = Env::new().await;
    // a wrong admin key => anonymous-looking => probe gets 401 on the authed call
    let o = app("a");
    let admin =
        SqldAdminClient::new(&env.fake.admin_base, Zeroizing::new("wrong-key".into())).unwrap();
    let svc = SqldProvisionService::new(
        env.registry(),
        env.vault(),
        admin,
        SqldBackupStore::new(&env.fake.data_base, env.dir.path().join("backups")).unwrap(),
        &env.fake.data_base,
        env.clock.clone(),
    )
    .unwrap();
    let err = svc.ensure(&o, QUOTA).await.unwrap_err();
    assert!(
        matches!(err, SqldProvisionError::Admin(SqldAdminError::Unauthorized)),
        "{err:?}"
    );
    assert_eq!(env.fake.count("POST", "/create"), 0);
    assert_eq!(
        env.state(&o),
        None,
        "probe failure precedes the reservation"
    );
    env.finish().await;
}

// ───────────────────────── tests: happy path and idempotency ─────────────────────────

#[tokio::test]
async fn happy_path_follows_the_exact_sequence_and_keeps_secrets_out_of_metadata() {
    let env = Env::new().await;
    let o = app("a");
    let out = env.svc().ensure(&o, QUOTA).await.unwrap();
    assert_eq!(out.metadata.state, SqldRegistryState::Ready);
    assert_eq!(out.metadata.quota_bytes, QUOTA);
    assert_eq!(out.metadata.sql_namespace, env.ns(&o));
    let artifact = out
        .initial_backup
        .clone()
        .expect("a new Proven gets its initial backup");
    assert!(artifact.file_name.starts_with(&env.ns(&o)));
    assert_eq!(env.backups_of(&env.ns(&o)), 1);

    // exact request order
    let seq: Vec<String> = env
        .fake
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path.replace(&env.ns(&o), "NS")))
        .collect();
    assert_eq!(
        seq,
        [
            "GET /v1/namespaces/edger_probe/config",
            "GET /v1/namespaces/edger_probe/config",
            "GET /v1/namespaces/NS/config",
            "POST /v1/namespaces/NS/create",
            "GET /v1/namespaces/NS/config",
            "POST /v2/pipeline",
            "GET /dump",
        ]
    );
    // create body: exactly the three fields, the registry public key, the quota
    let reqs = env.fake.requests();
    let body: Value = serde_json::from_slice(&reqs[3].body).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 3);
    assert_eq!(body["jwt_key"], pub_b64(own_pubkey(&env, &o)));
    assert_eq!(body["max_db_size"], json!(QUOTA));
    assert_eq!(body["allow_attach"], json!(false));

    // token: in the vault, exact claims, 12h, and the data/backup calls used it
    let token = env.token(&o).expect("token persisted in the vault");
    let claims = claims_of(&token);
    assert_eq!(claims["exp"], json!(T0 + 12 * HOUR));
    assert_eq!(claims.as_object().unwrap().len(), 2);
    assert_eq!(
        claims["p"],
        json!({"rw": {"ns": [env.ns(&o)]}, "ddl": {"ns": [env.ns(&o)]}})
    );
    for r in [&reqs[5], &reqs[6]] {
        assert_eq!(r.auth.as_deref(), Some(format!("Bearer {token}").as_str()));
        assert_eq!(
            r.ns_bin.as_deref(),
            Some(STANDARD_NO_PAD.encode(env.ns(&o)).as_str())
        );
    }
    // no secret in metadata / Debug
    let shown = format!("{out:?} {:?} {:?}", env.svc(), env.svc().metadata(&o).await);
    assert!(!shown.contains(&token) && !shown.contains("eyJ") && !shown.contains(ADMIN_KEY));
    env.finish().await;
}

#[tokio::test]
async fn repeat_ready_is_idempotent_across_a_durable_rebuild_and_quota_change_is_409() {
    let mut env = Env::new().await;
    let o = app("a");
    env.svc().ensure(&o, QUOTA).await.unwrap();
    let token = env.token(&o).unwrap();
    let before = env.fake.total();
    let second = env.svc().ensure(&o, QUOTA).await.unwrap();
    assert_eq!(
        second.initial_backup, None,
        "Ready repeated: no new initial backup"
    );
    env.rebuild();
    let third = env.svc().ensure(&o, QUOTA).await.unwrap();
    assert_eq!(third.initial_backup, None);
    assert_eq!(third.metadata.sql_namespace, env.ns(&o));
    assert_eq!(env.fake.count("POST", "/create"), 1, "one CREATE in total");
    assert_eq!(env.fake.count("GET", "/dump"), 1, "one dump in total");
    assert_eq!(env.backups_of(&env.ns(&o)), 1);
    assert_eq!(
        env.token(&o).unwrap(),
        token,
        "a fresh token is kept, not rotated"
    );
    // every repeat did the remote proof (config GET + quota PRAGMA)
    assert!(env.fake.total() > before);
    // different quota: 409 before any remote call, no reconfiguration
    let calls = env.fake.total();
    assert_eq!(
        env.svc().ensure(&o, QUOTA + 4096).await.unwrap_err(),
        SqldProvisionError::QuotaConflict
    );
    assert_eq!(env.fake.total(), calls);
    env.finish().await;
}

// ───────────────────────── tests: failures between steps ─────────────────────────

#[tokio::test]
async fn create_500_keeps_intent_and_the_next_ensure_resumes() {
    let env = Env::new().await;
    let o = app("a");
    env.fake
        .with(|s| s.create_script.push_back(CreateAct::Status(500)));
    let err = env.svc().ensure(&o, QUOTA).await.unwrap_err();
    assert!(
        matches!(
            err,
            SqldProvisionError::Admin(SqldAdminError::UnexpectedStatus)
        ),
        "{err:?}"
    );
    assert_eq!(env.state(&o), Some(SqldRegistryState::Intent));
    assert_eq!(env.token(&o), None);
    assert_eq!(env.backups_of(&env.ns(&o)), 0);
    let out = env.svc().ensure(&o, QUOTA).await.unwrap();
    assert_eq!(out.metadata.state, SqldRegistryState::Ready);
    assert_eq!(env.fake.count("POST", "/create"), 2);
    env.finish().await;
}

#[tokio::test]
async fn remote_creation_with_a_dropped_response_is_resumed_by_the_proof_get() {
    let env = Env::new().await;
    let o = app("a");
    env.fake
        .with(|s| s.create_script.push_back(CreateAct::RegisterThenDrop));
    let err = env.svc().ensure(&o, QUOTA).await.unwrap_err();
    assert!(
        matches!(
            err,
            SqldProvisionError::Admin(SqldAdminError::Transport | SqldAdminError::Timeout)
        ),
        "{err:?}"
    );
    assert_eq!(env.state(&o), Some(SqldRegistryState::Intent));
    let out = env.svc().ensure(&o, QUOTA).await.unwrap();
    assert_eq!(out.metadata.state, SqldRegistryState::Ready);
    assert_eq!(
        env.fake.count("POST", "/create"),
        1,
        "the retry found it by GET, no second CREATE"
    );
    env.finish().await;
}

#[tokio::test]
async fn create_400_is_decided_by_the_proof_get_not_assumed_to_mean_exists() {
    // 400 and the namespace exists with our key: proven by the GET
    let env = Env::new().await;
    let o = app("a");
    env.fake
        .with(|s| s.create_script.push_back(CreateAct::Reject400Registered));
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap().metadata.state,
        SqldRegistryState::Ready
    );
    env.finish().await;

    // 400 and nothing exists: CreateNotVisible, Intent kept, reservation kept
    let env = Env::new().await;
    env.fake
        .with(|s| s.create_script.push_back(CreateAct::Reject400));
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::CreateNotVisible
    );
    assert_eq!(env.state(&o), Some(SqldRegistryState::Intent));
    assert_eq!(env.token(&o), None);
    // the next ensure retries the CREATE (it is still Intent)
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap().metadata.state,
        SqldRegistryState::Ready
    );
    env.finish().await;
}

#[tokio::test]
async fn proven_and_ready_with_a_missing_remote_never_create_an_empty_replacement() {
    // Ready, remote vanished
    let env = Env::new().await;
    let o = app("a");
    env.svc().ensure(&o, QUOTA).await.unwrap();
    env.fake.with(|s| s.namespaces.clear());
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::MissingRemote
    );
    assert_eq!(env.fake.count("POST", "/create"), 1, "no CREATE for Ready");
    assert_eq!(env.state(&o), Some(SqldRegistryState::Ready));
    // backup(owner) also fails closed without a dump
    let dumps = env.fake.count("GET", "/dump");
    assert_eq!(
        env.svc().backup(&o).await.unwrap_err(),
        SqldProvisionError::MissingRemote
    );
    assert_eq!(env.fake.count("GET", "/dump"), dumps);
    env.finish().await;

    // Proven (initial backup failed), remote vanished
    let env = Env::new().await;
    env.fake.with(|s| s.dump = DumpMode::Status(500));
    assert!(matches!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::Backup(_)
    ));
    assert_eq!(env.state(&o), Some(SqldRegistryState::Proven));
    env.fake.with(|s| s.namespaces.clear());
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::MissingRemote
    );
    assert_eq!(env.fake.count("POST", "/create"), 1, "no CREATE for Proven");
    env.finish().await;
}

#[tokio::test]
async fn foreign_namespaces_are_refused_persistently_without_takeover_or_config_calls() {
    let variants: Vec<(&str, Value)> = vec![
        ("other key", pub_b64([9u8; 32])),
        ("absent key", Value::Null),
        ("empty key", json!("")),
        (
            "private-looking",
            json!("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----"),
        ),
        ("garbage", json!("not-a-key")),
        ("short key", json!(URL_SAFE_NO_PAD.encode([1u8; 31]))),
    ];
    for (label, remote) in variants {
        let env = Env::new().await;
        let o = app("a");
        env.fake.register(&env.ns(&o), remote.clone(), QUOTA);
        assert_eq!(
            env.svc().ensure(&o, QUOTA).await.unwrap_err(),
            SqldProvisionError::KeyConflict,
            "{label}"
        );
        assert_eq!(env.state(&o), Some(SqldRegistryState::Conflict), "{label}");
        assert_eq!(env.token(&o), None, "{label}");
        assert_eq!(
            env.fake.count("POST", ""),
            0,
            "{label}: no CREATE/config POST"
        );
        // reservation kept: a second owner cannot take the whole budget
        assert_eq!(
            env.svc().ensure(&app("b"), BUDGET).await.unwrap_err(),
            SqldProvisionError::BudgetExceeded,
            "{label}"
        );
        // persistent and terminal: further ensures make no remote call at all
        let calls = env.fake.total();
        assert_eq!(
            env.svc().ensure(&o, QUOTA).await.unwrap_err(),
            SqldProvisionError::KeyConflict
        );
        assert_eq!(env.fake.total(), calls, "{label}");
        // survives a durable rebuild
        let mut env = env;
        env.rebuild();
        assert_eq!(env.state(&o), Some(SqldRegistryState::Conflict), "{label}");
        env.finish().await;
    }
}

#[tokio::test]
async fn ready_owner_whose_remote_key_changes_becomes_a_persistent_conflict() {
    let env = Env::new().await;
    let o = app("a");
    env.svc().ensure(&o, QUOTA).await.unwrap();
    env.fake.register(&env.ns(&o), pub_b64([7u8; 32]), QUOTA);
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::KeyConflict
    );
    assert_eq!(env.state(&o), Some(SqldRegistryState::Conflict));
    let dumps = env.fake.count("GET", "/dump");
    assert_eq!(
        env.svc().backup(&o).await.unwrap_err(),
        SqldProvisionError::KeyConflict
    );
    assert_eq!(env.fake.count("GET", "/dump"), dumps);
    // the resolver now denies, even though a token is still in the vault
    assert!(env.token(&o).is_some());
    let resolver = env.svc().resolver_for(o.clone());
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        None
    );
    env.finish().await;
}

#[tokio::test]
async fn quota_proof_failures_never_reach_ready_and_do_not_leak_the_server_body() {
    let modes: Vec<(&str, PragmaMode)> = vec![
        ("wrong pages", PragmaMode::Wrong(4095)),
        ("hrana error in a 200", PragmaMode::Hrana200Error),
        ("max_db_size style text", PragmaMode::TextCell),
        ("redirect", PragmaMode::Redirect),
    ];
    for (label, mode) in modes {
        let env = Env::new().await;
        let o = app("a");
        env.fake.with(|s| s.pragma = mode.clone());
        let err = env.svc().ensure(&o, QUOTA).await.unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("SECRET-BODY-ECHO"), "{label}");
        assert!(
            matches!(
                err,
                SqldProvisionError::QuotaMismatch
                    | SqldProvisionError::DataInvalid
                    | SqldProvisionError::DataStatus
            ),
            "{label}: {err:?}"
        );
        assert_eq!(env.state(&o), Some(SqldRegistryState::Proven), "{label}");
        assert_eq!(
            env.backups_of(&env.ns(&o)),
            0,
            "{label}: no backup before the quota proof"
        );
        assert_eq!(env.fake.count("GET", "/dump"), 0, "{label}");
        // honest server: the retry completes without a second CREATE
        env.fake.with(|s| s.pragma = PragmaMode::Honest);
        assert_eq!(
            env.svc().ensure(&o, QUOTA).await.unwrap().metadata.state,
            SqldRegistryState::Ready,
            "{label}"
        );
        assert_eq!(env.fake.count("POST", "/create"), 1, "{label}");
        env.finish().await;
    }
}

#[tokio::test]
async fn oversized_data_responses_are_refused_by_header_and_by_streamed_size() {
    for (label, mode) in [
        ("content-length", PragmaMode::Huge),
        ("chunked", PragmaMode::HugeChunked),
    ] {
        let env = Env::new().await;
        let o = app("a");
        env.fake.with(|s| s.pragma = mode.clone());
        assert_eq!(
            env.svc().ensure(&o, QUOTA).await.unwrap_err(),
            SqldProvisionError::DataTooLarge,
            "{label}"
        );
        assert_eq!(env.state(&o), Some(SqldRegistryState::Proven), "{label}");
        env.finish().await;
    }
}

#[tokio::test]
async fn data_deadline_stops_a_hanging_server() {
    let cfg = SqldProvisionConfig {
        data_timeout: Duration::from_millis(300),
        ..Default::default()
    };
    let env = Env::with(cfg, true).await;
    let o = app("a");
    env.fake.with(|s| s.pragma = PragmaMode::Hang);
    let started = std::time::Instant::now();
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::DataTimeout
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(env.state(&o), Some(SqldRegistryState::Proven));
    env.finish().await;
}

#[tokio::test]
async fn initial_backup_failure_keeps_proven_and_the_token_hidden_until_ready() {
    let env = Env::new().await;
    let o = app("a");
    env.fake.with(|s| s.dump = DumpMode::Status(500));
    let err = env.svc().ensure(&o, QUOTA).await.unwrap_err();
    assert!(
        matches!(
            err,
            SqldProvisionError::Backup(SqldBackupError::UnexpectedStatus)
        ),
        "{err:?}"
    );
    assert_eq!(env.state(&o), Some(SqldRegistryState::Proven));
    assert!(
        env.token(&o).is_some(),
        "the own token is already persisted"
    );
    assert_eq!(env.backups_of(&env.ns(&o)), 0);
    // not Ready: the resolver never leaks the token
    let resolver = env.svc().resolver_for(o.clone());
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        None
    );
    // retry: backup succeeds, Ready, resolver now answers
    env.fake.with(|s| s.dump = DumpMode::Ok);
    let out = env.svc().ensure(&o, QUOTA).await.unwrap();
    assert!(out.initial_backup.is_some());
    assert_eq!(env.fake.count("POST", "/create"), 1);
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        env.token(&o)
    );
    env.finish().await;
}

// ───────────────────────── tests: resolver and token lifecycle ─────────────────────────

#[tokio::test]
async fn resolver_serves_only_ready_owners_and_delegates_everything_else() {
    let env = Env::new().await;
    let o = app("a");
    let resolver = env.svc().resolver_for(o.clone());
    // no registry record: legacy behaviour (manual token, even empty)
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        None
    );
    env.vault()
        .set(o.worker(), SQLD_TOKEN_SECRET, "legacy")
        .unwrap();
    assert_eq!(
        resolver
            .resolve_secret(o.worker(), SQLD_TOKEN_SECRET)
            .await
            .as_deref(),
        Some("legacy")
    );
    env.vault().set(o.worker(), SQLD_TOKEN_SECRET, "").unwrap();
    assert_eq!(
        resolver
            .resolve_secret(o.worker(), SQLD_TOKEN_SECRET)
            .await
            .as_deref(),
        Some("")
    );
    // a registered Intent never leaks the manual value
    env.registry().ensure_intent(&reg_owner(&o), QUOTA).unwrap();
    env.vault()
        .set(o.worker(), SQLD_TOKEN_SECRET, "legacy")
        .unwrap();
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        None
    );
    env.finish().await;

    let env = Env::new().await;
    let resolver = env.svc().resolver_for(o.clone());
    env.svc().ensure(&o, QUOTA).await.unwrap();
    let token = env.token(&o).unwrap();
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        Some(token)
    );
    // other secrets delegate to the vault; other worker strings are refused
    env.vault().set(o.worker(), "OTHER_SECRET", "v1").unwrap();
    env.vault()
        .set("someone-else", SQLD_TOKEN_SECRET, "theirs")
        .unwrap();
    env.vault()
        .set("someone-else", "OTHER_SECRET", "theirs")
        .unwrap();
    assert_eq!(
        resolver
            .resolve_secret(o.worker(), "OTHER_SECRET")
            .await
            .as_deref(),
        Some("v1")
    );
    assert_eq!(
        resolver
            .resolve_secret("someone-else", SQLD_TOKEN_SECRET)
            .await,
        None
    );
    assert_eq!(
        resolver
            .resolve_secret("someone-else", "OTHER_SECRET")
            .await,
        None
    );
    assert_eq!(resolver.resolve_secret(o.worker(), "MISSING").await, None);
    env.finish().await;
}

#[tokio::test]
async fn resolver_never_substitutes_or_recreates_and_only_ensure_reseeds() {
    let env = Env::new().await;
    let o = app("a");
    let resolver = env.svc().resolver_for(o.clone());
    env.svc().ensure(&o, QUOTA).await.unwrap();
    let own = env.token(&o).unwrap();
    // manual PUT of a foreign value, an empty value, and a token from a
    // foreign signer: all `None`, none replaced.
    let foreign_signer = {
        let other = SqldProvisionRegistry::in_memory(Zeroizing::new([3u8; 32]), BUDGET).unwrap();
        other.ensure_intent(&reg_owner(&o), QUOTA).unwrap();
        let pk = other.get(&reg_owner(&o)).unwrap().unwrap().public_key;
        other.mark_proven(&reg_owner(&o), pk).unwrap();
        other
            .sign_scoped_token(&reg_owner(&o), T0, 3600 * 12)
            .unwrap()
            .expose_jwt()
            .to_owned()
    };
    for manual in ["manual", "", foreign_signer.as_str()] {
        env.vault()
            .set(o.worker(), SQLD_TOKEN_SECRET, manual)
            .unwrap();
        assert_eq!(
            resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
            None,
            "{manual:.12}"
        );
        assert_eq!(
            env.token(&o).as_deref(),
            Some(manual),
            "manual value preserved"
        );
    }
    // ensure over a foreign value: TokenConflict, value preserved
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::TokenConflict
    );
    assert_eq!(env.token(&o).as_deref(), Some(foreign_signer.as_str()));
    // DELETE at runtime: None, and the resolver does not recreate
    assert!(env.vault().delete(o.worker(), SQLD_TOKEN_SECRET).unwrap());
    let calls = env.fake.total();
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        None
    );
    assert_eq!(
        env.token(&o),
        None,
        "runtime never recreates a deleted token"
    );
    assert_eq!(
        env.fake.total(),
        calls,
        "runtime resolution makes no remote call"
    );
    // backup(owner) does not seed either
    assert_eq!(
        env.svc().backup(&o).await.unwrap_err(),
        SqldProvisionError::TokenMissing
    );
    assert_eq!(env.token(&o), None);
    // the explicit ensure may seed the absent name again
    env.svc().ensure(&o, QUOTA).await.unwrap();
    let reseeded = env.token(&o).unwrap();
    // Ed25519 is deterministic: same key + same clock second = same JWT, so
    // equality with `own` is expected; what matters is that it is valid.
    assert_eq!(reseeded, own);
    assert_eq!(claims_of(&reseeded)["exp"], json!(T0 + 12 * HOUR));
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        Some(reseeded)
    );
    env.finish().await;
}

#[tokio::test]
async fn token_is_renewed_inside_the_last_hour_and_when_authenticated_but_expired() {
    let env = Env::new().await;
    let o = app("a");
    let resolver = env.svc().resolver_for(o.clone());
    env.svc().ensure(&o, QUOTA).await.unwrap();
    let t1 = env.token(&o).unwrap();
    // 1 second before the renewal window: unchanged
    env.clock.set(T0 + 11 * HOUR);
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        Some(t1.clone())
    );
    assert_eq!(env.token(&o).unwrap(), t1);
    // less than 1h left: renewed, persisted, 12h from the injected clock
    env.clock.set(T0 + 11 * HOUR + 1);
    let t2 = resolver
        .resolve_secret(o.worker(), SQLD_TOKEN_SECRET)
        .await
        .unwrap();
    assert_ne!(t2, t1);
    assert_eq!(claims_of(&t2)["exp"], json!(T0 + 11 * HOUR + 1 + 12 * HOUR));
    assert_eq!(env.token(&o).unwrap(), t2);
    // authenticated but long expired: renewed as well
    env.clock.set(T0 + 40 * HOUR);
    let t3 = resolver
        .resolve_secret(o.worker(), SQLD_TOKEN_SECRET)
        .await
        .unwrap();
    assert_ne!(t3, t2);
    assert_eq!(claims_of(&t3)["exp"], json!(T0 + 40 * HOUR + 12 * HOUR));
    assert_eq!(env.token(&o).unwrap(), t3);
    // ensure renews through the same CAS path
    env.clock.set(T0 + 60 * HOUR);
    env.svc().ensure(&o, QUOTA).await.unwrap();
    assert_eq!(
        claims_of(&env.token(&o).unwrap())["exp"],
        json!(T0 + 72 * HOUR)
    );
    env.finish().await;
}

#[tokio::test]
async fn concurrent_manual_put_inside_the_read_to_cas_window_wins_on_renewal() {
    let env = Env::new().await;
    let o = app("a");
    let resolver = env.svc().resolver_for(o.clone());
    env.svc().ensure(&o, QUOTA).await.unwrap();
    env.clock.set(T0 + 20 * HOUR); // expired and authenticated
                                   // resolver clock calls: 1 fast-path read, 2 locked re-read, 3 mint (right
                                   // before the CAS): the manual PUT lands exactly between read and CAS
    let vault = env.vault();
    let worker = o.worker().to_owned();
    env.clock.hook_on_call(3, move || {
        vault
            .set(&worker, SQLD_TOKEN_SECRET, "manual-put-wins")
            .unwrap();
    });
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        None,
        "CAS lost: fail closed"
    );
    assert_eq!(
        env.token(&o).as_deref(),
        Some("manual-put-wins"),
        "the manual PUT is preserved"
    );
    // and nothing retried/looped: the next resolution still sees the manual value
    assert_eq!(
        resolver.resolve_secret(o.worker(), SQLD_TOKEN_SECRET).await,
        None
    );
    assert_eq!(env.token(&o).as_deref(), Some("manual-put-wins"));
    env.finish().await;
}

#[tokio::test]
async fn concurrent_manual_put_inside_the_seed_window_wins_over_ensure() {
    let env = Env::new().await;
    let o = app("a");
    // ensure clock calls: 1 token read (absent), 2 mint (right before CAS(None))
    let vault = env.vault();
    let worker = o.worker().to_owned();
    env.clock.hook_on_call(2, move || {
        vault
            .set(&worker, SQLD_TOKEN_SECRET, "put-during-seed")
            .unwrap();
    });
    assert_eq!(
        env.svc().ensure(&o, QUOTA).await.unwrap_err(),
        SqldProvisionError::TokenConflict
    );
    assert_eq!(env.token(&o).as_deref(), Some("put-during-seed"));
    assert_eq!(
        env.state(&o),
        Some(SqldRegistryState::Proven),
        "no Ready without own credential"
    );
    assert_eq!(env.backups_of(&env.ns(&o)), 0);
    env.finish().await;
}

// ───────────────────────── tests: backup(owner) ─────────────────────────

#[tokio::test]
async fn backup_requires_ready_proves_remotely_first_and_uses_the_current_credential() {
    let env = Env::new().await;
    let o = app("a");
    assert_eq!(
        env.svc().backup(&o).await.unwrap_err(),
        SqldProvisionError::NotFound
    );
    env.fake.with(|s| s.dump = DumpMode::Status(500));
    env.svc().ensure(&o, QUOTA).await.unwrap_err(); // leaves Proven
    env.fake.with(|s| s.dump = DumpMode::Ok);
    assert_eq!(
        env.svc().backup(&o).await.unwrap_err(),
        SqldProvisionError::NotReady
    );
    env.svc().ensure(&o, QUOTA).await.unwrap();
    let before = env.fake.total();
    let artifact = env.svc().backup(&o).await.unwrap();
    assert!(artifact.file_name.starts_with(&env.ns(&o)));
    assert_eq!(env.backups_of(&env.ns(&o)), 2);
    let reqs = env.fake.requests();
    assert_eq!(
        reqs[before].path.replace(&env.ns(&o), "NS"),
        "/v1/namespaces/NS/config",
        "proof first"
    );
    assert_eq!(reqs.last().unwrap().path, "/dump");
    let t1 = env.token(&o).unwrap();
    assert_eq!(
        reqs.last().unwrap().auth.as_deref(),
        Some(format!("Bearer {t1}").as_str())
    );
    // expired credential: backup renews it first and dumps with the NEW token
    env.clock.set(T0 + 30 * HOUR);
    env.svc().backup(&o).await.unwrap();
    let t2 = env.token(&o).unwrap();
    assert_ne!(t1, t2);
    assert_eq!(
        env.fake.requests().last().unwrap().auth.as_deref(),
        Some(format!("Bearer {t2}").as_str())
    );
    // metadata only: nothing but name/size/hash/retention
    let shown = format!("{artifact:?}");
    assert!(!shown.contains("eyJ") && !shown.contains(&t1));
    env.finish().await;
}

// ───────────────────────── tests: concurrency and isolation ─────────────────────────

#[tokio::test]
async fn same_owner_concurrency_reuses_one_identity_and_one_initial_backup() {
    let env = Env::new().await;
    let o = app("a");
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let svc = env.svc();
        let o = o.clone();
        set.spawn(async move { svc.ensure(&o, QUOTA).await });
    }
    let mut initial = 0;
    while let Some(joined) = set.join_next().await {
        let out = joined.unwrap().expect("every concurrent ensure succeeds");
        assert_eq!(out.metadata.sql_namespace, env.ns(&o));
        initial += usize::from(out.initial_backup.is_some());
    }
    assert_eq!(initial, 1, "exactly one ensure created the initial backup");
    assert_eq!(env.fake.count("POST", "/create"), 1);
    assert_eq!(env.backups_of(&env.ns(&o)), 1);
    env.finish().await;
}

#[tokio::test]
async fn distinct_owners_have_distinct_namespaces_keys_and_tokens() {
    let env = Env::new().await;
    let (a, b) = (app("a"), app("b"));
    let svc = env.svc();
    let (ra, rb) = tokio::join!(svc.ensure(&a, QUOTA), svc.ensure(&b, 2 * QUOTA));
    let (ra, rb) = (ra.unwrap(), rb.unwrap());
    assert_ne!(ra.metadata.sql_namespace, rb.metadata.sql_namespace);
    assert_eq!(rb.metadata.quota_bytes, 2 * QUOTA);
    assert_ne!(own_pubkey(&env, &a), own_pubkey(&env, &b));
    let (ta, tb) = (env.token(&a).unwrap(), env.token(&b).unwrap());
    assert_ne!(ta, tb);
    assert_eq!(claims_of(&ta)["p"]["rw"]["ns"], json!([env.ns(&a)]));
    assert_eq!(claims_of(&tb)["p"]["rw"]["ns"], json!([env.ns(&b)]));
    // each resolver only serves its own token, bound to its owner
    let (resolver_a, resolver_b) = (
        env.svc().resolver_for(a.clone()),
        env.svc().resolver_for(b.clone()),
    );
    assert_eq!(
        resolver_a
            .resolve_secret(a.worker(), SQLD_TOKEN_SECRET)
            .await,
        Some(ta)
    );
    assert_eq!(
        resolver_a
            .resolve_secret(b.worker(), SQLD_TOKEN_SECRET)
            .await,
        None
    );
    assert_eq!(
        resolver_b
            .resolve_secret(b.worker(), SQLD_TOKEN_SECRET)
            .await,
        Some(tb)
    );
    // the PRAGMA of each used its own namespace header (8192 pages for B)
    assert_eq!(env.fake.count("POST", "/v2/pipeline"), 2);
    env.finish().await;
}

#[tokio::test]
async fn a_long_backup_blocks_neither_other_apps_nor_the_token_renewal_of_its_owner() {
    let env = Env::new().await;
    let (a, b) = (app("a"), app("b"));
    env.svc().ensure(&a, QUOTA).await.unwrap();
    env.svc().ensure(&b, QUOTA).await.unwrap();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    env.fake.with(|s| s.dump_gate = Some(Arc::clone(&gate)));
    let dumps = env.fake.count("GET", "/dump");
    let long_backup = {
        let svc = env.svc();
        let a = a.clone();
        tokio::spawn(async move { svc.backup(&a).await })
    };
    env.fake
        .wait_for("the dump to start", |s| {
            s.log.iter().filter(|r| r.path == "/dump").count() == dumps + 1
        })
        .await;
    // the dump is in flight (gated): the renewal lock of A and everything of B stay free
    let (ra, rb) = (
        env.svc().resolver_for(a.clone()),
        env.svc().resolver_for(b.clone()),
    );
    env.clock.set(T0 + 20 * HOUR);
    let renewed = tokio::time::timeout(
        Duration::from_secs(3),
        ra.resolve_secret(a.worker(), SQLD_TOKEN_SECRET),
    )
    .await
    .expect("renewal of A is not blocked by A's long backup")
    .expect("renewed");
    assert_eq!(claims_of(&renewed)["exp"], json!(T0 + 32 * HOUR));
    assert!(tokio::time::timeout(
        Duration::from_secs(3),
        rb.resolve_secret(b.worker(), SQLD_TOKEN_SECRET)
    )
    .await
    .expect("B not blocked")
    .is_some());
    assert!(
        tokio::time::timeout(Duration::from_secs(3), env.svc().metadata(&b))
            .await
            .expect("metadata not blocked")
            .is_ok()
    );
    assert!(
        !long_backup.is_finished(),
        "the long backup is still running"
    );
    gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), long_backup)
        .await
        .expect("backup completes")
        .unwrap()
        .unwrap();
    env.finish().await;
}

#[tokio::test]
async fn cancelling_ensure_during_the_initial_backup_releases_the_owner_lock() {
    let env = Env::new().await;
    let (a, b) = (app("a"), app("b"));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    env.fake.with(|s| s.dump_gate = Some(Arc::clone(&gate)));
    // A's ensure reaches the gated dump and then the caller cancels (timeout)
    let first =
        tokio::time::timeout(Duration::from_millis(1500), env.svc().ensure(&a, QUOTA)).await;
    assert!(
        first.is_err(),
        "the ensure was cancelled while blocked in the dump"
    );
    assert_eq!(env.fake.count("GET", "/dump"), 1);
    assert_eq!(
        env.state(&a),
        Some(SqldRegistryState::Proven),
        "no Ready after a cancelled ensure"
    );
    // another owner is not affected by A's (released) lock or a global one
    env.vault()
        .set(b.worker(), SQLD_TOKEN_SECRET, "manual")
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), env.svc().ensure(&b, QUOTA))
            .await
            .expect("not blocked")
            .unwrap_err(),
        SqldProvisionError::TokenConflict
    );
    // the same owner can run again immediately (RAII released the lock)
    env.fake.with(|s| s.dump_gate = None);
    let out = tokio::time::timeout(Duration::from_secs(10), env.svc().ensure(&a, QUOTA))
        .await
        .expect("lock released")
        .unwrap();
    assert_eq!(out.metadata.state, SqldRegistryState::Ready);
    assert_eq!(env.fake.count("POST", "/create"), 1);
    env.finish().await;
}

#[tokio::test]
async fn same_owner_ensure_waits_for_the_owner_lock_while_other_owners_proceed() {
    let env = Env::new().await;
    let (a, b) = (app("a"), app("b"));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    env.fake.with(|s| s.dump_gate = Some(Arc::clone(&gate)));
    let first = {
        let svc = env.svc();
        let a = a.clone();
        tokio::spawn(async move { svc.ensure(&a, QUOTA).await })
    };
    env.fake
        .wait_for("the first dump", |s| {
            s.log.iter().any(|r| r.path == "/dump")
        })
        .await;
    let second = {
        let svc = env.svc();
        let a = a.clone();
        tokio::spawn(async move { svc.ensure(&a, QUOTA).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !second.is_finished(),
        "the same owner waits on the provisioning lock"
    );
    // another owner's early failure returns at once (no global lock)
    env.vault()
        .set(b.worker(), SQLD_TOKEN_SECRET, "manual")
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), env.svc().ensure(&b, QUOTA))
            .await
            .unwrap()
            .unwrap_err(),
        SqldProvisionError::TokenConflict
    );
    gate.add_permits(1);
    let one = tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let two = tokio::time::timeout(Duration::from_secs(10), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(one.initial_backup.is_some() && two.initial_backup.is_none());
    env.finish().await;
}

// ───────────────────────── REAL SQLD 0.24.32 (opt-in) ─────────────────────────

const SCENARIO_DEADLINE: Duration = Duration::from_secs(240);
const STARTUP_DEADLINE: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const PUB_KEY_FILE: &str = "ed25519-global.pub";
const REAL_BUDGET: u64 = 16 * MIN_QUOTA_BYTES;
const LOG_READ_MAX: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Ok(Vec<Vec<Value>>),
    Denied(String),
}

fn classify(status: u16, value: &Value) -> Step {
    if status != 200 {
        return Step::Denied(format!("http {status}"));
    }
    let first = value.pointer("/results/0");
    match first.and_then(|r| r.get("type")).and_then(Value::as_str) {
        Some("ok") => Step::Ok(
            value
                .pointer("/results/0/response/result/rows")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .map(|row| row.as_array().cloned().unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default(),
        ),
        _ => Step::Denied(
            first
                .and_then(|r| r.pointer("/error/code"))
                .and_then(Value::as_str)
                .unwrap_or("protocol")
                .to_owned(),
        ),
    }
}

fn single_int(step: &Step) -> Option<i64> {
    match step {
        Step::Ok(rows) if rows.len() == 1 && rows[0].len() == 1 => {
            rows[0][0].get("value")?.as_str()?.parse().ok()
        }
        _ => None,
    }
}

struct GlobalKeys {
    pub_pem: String,
}

fn generate_global_keys() -> GlobalKeys {
    let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("ed25519 pair");
    GlobalKeys {
        pub_pem: pair.public_key_pem(),
    }
}

/// Two DISTINCT ephemeral loopback ports for one process. Both probe
/// listeners stay open until both ports are known and only then are released:
/// two calls to a one-port helper could get the same port back from the
/// kernel, because the first probe is already closed when the second binds.
async fn ephemeral_port_pair() -> (u16, u16) {
    let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second = TcpListener::bind("127.0.0.1:0").await.unwrap();
    (
        first.local_addr().unwrap().port(),
        second.local_addr().unwrap().port(),
    )
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(HTTP_TIMEOUT)
        .build()
        .unwrap()
}

/// The REAL sqld child; every path ends in `shutdown` (kill + wait + pid gone).
struct Sqld {
    child: Option<tokio::process::Child>,
    pid: u32,
    user_port: u16,
    admin_port: u16,
}

impl Sqld {
    fn spawn(bin: &str, dir: &Path, pub_pem: &str, user_port: u16, admin_port: u16) -> Self {
        let pub_path = dir.join(PUB_KEY_FILE);
        std::fs::write(&pub_path, pub_pem.as_bytes()).unwrap();
        let stdout = std::fs::File::create(dir.join("sqld-stdout.log")).unwrap();
        let stderr = std::fs::File::create(dir.join("sqld-stderr.log")).unwrap();
        let child = tokio::process::Command::new(bin)
            .arg("--db-path")
            .arg(dir.join("data.sqld"))
            .arg("--http-listen-addr")
            .arg(format!("127.0.0.1:{user_port}"))
            .arg("--admin-listen-addr")
            .arg(format!("127.0.0.1:{admin_port}"))
            .arg("--admin-auth-key")
            .arg(ADMIN_KEY)
            .arg("--enable-namespaces")
            .arg("--disable-default-namespace")
            .arg("--auth-jwt-key-file")
            .arg(&pub_path)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("RUST_LOG", "error")
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|e| panic!("cannot spawn the real sqld {bin:?}: {e}"));
        let pid = child.id().expect("child pid");
        Self {
            child: Some(child),
            pid,
            user_port,
            admin_port,
        }
    }

    async fn wait_ready(&mut self, client: &reqwest::Client) -> Result<(), String> {
        let url = format!(
            "http://127.0.0.1:{}/v1/namespaces/edger_probe/config",
            self.admin_port
        );
        let deadline = tokio::time::Instant::now() + STARTUP_DEADLINE;
        loop {
            if let Some(child) = self.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        return Err(format!("sqld exited during startup: {status:?}"))
                    }
                    Ok(None) => {}
                    Err(e) => return Err(format!("cannot poll the child: {e}")),
                }
            }
            match client
                .get(&url)
                .header("authorization", format!("Basic {ADMIN_KEY}"))
                .send()
                .await
            {
                Ok(resp) if resp.status().as_u16() == 404 => return Ok(()),
                Ok(resp) => {
                    return Err(format!(
                        "admin gate answered {} before ready",
                        resp.status()
                    ))
                }
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await
                }
                Err(_) => return Err("the admin gate did not answer in time".into()),
            }
        }
    }

    async fn shutdown(&mut self) -> Result<String, String> {
        let Some(mut child) = self.child.take() else {
            return Ok("already stopped".into());
        };
        let kill = child.kill().await;
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .map_err(|_| "the owned sqld child was not reaped within 10 s".to_string())?
            .map_err(|e| format!("wait failed: {e}"))?;
        let pid = rustix::process::Pid::from_raw(self.pid as i32).ok_or("bad pid")?;
        let gone = matches!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        if !gone {
            return Err(format!("pid {} still answers after wait", self.pid));
        }
        Ok(format!(
            "kill={kill:?} wait={status:?} pid={} gone=true",
            self.pid
        ))
    }
}

struct Ctx {
    user_base: String,
    admin_base: String,
    dir: PathBuf,
    client: reqwest::Client,
}

impl Ctx {
    async fn pipeline(&self, ns: &str, token: &str, sql: &str) -> Step {
        let body = json!({"requests": [
            {"type": "execute", "stmt": {"sql": sql}}, {"type": "close"}]})
        .to_string();
        let resp = self
            .client
            .post(format!("{}/v2/pipeline", self.user_base))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .header("x-namespace-bin", STANDARD_NO_PAD.encode(ns))
            .body(body)
            .send()
            .await
            .expect("pipeline request completes");
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.expect("bounded body");
        assert!(bytes.len() < 256 * 1024);
        classify(
            status,
            &serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn admin_post(&self, ns: &str, body: Value) -> u16 {
        self.client
            .post(format!("{}/v1/namespaces/{ns}/create", self.admin_base))
            .header("authorization", format!("Basic {ADMIN_KEY}"))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .expect("admin create completes")
            .status()
            .as_u16()
    }
}

fn restore_dump(path: &Path) -> rusqlite::Connection {
    let text = std::fs::read_to_string(path).expect("the backup is UTF-8 SQL text");
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(&text)
        .expect("the backup restores into an isolated SQLite");
    conn
}

fn real_root(dir: &Path) -> PathBuf {
    let root = dir.join("service");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    private_dir(&root, "ledger");
    private_dir(&root, "backups");
    root
}

async fn real_scenario(ctx: Ctx) {
    let root = real_root(&ctx.dir);
    let cfg = SqldProvisionConfig::default();
    let mut parts = build_parts(
        &root,
        &ctx.admin_base,
        &ctx.user_base,
        Arc::new(SqldSystemClock),
        cfg,
        true,
        REAL_BUDGET,
    );
    let (a, b) = (app("real-a"), app("real-b"));
    let (ns_a, ns_b) = (derive_ns(&a), derive_ns(&b));
    let (quota_a, quota_b) = (QUOTA, 2 * QUOTA);

    // 1. two apps, own keys and quotas; Ready comes only from the service
    let svc = parts.svc.clone();
    let (ra, rb) = tokio::join!(svc.ensure(&a, quota_a), svc.ensure(&b, quota_b));
    let (ra, rb) = (ra.expect("ensure A"), rb.expect("ensure B"));
    for (out, ns, quota) in [(&ra, &ns_a, quota_a), (&rb, &ns_b, quota_b)] {
        assert_eq!(out.metadata.state, SqldRegistryState::Ready);
        assert_eq!(&out.metadata.sql_namespace, ns);
        assert_eq!(out.metadata.quota_bytes, quota);
        assert!(
            out.initial_backup.is_some(),
            "initial scoped backup before Ready"
        );
    }
    let tok = |o: &SqldOwner, parts: &Parts| -> String {
        parts
            .vault
            .get(o.worker(), SQLD_TOKEN_SECRET)
            .expect("token in the vault")
            .to_string()
    };
    let (ta, tb) = (tok(&a, &parts), tok(&b, &parts));
    assert_ne!(ta, tb);
    // tokens live in the vault, never in metadata/outcomes
    let shown = format!(
        "{ra:?}{rb:?}{:?}{:?}",
        svc.metadata(&a).await,
        svc.metadata(&b).await
    );
    assert!(!shown.contains(&ta) && !shown.contains(&tb) && !shown.contains("eyJ"));
    // the resolver hands out exactly the vault tokens
    let (res_a, res_b) = (svc.resolver_for(a.clone()), svc.resolver_for(b.clone()));
    assert_eq!(
        res_a
            .resolve_secret(a.worker(), SQLD_TOKEN_SECRET)
            .await
            .as_deref(),
        Some(ta.as_str())
    );
    assert_eq!(
        res_b
            .resolve_secret(b.worker(), SQLD_TOKEN_SECRET)
            .await
            .as_deref(),
        Some(tb.as_str())
    );
    // per-app quota enforced by the real server
    assert_eq!(
        single_int(&ctx.pipeline(&ns_a, &ta, "PRAGMA max_page_count").await),
        Some((quota_a / 4096) as i64)
    );
    assert_eq!(
        single_int(&ctx.pipeline(&ns_b, &tb, "PRAGMA max_page_count").await),
        Some((quota_b / 4096) as i64)
    );
    // cross credentials/namespaces are denied
    assert!(
        matches!(ctx.pipeline(&ns_b, &ta, "SELECT 1").await, Step::Denied(_)),
        "token A on namespace B"
    );
    assert!(
        matches!(ctx.pipeline(&ns_a, &tb, "SELECT 1").await, Step::Denied(_)),
        "token B on namespace A"
    );
    assert!(matches!(
        ctx.pipeline(&ns_a, "manual", "SELECT 1").await,
        Step::Denied(_)
    ));

    // 2. restorable initial backup, then a real data backup through backup(owner)
    let backups = root.join("backups");
    let initial_a = backups.join(&ra.initial_backup.as_ref().unwrap().file_name);
    assert_eq!(
        std::fs::metadata(&initial_a).unwrap().permissions().mode() & 0o777,
        0o600
    );
    restore_dump(&initial_a);
    for sql in [
        "CREATE TABLE t(v TEXT)",
        "INSERT INTO t(v) VALUES ('alpha'), ('beta')",
    ] {
        assert!(
            matches!(ctx.pipeline(&ns_a, &ta, sql).await, Step::Ok(_)),
            "{sql}"
        );
    }
    let artifact = svc.backup(&a).await.expect("backup(owner) on Ready");
    let restored = restore_dump(&backups.join(&artifact.file_name));
    let rows: i64 = restored
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 2, "the owner's data is in the restorable dump");
    let b_sees_t = ctx.pipeline(&ns_b, &tb, "SELECT count(*) FROM t").await;
    assert!(matches!(b_sees_t, Step::Denied(_)), "B never saw A's table");

    // 3. idempotency after rebuilding the service with the SAME ledger/vault
    let count_files = |ns: &str| {
        std::fs::read_dir(&backups)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(ns))
            .count()
    };
    let before = (count_files(&ns_a), count_files(&ns_b));
    drop(svc);
    drop(res_a);
    drop(res_b);
    drop(parts);
    parts = build_parts(
        &root,
        &ctx.admin_base,
        &ctx.user_base,
        Arc::new(SqldSystemClock),
        cfg,
        true,
        REAL_BUDGET,
    );
    let svc = parts.svc.clone();
    for (o, ns, quota, token) in [(&a, &ns_a, quota_a, &ta), (&b, &ns_b, quota_b, &tb)] {
        let again = svc
            .ensure(o, quota)
            .await
            .expect("idempotent ensure after rebuild");
        assert_eq!(again.metadata.state, SqldRegistryState::Ready);
        assert_eq!(&again.metadata.sql_namespace, ns);
        assert!(
            again.initial_backup.is_none(),
            "no new initial backup for Ready"
        );
        assert_eq!(tok(o, &parts), *token, "fresh token kept");
    }
    assert_eq!(
        (count_files(&ns_a), count_files(&ns_b)),
        before,
        "no extra backups"
    );
    assert_eq!(
        svc.ensure(&a, quota_a + 4096).await.unwrap_err(),
        SqldProvisionError::QuotaConflict
    );

    // 4. existing foreign namespaces (other key / no per-app key): refused
    let admin =
        SqldAdminClient::new(&ctx.admin_base, Zeroizing::new(ADMIN_KEY.to_owned())).unwrap();
    let other_pub = {
        let scratch =
            SqldProvisionRegistry::in_memory(Zeroizing::new([2u8; 32]), REAL_BUDGET).unwrap();
        scratch
            .ensure_intent(&reg_owner(&a), QUOTA)
            .unwrap()
            .public_key
    };
    let (c, d) = (app("real-c"), app("real-d"));
    let (ns_c, ns_d) = (derive_ns(&c), derive_ns(&d));
    let status = ctx
        .admin_post(
            &ns_c,
            json!({"jwt_key": URL_SAFE_NO_PAD.encode(other_pub), "max_db_size": QUOTA, "allow_attach": false}),
        )
        .await;
    assert_eq!(status, 200, "foreign namespace C created by the fixture");
    assert_eq!(
        ctx.admin_post(&ns_d, json!({"max_db_size": QUOTA, "allow_attach": false}))
            .await,
        200
    );
    for (o, ns) in [(&c, &ns_c), (&d, &ns_d)] {
        let before_cfg = admin.read_config(ns).await.expect("config before");
        assert_eq!(
            svc.ensure(o, QUOTA).await.unwrap_err(),
            SqldProvisionError::KeyConflict
        );
        assert_eq!(
            parts.registry.get(&reg_owner(o)).unwrap().unwrap().state,
            SqldRegistryState::Conflict
        );
        assert_eq!(
            admin.read_config(ns).await.expect("config after"),
            before_cfg,
            "no takeover"
        );
        assert!(
            parts.vault.get(o.worker(), SQLD_TOKEN_SECRET).is_none(),
            "no token on conflict"
        );
        assert_eq!(count_files(ns), 0, "no backup for a conflict");
        assert_eq!(
            svc.ensure(o, QUOTA).await.unwrap_err(),
            SqldProvisionError::KeyConflict
        );
    }

    // 5. concurrency, same owner: one identity, one initial backup
    let e = app("real-e");
    let ns_e = derive_ns(&e);
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let (svc, e) = (svc.clone(), e.clone());
        set.spawn(async move { svc.ensure(&e, QUOTA).await });
    }
    let mut initials = 0;
    while let Some(joined) = set.join_next().await {
        let out = joined.unwrap().expect("concurrent ensure");
        assert_eq!(out.metadata.sql_namespace, ns_e);
        initials += usize::from(out.initial_backup.is_some());
    }
    assert_eq!((initials, count_files(&ns_e)), (1, 1));

    // 6. expiry: a REALLY expired own token is denied by the server, then
    //    renewed by the resolver; the new one works, the old stays denied
    let old = {
        let now = chrono::Utc::now().timestamp();
        let signed = parts
            .registry
            .sign_scoped_token(&reg_owner(&a), now - 12 * HOUR - 120, 12 * 3600)
            .expect("past-dated token");
        signed.expose_jwt().to_owned()
    };
    parts
        .vault
        .set(a.worker(), SQLD_TOKEN_SECRET, &old)
        .unwrap(); // fixture state
    assert!(
        matches!(ctx.pipeline(&ns_a, &old, "SELECT 1").await, Step::Denied(_)),
        "expired token denied by the server"
    );
    let resolver = svc.resolver_for(a.clone());
    let fresh = resolver
        .resolve_secret(a.worker(), SQLD_TOKEN_SECRET)
        .await
        .expect("renewed");
    assert_ne!(fresh, old);
    assert_eq!(tok(&a, &parts), fresh, "renewal persisted by CAS");
    assert!(matches!(
        ctx.pipeline(&ns_a, &fresh, "SELECT count(*) FROM t").await,
        Step::Ok(_)
    ));
    assert!(
        matches!(ctx.pipeline(&ns_a, &old, "SELECT 1").await, Step::Denied(_)),
        "old stays denied"
    );
    svc.backup(&a)
        .await
        .expect("backup with the renewed credential");
}

async fn run_owned<T, F>(deadline: Duration, fut: F) -> Result<T, Option<tokio::task::JoinError>>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let mut task = tokio::spawn(fut);
    match tokio::time::timeout(deadline, &mut task).await {
        Ok(joined) => joined.map_err(Some),
        Err(_) => {
            task.abort();
            let _ = (&mut task).await;
            Err(None)
        }
    }
}

fn bounded_read(path: &Path, max: usize) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .unwrap();
    assert!(out.len() <= max);
    out
}

fn has_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn expired_scenario_is_aborted_and_joined_before_run_owned_returns() {
    use std::sync::atomic::AtomicBool;
    struct Flag(Arc<AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = Flag(dropped.clone());
    let out = run_owned(Duration::from_millis(50), async move {
        let _flag = flag;
        std::future::pending::<()>().await;
    })
    .await;
    assert!(matches!(out, Err(None)));
    assert!(
        dropped.load(Ordering::SeqCst),
        "the task is gone when run_owned returns"
    );
}

#[tokio::test]
#[ignore = "requires the operator-approved real sqld 0.24.32 (EDGER_SQLD_BIN)"]
async fn real_sqld_provisions_two_apps_end_to_end() {
    let bin = std::env::var("EDGER_SQLD_BIN").unwrap_or_else(|_| {
        panic!("EDGER_SQLD_BIN must name the real sqld binary; this test never skips")
    });
    assert!(
        Path::new(&bin).is_file(),
        "EDGER_SQLD_BIN does not name a file: the test never skips"
    );
    let global = generate_global_keys();
    let dir = tempfile::tempdir().expect("fixture tempdir");
    let (user_port, admin_port) = ephemeral_port_pair().await;
    assert_ne!(user_port, admin_port);
    let mut sqld = Sqld::spawn(&bin, dir.path(), &global.pub_pem, user_port, admin_port);
    eprintln!("provision-evidence: sqld pid={}", sqld.pid);
    let client = http_client();
    let outcome = match sqld.wait_ready(&client).await {
        Ok(()) => {
            let ctx = Ctx {
                user_base: format!("http://127.0.0.1:{}", sqld.user_port),
                admin_base: format!("http://127.0.0.1:{}", sqld.admin_port),
                dir: dir.path().to_path_buf(),
                client,
            };
            run_owned(SCENARIO_DEADLINE, real_scenario(ctx)).await
        }
        Err(message) => {
            let stop = sqld.shutdown().await;
            panic!("sqld not ready: {message}; shutdown={stop:?}");
        }
    };
    // shutdown BEFORE propagating any scenario failure
    let stop = sqld.shutdown().await;
    eprintln!("provision-evidence: shutdown {stop:?}");
    let mut leaks = Vec::new();
    for name in ["sqld-stdout.log", "sqld-stderr.log"] {
        let bytes = bounded_read(&dir.path().join(name), LOG_READ_MAX);
        if has_bytes(&bytes, ADMIN_KEY.as_bytes()) || has_bytes(&bytes, b"eyJ") {
            leaks.push(name);
        }
    }
    match outcome {
        Ok(()) => {}
        Err(Some(join)) if join.is_panic() => std::panic::resume_unwind(join.into_panic()),
        Err(Some(join)) => panic!("scenario task failed: {join}"),
        Err(None) => panic!("scenario exceeded {SCENARIO_DEADLINE:?}"),
    }
    stop.expect("the owned sqld child is killed, reaped and gone");
    assert!(leaks.is_empty(), "child logs carry a secret: {leaks:?}");
}
