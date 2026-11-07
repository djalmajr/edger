#![cfg(unix)]
//! (EDG-54 / 15b) The dynamic private database guard: the provisioning
//! SERVICE (registry + vault + real `ensure`) wired to the REAL transport —
//! the single shared `EgressGuard` — for Deno (the private socket sink) and
//! WASM (the private transport + the dynamic builder).
//!
//! Levels of proof (the report keeps them apart):
//!
//! * non-ignored, no runtime: the object is built synchronously without I/O,
//!   the non-`Ready` states (absent / `Intent` / `Proven` / `Conflict`) deny
//!   BEFORE any outbound connection (a TCP spy on the origin sees nothing),
//!   a sink with another identity serves nothing.
//! * `#[ignore]` real runtimes (they FAIL, never skip, when the runtime is
//!   missing): SQLD 0.24.32 (`EDGER_SQLD_BIN`), the approved Go artifact
//!   (`EDGER_GO_WASM_FIXTURE`, SHA 8062d9cb…), Deno (`EDGER_DENO_BIN`).
//!   The grant is NEVER built by the test: `Ready` comes only from
//!   `SqldProvisionService::ensure` against the real SQLD, and the token is
//!   minted/renewed by the service.
//!
//! "Reconstructed" objects here are NOT process crashes: no test kills and
//! restarts the `edger` process. The product factory/config/CLI is not
//! wired; the tests assemble the host component only.
//!
//! ```text
//! EDGER_SQLD_BIN=…/sqld-0.24.32/bin/sqld EDGER_DENO_BIN=…/deno-2.9.8/bin/deno \
//! EDGER_GO_WASM_FIXTURE=…/go-ns-host-root-index.wasm \
//!   cargo test -p edger-orchestrator --test managed_database_egress -- --include-ignored
//! ```

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use bytes::Bytes;
use edger_core::egress::EgressCidr;
use edger_core::{
    parse_worker_config, EgressSettings, ExecutionKind, Isolate, SerializedRequest, WorkerManifest,
    WorkerRef, WorkerResponse,
};
use edger_isolation::egress::default_egress_limits;
use edger_isolation::wasm::{WasmHttpError, WasmHttpRequest, WasmHttpTransport};
use edger_isolation::{
    DenoProcessIsolate, EgressListenerContext, EgressListenerSink, EgressServeFuture,
    EGRESS_GUARD_HOST,
};
use edger_orchestrator::app_secrets::AppSecretVault;
use edger_orchestrator::egress_wasm_bridge::assemble_wasm_isolate_with_managed_database;
use edger_orchestrator::managed_database_egress::{
    ManagedDatabaseEgress, ManagedDatabaseEgressError,
};
use edger_orchestrator::observability::{
    OperationalEvent, OperationalEventQuery, OperationalStore,
};
use edger_orchestrator::sqld_admin::SqldAdminClient;
use edger_orchestrator::sqld_backup::SqldBackupStore;
use edger_orchestrator::sqld_provision::{
    SqldOwner, SqldProvisionError, SqldProvisionService, SqldSystemClock, SQLD_TOKEN_SECRET,
};
use edger_orchestrator::sqld_registry::{
    SqldProvisionRegistry, SqldRegistryOwner, SqldRegistryState, MIN_QUOTA_BYTES,
};
use edger_orchestrator::{EgressGuard, EgressGuardConfig};
use futures_util::StreamExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::ServerConfig;
use rustls::sign::CertifiedKey;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;
use zeroize::Zeroizing;

const ADMIN_KEY: &str = "canary-admin-managed-egress";
const LEDGER_MASTER: [u8; 32] = [0x42; 32];
const VAULT_MASTER: [u8; 32] = [0x24; 32];
const QUOTA: u64 = MIN_QUOTA_BYTES;
const BUDGET: u64 = 16 * MIN_QUOTA_BYTES;
const RBAC: &str = "@t";
const GO_FIXTURE_SHA: &str = "8062d9cbed8f48dd8f167d7805b2f83e30e401c0d3a072c9d82dc5f810f535b0";
const SCENARIO: Duration = Duration::from_secs(240);

// ───────────────────────── identities ─────────────────────────

fn worker_ref(name: &str, version: &str, namespace: Option<&str>) -> WorkerRef {
    let manifest = WorkerManifest {
        name: name.into(),
        entrypoint: Some("index.ts".into()),
        allow_net: Some(vec![]),
        ..WorkerManifest::default()
    };
    let config = parse_worker_config(&manifest);
    WorkerRef {
        id: uuid::Uuid::new_v4(),
        name: name.into(),
        version: version.into(),
        dir: PathBuf::from("/nonexistent"),
        namespace: namespace.map(str::to_owned),
        kind: ExecutionKind::FetchHandler,
        config,
    }
}

fn owner_of(worker: &WorkerRef) -> SqldOwner {
    SqldOwner::new(worker.name.clone(), worker.namespace.clone())
}

fn reg_owner(worker: &WorkerRef) -> SqldRegistryOwner<'_> {
    SqldRegistryOwner {
        worker: &worker.name,
        rbac_namespace: worker.namespace.as_deref(),
    }
}

// ───────────────────────── the service (real parts) ─────────────────────────

fn private_dir(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

struct Svc {
    _dir: tempfile::TempDir,
    registry: Arc<SqldProvisionRegistry>,
    vault: Arc<AppSecretVault>,
    service: Arc<SqldProvisionService>,
}

fn build_svc(admin_base: &str, data_base: &str) -> Svc {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    private_dir(dir.path(), "ledger");
    private_dir(dir.path(), "backups");
    let registry = Arc::new(
        SqldProvisionRegistry::open(
            &dir.path().join("ledger").join("ledger.db"),
            Zeroizing::new(LEDGER_MASTER),
            BUDGET,
        )
        .expect("ledger opens"),
    );
    let vault = Arc::new(
        AppSecretVault::open(dir.path().join("vault.db"), Zeroizing::new(VAULT_MASTER))
            .expect("vault opens"),
    );
    let admin = SqldAdminClient::new(admin_base, Zeroizing::new(ADMIN_KEY.to_owned())).unwrap();
    let backup = SqldBackupStore::new(data_base, dir.path().join("backups")).unwrap();
    let service = Arc::new(
        SqldProvisionService::new(
            Arc::clone(&registry),
            Arc::clone(&vault),
            admin,
            backup,
            data_base,
            Arc::new(SqldSystemClock),
        )
        .expect("service builds"),
    );
    Svc {
        _dir: dir,
        registry,
        vault,
        service,
    }
}

fn store_and_guard() -> (OperationalStore, Arc<EgressGuard>) {
    let store = OperationalStore::default();
    let guard =
        Arc::new(EgressGuard::new(EgressGuardConfig::default()).with_event_store(store.clone()));
    (store, guard)
}

fn egress_events(store: &OperationalStore) -> Vec<OperationalEvent> {
    store
        .query(OperationalEventQuery {
            kind: Some("worker.egress".into()),
            limit: Some(200),
            ..Default::default()
        })
        .events
}

fn event_for(store: &OperationalStore, request_id: &str) -> OperationalEvent {
    let mut found: Vec<_> = egress_events(store)
        .into_iter()
        .filter(|e| e.request_id.as_deref() == Some(request_id))
        .collect();
    assert_eq!(found.len(), 1, "exactly one event per call ({request_id})");
    found.remove(0)
}

/// No fixture secret appears anywhere in the serialized events.
fn assert_events_clean(store: &OperationalStore, secrets: &[&str]) {
    let text = serde_json::to_string(&egress_events(store)).unwrap();
    for secret in secrets {
        assert!(!secret.is_empty());
        assert!(!text.contains(secret), "an event carries a fixture secret");
    }
    assert!(
        !text.contains("Bearer"),
        "an event carries an authorization"
    );
    assert!(!text.contains("eyJ"), "an event carries a JWT");
}

// ───────────────────────── WASM request helpers ─────────────────────────

fn pipeline_request(base: &str, sql: &str, extra: &[(&str, String)], id: &str) -> WasmHttpRequest {
    let body = json!({"requests": [
        {"type": "execute", "stmt": {"sql": sql}}, {"type": "close"}]})
    .to_string();
    let mut headers = vec![("content-type".to_owned(), "application/json".to_owned())];
    for (k, v) in extra {
        headers.push(((*k).to_owned(), v.clone()));
    }
    WasmHttpRequest {
        method: "POST".into(),
        url: format!("{base}/v2/pipeline"),
        headers,
        body: Bytes::from(body),
        request_id: id.into(),
    }
}

fn first_cell(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let cell = value
        .pointer("/results/0/response/result/rows/0/0")?
        .get("value")?;
    Some(match cell {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

async fn sql(
    transport: &Arc<dyn WasmHttpTransport>,
    base: &str,
    statement: &str,
    extra: &[(&str, String)],
    id: &str,
) -> Result<Option<String>, WasmHttpError> {
    let response = transport
        .fetch(pipeline_request(base, statement, extra, id), 1 << 20)
        .await?;
    if response.status != 200 {
        return Ok(None);
    }
    Ok(first_cell(&response.body).or(Some(String::new())))
}

fn limits() -> EgressSettings {
    default_egress_limits()
}

// ───────────────────────── a TCP spy / fake data origin ─────────────────────────

/// One request head as lowercase `(name, value)` pairs.
type Headers = Vec<(String, String)>;

#[derive(Clone, Copy)]
enum Mode {
    Ok,
    Redirect,
    Big,
    Hang,
}

struct FakeOrigin {
    port: u16,
    connections: Arc<AtomicUsize>,
    heads: Arc<Mutex<Vec<Headers>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeOrigin {
    async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let heads: Arc<Mutex<Vec<Headers>>> = Arc::default();
        let (c, h) = (Arc::clone(&connections), Arc::clone(&heads));
        let task = tokio::spawn(async move {
            let mut conns = tokio::task::JoinSet::new();
            loop {
                let Ok((mut tcp, _)) = listener.accept().await else {
                    break;
                };
                c.fetch_add(1, Ordering::SeqCst);
                let h = Arc::clone(&h);
                conns.spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tcp.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let mut scratch = [httparse::EMPTY_HEADER; 64];
                    let mut req = httparse::Request::new(&mut scratch);
                    let _ = req.parse(&buf);
                    h.lock().unwrap().push(
                        req.headers
                            .iter()
                            .map(|x| {
                                (
                                    x.name.to_ascii_lowercase(),
                                    String::from_utf8_lossy(x.value).into_owned(),
                                )
                            })
                            .collect(),
                    );
                    match mode {
                        Mode::Ok => {
                            let _ = tcp
                                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                                .await;
                        }
                        Mode::Redirect => {
                            let _ = tcp
                                .write_all(b"HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:1/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                                .await;
                        }
                        Mode::Big => {
                            // No Content-Length: the cap is a mid-stream cut.
                            let _ = tcp
                                .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n")
                                .await;
                            let block = [b'x'; 4096];
                            for _ in 0..64 {
                                if tcp.write_all(&block).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Mode::Hang => {
                            tokio::time::sleep(Duration::from_secs(120)).await;
                        }
                    }
                    let _ = tcp.shutdown().await;
                });
            }
            conns.abort_all();
            while conns.join_next().await.is_some() {}
        });
        Self {
            port,
            connections,
            heads,
            task,
        }
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

// ───────────────────────── real SQLD ─────────────────────────

struct Sqld {
    child: Option<tokio::process::Child>,
    pid: u32,
    user_base: String,
    admin_base: String,
}

fn require_env_file(var: &str) -> String {
    let value = std::env::var(var)
        .unwrap_or_else(|_| panic!("{var} must name the real runtime; this test never skips"));
    assert!(Path::new(&value).is_file(), "{var} does not name a file");
    value
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

impl Sqld {
    async fn start(dir: &Path) -> Self {
        let bin = require_env_file("EDGER_SQLD_BIN");
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("ed25519 pair");
        let pub_path = dir.join("global.pub");
        std::fs::write(&pub_path, pair.public_key_pem()).unwrap();
        let (user_port, admin_port) = ephemeral_port_pair().await;
        assert_ne!(user_port, admin_port, "the two fixture ports are distinct");
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
            .stdout(std::fs::File::create(dir.join("sqld.out")).unwrap())
            .stderr(std::fs::File::create(dir.join("sqld.err")).unwrap())
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|e| panic!("cannot spawn the real sqld: {e}"));
        let pid = child.id().expect("child pid");
        let mut sqld = Self {
            child: Some(child),
            pid,
            user_base: format!("http://127.0.0.1:{user_port}"),
            admin_base: format!("http://127.0.0.1:{admin_port}"),
        };
        let client = direct_client();
        let url = format!("{}/v1/namespaces/edger_probe/config", sqld.admin_base);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(Ok(Some(status))) = sqld.child.as_mut().map(|c| c.try_wait()) {
                panic!("sqld exited during startup: {status:?}");
            }
            match client
                .get(&url)
                .header("authorization", format!("Basic {ADMIN_KEY}"))
                .send()
                .await
            {
                Ok(r) if r.status().as_u16() == 404 => break,
                Ok(r) => panic!("admin gate answered {} before ready", r.status()),
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await
                }
                Err(_) => panic!("the admin gate did not answer in time"),
            }
        }
        sqld
    }

    /// Kill + reap the OWNED child and prove its pid is gone.
    async fn shutdown(&mut self) -> String {
        let Some(mut child) = self.child.take() else {
            return "already stopped".into();
        };
        let kill = child.kill().await;
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("the owned sqld child is reaped within 10 s")
            .expect("wait");
        let pid = rustix::process::Pid::from_raw(self.pid as i32).expect("pid");
        let gone = matches!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        assert!(gone, "pid {} still answers after wait", self.pid);
        format!("kill={kill:?} wait={status:?} pid={} gone=true", self.pid)
    }
}

fn direct_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap()
}

/// A raw (host-side, not guard) SQLD call: the status of one pipeline.
async fn direct_pipeline(user_base: &str, ns: &str, token: &str, statement: &str) -> (u16, Value) {
    let body = json!({"requests": [
        {"type": "execute", "stmt": {"sql": statement}}, {"type": "close"}]});
    let response = direct_client()
        .post(format!("{user_base}/v2/pipeline"))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .header("x-namespace-bin", STANDARD_NO_PAD.encode(ns))
        .body(body.to_string())
        .send()
        .await
        .expect("direct pipeline completes");
    let status = response.status().as_u16();
    let bytes = response.bytes().await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn vault_token(svc: &Svc, worker: &WorkerRef) -> Option<String> {
    svc.vault
        .get(&worker.name, SQLD_TOKEN_SECRET)
        .map(|v| v.to_string())
}

/// An OWN token of the app signed by its ledger key, expired for real time
/// (now - 13h, ttl 12h => exp = now - 1h): the registry signer is the
/// product's own, the test only chooses the clock.
fn expired_own_token(svc: &Svc, worker: &WorkerRef) -> String {
    let now = chrono_now();
    svc.registry
        .sign_scoped_token(&reg_owner(worker), now - 13 * 3600, 12 * 3600)
        .expect("own expired token")
        .expose_jwt()
        .to_owned()
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn ensure_ready(svc: &Svc, worker: &WorkerRef) -> String {
    let outcome = svc
        .service
        .ensure(&owner_of(worker), QUOTA)
        .await
        .expect("ensure reaches Ready through the SERVICE (no manual markReady)");
    assert_eq!(outcome.metadata.state, SqldRegistryState::Ready);
    outcome.metadata.sql_namespace
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(SCENARIO, fut)
        .await
        .unwrap_or_else(|_| panic!("{what} exceeded {SCENARIO:?}"))
}

// ═════════════════════ non-ignored: no runtime needed ═════════════════════

/// The constructor is SYNCHRONOUS (a plain `#[test]`, no runtime exists):
/// no SQLite, no crypto, no network. Hostile origins and identities fail
/// closed with static errors.
#[test]
fn constructor_is_synchronous_validates_the_origin_and_the_identity() {
    let svc = build_svc("http://127.0.0.1:1", "http://127.0.0.1:2");
    let (_, guard) = store_and_guard();
    let worker = worker_ref("app-a", "1.0.0", Some(RBAC));
    let build = |worker: &WorkerRef, origin: &str| {
        ManagedDatabaseEgress::new(Arc::clone(&svc.service), Arc::clone(&guard), worker, origin)
    };
    let ok = build(&worker, "http://127.0.0.1:8080").expect("valid origin");
    assert_eq!(ok.origin().as_str(), "http://127.0.0.1:8080/");
    assert!(Arc::ptr_eq(ok.guard(), &guard), "the ONE shared guard");
    for bad in [
        "",
        "https://127.0.0.1:8080",
        "http://127.0.0.1:9090",
        "http://u:p@127.0.0.1:8080",
        "http://127.0.0.1:8080/x",
        "http://127.0.0.1:8080/?q=1",
        "http://127.0.0.1:8080/#f",
        "not a url",
        "ftp://127.0.0.1:8080",
    ] {
        assert!(
            matches!(
                build(&worker, bad),
                Err(ManagedDatabaseEgressError::InvalidOrigin)
            ),
            "{bad:?}"
        );
    }
    let empty = worker_ref("", "1.0.0", None);
    assert!(matches!(
        build(&empty, "http://127.0.0.1:8080"),
        Err(ManagedDatabaseEgressError::InvalidIdentity)
    ));
    // The bound identity is name + version + RBAC namespace, exactly.
    assert!(ok.binds(&worker));
    assert!(!ok.binds(&worker_ref("app-a", "1.0.1", Some(RBAC))));
    assert!(!ok.binds(&worker_ref("app-a", "1.0.0", None)));
    assert!(!ok.binds(&worker_ref("app-b", "1.0.0", Some(RBAC))));
    assert!(ok
        .wasm_transport(&worker_ref("app-b", "1.0.0", Some(RBAC)), limits())
        .is_none());
}

/// absent / Intent / Proven / Conflict (and a stored foreign token) deny
/// BEFORE any outbound connection: the origin spy never sees a connection,
/// the guard records ONE static event per call, and the service creates
/// nothing (no ensure, no CREATE, no token).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_ready_states_deny_without_any_outbound_and_without_creating() {
    let spy = FakeOrigin::start(Mode::Ok).await;
    let svc = build_svc("http://127.0.0.1:1", "http://127.0.0.1:2");
    let (store, guard) = store_and_guard();
    let key = [7u8; 32];

    let absent = worker_ref("st-absent", "1.0.0", Some(RBAC));
    let intent = worker_ref("st-intent", "1.0.0", Some(RBAC));
    let proven = worker_ref("st-proven", "1.0.0", Some(RBAC));
    let conflict = worker_ref("st-conflict", "1.0.0", Some(RBAC));
    for w in [&intent, &proven, &conflict] {
        svc.registry.ensure_intent(&reg_owner(w), QUOTA).unwrap();
    }
    let stored = svc.registry.get(&reg_owner(&proven)).unwrap().unwrap();
    svc.registry
        .mark_proven(&reg_owner(&proven), stored.public_key)
        .unwrap();
    svc.registry.mark_conflict(&reg_owner(&conflict)).unwrap();
    let _ = key;

    let mut n = 0;
    for (label, w, state) in [
        ("absent", &absent, None),
        ("intent", &intent, Some(SqldRegistryState::Intent)),
        ("proven", &proven, Some(SqldRegistryState::Proven)),
        ("conflict", &conflict, Some(SqldRegistryState::Conflict)),
    ] {
        let meta = svc.service.metadata(&owner_of(w)).await.unwrap();
        assert_eq!(meta.map(|m| m.state), state, "{label} state precondition");
        let egress = ManagedDatabaseEgress::new(
            Arc::clone(&svc.service),
            Arc::clone(&guard),
            w,
            &spy.base(),
        )
        .unwrap();
        let transport = egress.wasm_transport(w, limits()).unwrap();
        n += 1;
        let id = format!("deny-{n}");
        let result = transport
            .fetch(pipeline_request(&spy.base(), "SELECT 1", &[], &id), 4096)
            .await;
        assert!(result.is_err(), "{label}: a non-Ready binding is denied");
        let event = event_for(&store, &id);
        assert_eq!(
            event.code.as_deref(),
            Some("EGRESS_DATABASE_DENIED"),
            "{label}"
        );
        assert_eq!(event.worker.as_deref(), Some(w.name.as_str()));
    }
    assert_eq!(
        spy.connections.load(Ordering::SeqCst),
        0,
        "no outbound connection for any non-Ready state"
    );
    // Nothing was created: Intent stays Intent, absent stays absent, no
    // token reached the vault, the ledger is otherwise untouched.
    let after = |w: &WorkerRef| svc.registry.get(&reg_owner(w)).unwrap().map(|r| r.state);
    assert_eq!(after(&absent), None);
    assert_eq!(after(&intent), Some(SqldRegistryState::Intent));
    assert_eq!(after(&proven), Some(SqldRegistryState::Proven));
    assert_eq!(after(&conflict), Some(SqldRegistryState::Conflict));
    for w in [&absent, &intent, &proven, &conflict] {
        assert!(vault_token(&svc, w).is_none(), "no token was created");
    }
    assert_events_clean(&store, &[]);
    spy.stop().await;
}

/// A sink bound to ONE identity serves nothing for another (worker, version
/// or namespace): the listener is dropped, so the socket refuses
/// connections. The same identity keeps serving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sink_refuses_a_context_that_is_not_the_bound_identity() {
    let spy = FakeOrigin::start(Mode::Ok).await;
    let svc = build_svc("http://127.0.0.1:1", "http://127.0.0.1:2");
    let (store, guard) = store_and_guard();
    let bound = worker_ref("sink-a", "1.0.0", Some(RBAC));
    let egress = ManagedDatabaseEgress::new(
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        &bound,
        &spy.base(),
    )
    .unwrap();
    let sink = egress.listener_sink();
    let dir = tempfile::tempdir().unwrap();
    let ctx = |worker: &str, version: &str, ns: Option<&str>| EgressListenerContext {
        worker: worker.into(),
        version: version.into(),
        namespace: ns.map(str::to_owned),
        process_id: "p1".into(),
        settings: None,
        limits: limits(),
    };
    for (i, wrong) in [
        ctx("sink-b", "1.0.0", Some(RBAC)),
        ctx("sink-a", "2.0.0", Some(RBAC)),
        ctx("sink-a", "1.0.0", None),
        ctx("sink-a", "1.0.0", Some("@other")),
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.path().join(format!("wrong{i}.sock"));
        let listener = UnixListener::bind(&path).unwrap();
        let served = sink.serve(listener, wrong);
        let task = tokio::spawn(served);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("a refused context completes at once")
            .expect("join");
        assert!(
            UnixStream::connect(&path).await.is_err(),
            "case {i}: the socket of another identity is closed"
        );
    }
    // The bound identity is served (the listener stays open until aborted).
    let path = dir.path().join("ok.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let task = tokio::spawn(sink.serve(listener, ctx("sink-a", "1.0.0", Some(RBAC))));
    assert!(UnixStream::connect(&path).await.is_ok());
    task.abort();
    let _ = task.await;
    assert_eq!(spy.connections.load(Ordering::SeqCst), 0);
    assert!(egress_events(&store).is_empty());
    spy.stop().await;
}

// ═════════════════════ real SQLD: the WASM transport ═════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the operator-approved real sqld 0.24.32 (EDGER_SQLD_BIN)"]
async fn real_sqld_wasm_transport_two_apps_renewal_and_denials() {
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    eprintln!("managed-evidence: sqld pid={}", sqld.pid);
    let (user_base, admin_base) = (sqld.user_base.clone(), sqld.admin_base.clone());
    let outcome = tokio::spawn(wasm_scenario(user_base, admin_base)).await;
    let stop = sqld.shutdown().await;
    eprintln!("managed-evidence: shutdown {stop}");
    for name in ["sqld.out", "sqld.err"] {
        let bytes = std::fs::read(dir.path().join(name)).unwrap_or_default();
        assert!(
            !bytes.windows(3).any(|w| w == b"eyJ"),
            "{name} carries a JWT"
        );
    }
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
}

async fn wasm_scenario(user_base: String, admin_base: String) {
    within("wasm scenario", async move {
        let svc = build_svc(&admin_base, &user_base);
        let (store, guard) = store_and_guard();
        let a = worker_ref("app-a", "1.0.0", Some(RBAC));
        let b = worker_ref("app-b", "1.0.0", Some(RBAC));
        let (ns_a, ns_b) = (ensure_ready(&svc, &a).await, ensure_ready(&svc, &b).await);
        assert_ne!(ns_a, ns_b);
        let (tok_a, tok_b) = (
            vault_token(&svc, &a).expect("token A"),
            vault_token(&svc, &b).expect("token B"),
        );
        assert_ne!(tok_a, tok_b);

        let egress = |w: &WorkerRef| {
            ManagedDatabaseEgress::new(Arc::clone(&svc.service), Arc::clone(&guard), w, &user_base)
                .unwrap()
        };
        let (egress_a, egress_b) = (egress(&a), egress(&b));
        let ta = egress_a.wasm_transport(&a, limits()).unwrap();
        let tb = egress_b.wasm_transport(&b, limits()).unwrap();

        // Distinct data per app, written THROUGH the service authority.
        for (t, label, id) in [(&ta, "A-data", "w-a"), (&tb, "B-data", "w-b")] {
            sql(
                t,
                &user_base,
                "CREATE TABLE marker (v TEXT)",
                &[],
                &format!("{id}-c"),
            )
            .await
            .expect("create");
            sql(
                t,
                &user_base,
                &format!("INSERT INTO marker VALUES ('{label}')"),
                &[],
                &format!("{id}-i"),
            )
            .await
            .expect("insert");
        }
        let read = "SELECT v FROM marker";
        assert_eq!(
            sql(&ta, &user_base, read, &[], "r-a")
                .await
                .unwrap()
                .as_deref(),
            Some("A-data")
        );
        assert_eq!(
            sql(&tb, &user_base, read, &[], "r-b")
                .await
                .unwrap()
                .as_deref(),
            Some("B-data")
        );
        let ev = event_for(&store, "r-a");
        assert_eq!(ev.outcome.as_deref(), Some("ok"));
        assert_eq!(ev.status, Some(200));
        assert_eq!(ev.worker.as_deref(), Some("app-a"));

        // B's credential and namespace sent on A's call: the guard imposes
        // A's (A's data comes back; B's data never does).
        let hostile = [
            ("authorization", format!("Bearer {tok_b}")),
            ("x-namespace-bin", STANDARD_NO_PAD.encode(&ns_b)),
            ("cookie", format!("t={tok_b}")),
            ("proxy-authorization", format!("Bearer {tok_b}")),
        ];
        assert_eq!(
            sql(&ta, &user_base, read, &hostile, "x-a")
                .await
                .unwrap()
                .as_deref(),
            Some("A-data"),
            "guest credentials/namespace of B never give B's data"
        );
        // Control: the SAME hostile headers sent DIRECT to SQLD with the
        // wrong pairing (token A + B's namespace header) are refused, so
        // the pairing above was really the guard's.
        let (status, _) = direct_pipeline(&user_base, &ns_b, &tok_a, read).await;
        assert_ne!(status, 200, "token A is not valid for namespace B");

        // Another URL on the same host/other port / another scheme: denied.
        for (i, url) in [
            "http://127.0.0.1:9/v2/pipeline".to_owned(),
            user_base.replace("http://", "https://") + "/v2/pipeline",
            user_base.replace("127.0.0.1", "localhost") + "/v2/pipeline",
            user_base.replace("http://", "http://u:p@") + "/v2/pipeline",
        ]
        .into_iter()
        .enumerate()
        {
            let mut request = pipeline_request(&user_base, read, &[], &format!("o-{i}"));
            request.url = url;
            assert!(ta.fetch(request, 4096).await.is_err(), "other origin {i}");
            assert_eq!(
                event_for(&store, &format!("o-{i}")).code.as_deref(),
                Some("EGRESS_DATABASE_DENIED")
            );
        }

        // ── renewal: an own EXPIRED token in the vault ──
        let expired = expired_own_token(&svc, &a);
        svc.vault.set(&a.name, SQLD_TOKEN_SECRET, &expired).unwrap();
        let (status, _) = direct_pipeline(&user_base, &ns_a, &expired, read).await;
        assert_eq!(status, 401, "the expired token is refused by SQLD directly");
        assert_eq!(
            sql(&ta, &user_base, read, &[], "renew-a")
                .await
                .unwrap()
                .as_deref(),
            Some("A-data"),
            "the service renewed the token on the next access"
        );
        let renewed = vault_token(&svc, &a).expect("renewed token stored");
        assert_ne!(renewed, expired);
        // (a JWT without jti minted in the same second as the first one can
        // be byte-identical to it: only the expired value must differ)
        let (status, body) = direct_pipeline(&user_base, &ns_a, &renewed, read).await;
        assert_eq!(status, 200, "the renewed token works on SQLD directly");
        assert_eq!(
            first_cell(body.to_string().as_bytes()).as_deref(),
            Some("A-data")
        );
        let (status, _) = direct_pipeline(&user_base, &ns_a, &expired, read).await;
        assert_eq!(status, 401, "the old token stays refused after the renewal");
        // B's vault value is untouched by A's renewal.
        assert_eq!(vault_token(&svc, &b), Some(tok_b.clone()));

        // ── a manual token: denied, preserved, no reseed, no general fallback ──
        svc.vault
            .set(&a.name, SQLD_TOKEN_SECRET, "manual-token-value")
            .unwrap();
        assert!(sql(&ta, &user_base, read, &[], "manual").await.is_err());
        assert_eq!(
            event_for(&store, "manual").code.as_deref(),
            Some("EGRESS_DATABASE_AUTH_UNAVAILABLE")
        );
        assert_eq!(
            vault_token(&svc, &a).as_deref(),
            Some("manual-token-value"),
            "a manual value is preserved (no overwrite)"
        );
        // ── DELETE: denied, NOT reseeded ──
        assert!(svc.vault.delete(&a.name, SQLD_TOKEN_SECRET).unwrap());
        assert!(sql(&ta, &user_base, read, &[], "deleted").await.is_err());
        assert_eq!(
            event_for(&store, "deleted").code.as_deref(),
            Some("EGRESS_DATABASE_AUTH_UNAVAILABLE")
        );
        assert!(
            vault_token(&svc, &a).is_none(),
            "no auto-reseed after DELETE"
        );
        assert!(sql(&ta, &user_base, read, &[], "deleted2").await.is_err());
        assert!(vault_token(&svc, &a).is_none());

        // ── remote key conflict: denied, no new namespace, values preserved ──
        svc.registry.mark_conflict(&reg_owner(&b)).unwrap();
        assert!(sql(&tb, &user_base, read, &[], "conflict").await.is_err());
        assert_eq!(
            event_for(&store, "conflict").code.as_deref(),
            Some("EGRESS_DATABASE_DENIED")
        );
        assert!(matches!(
            svc.service.ensure(&owner_of(&b), QUOTA).await,
            Err(SqldProvisionError::KeyConflict)
        ));
        assert_eq!(
            vault_token(&svc, &b),
            Some(tok_b.clone()),
            "B's value preserved"
        );
        // The ledger still names the SAME namespace (no new one was derived).
        let meta = svc.service.metadata(&owner_of(&b)).await.unwrap().unwrap();
        assert_eq!(meta.sql_namespace, ns_b);
        assert_eq!(meta.state, SqldRegistryState::Conflict);

        assert_events_clean(&store, &[&tok_a, &tok_b, &renewed, &expired]);
    })
    .await;
}

// ═════════════════════ real SQLD + the approved Go artifact ═════════════════════

fn go_fixture() -> PathBuf {
    let raw = std::env::var("EDGER_GO_WASM_FIXTURE").unwrap_or_else(|_| {
        panic!("EDGER_GO_WASM_FIXTURE must name the real Go build artifact; this test never skips")
    });
    let path = PathBuf::from(&raw);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {raw}: {e}"));
    assert_eq!(&bytes[..4], b"\0asm", "not a wasm binary");
    let sha: String = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    assert_eq!(sha, GO_FIXTURE_SHA, "the Go artifact identity is immutable");
    path
}

fn go_ref(name: &str, namespace: &str, wasm: &Path, dir: &Path, sqld_url: &str) -> WorkerRef {
    std::fs::copy(wasm, dir.join("index.wasm")).expect("copy the Go artifact");
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../workers/examples/go-wasm-http/manifest.yaml"),
    )
    .unwrap();
    let text = text
        .replace("name: go-wasm-http\n", &format!("name: \"{name}\"\n"))
        .replace(
            "  SQLD_URL: \"\"\n",
            &format!("  SQLD_URL: \"{sqld_url}\"\n"),
        )
        .replace("timeout: 30s\n", "timeout: 60s\n");
    let options = serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins,
        with_snippet: false,
    };
    let manifest: WorkerManifest = serde_saphyr::from_str_with_options(&text, options).unwrap();
    let mut config = parse_worker_config(&manifest);
    config.worker_dir = Some(dir.to_path_buf());
    assert!(
        config.egress.is_none(),
        "the manifest declares no general egress"
    );
    WorkerRef {
        id: uuid::Uuid::new_v4(),
        name: name.into(),
        version: manifest.version.clone().expect("version"),
        dir: dir.to_path_buf(),
        namespace: Some(namespace.into()),
        kind: config.kind.clone().unwrap_or(ExecutionKind::FetchHandler),
        config,
    }
}

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real sqld 0.24.32 (EDGER_SQLD_BIN) and the approved Go artifact (EDGER_GO_WASM_FIXTURE)"]
async fn real_go_wasm_reaches_its_database_through_the_service_authority() {
    let wasm = go_fixture();
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    let (user_base, admin_base) = (sqld.user_base.clone(), sqld.admin_base.clone());
    let wasm2 = wasm.clone();
    let outcome = tokio::spawn(async move {
        within("go scenario", go_scenario(wasm2, user_base, admin_base)).await
    })
    .await;
    let stop = sqld.shutdown().await;
    eprintln!("managed-evidence: go shutdown {stop}");
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
}

async fn go_scenario(wasm: PathBuf, user_base: String, admin_base: String) {
    const DB_OK: &str = r#"{"ok":true,"value":1}"#;
    const DB_503: &str = r#"{"error":"configuration missing"}"#;
    let svc = build_svc(&admin_base, &user_base);
    let (store, guard) = store_and_guard();
    let (dir_a, dir_c) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let go_a = go_ref("go-wasm-http", "nsGo", &wasm, dir_a.path(), &user_base);
    let go_c = go_ref("go-wasm-http-c", "nsGoC", &wasm, dir_c.path(), &user_base);
    let ns_a = ensure_ready(&svc, &go_a).await;
    // C stays at Intent (never ensured to Ready): its binding is denied.
    svc.registry
        .ensure_intent(&reg_owner(&go_c), QUOTA)
        .unwrap();

    let build = |w: &WorkerRef| {
        let egress =
            ManagedDatabaseEgress::new(Arc::clone(&svc.service), Arc::clone(&guard), w, &user_base)
                .unwrap();
        let resolver: Arc<dyn edger_core::AppSecretResolver> =
            svc.service.resolver_for(owner_of(w));
        assemble_wasm_isolate_with_managed_database(
            w,
            Some(resolver),
            Some(Arc::clone(&guard)),
            Some(&egress),
        )
    };
    // ONE isolate per worker (never one per request).
    let mut iso_a = build(&go_a);
    let mut iso_c = build(&go_c);

    let run = |id: &'static str| host_request(id);
    let res = iso_a
        .execute_wasm(run("go-a-1"), &go_a.config)
        .await
        .expect("A /db");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.as_deref(), Some(DB_OK.as_bytes()));
    let ev = event_for(&store, "go-a-1");
    assert_eq!(ev.outcome.as_deref(), Some("ok"));
    assert_eq!(ev.worker.as_deref(), Some("go-wasm-http"));
    assert_eq!(ev.namespace.as_deref(), Some("nsGo"));

    // Renewal under the SAME isolate: an expired own token in the vault.
    let expired = expired_own_token(&svc, &go_a);
    svc.vault
        .set(&go_a.name, SQLD_TOKEN_SECRET, &expired)
        .unwrap();
    let (status, _) = direct_pipeline(&user_base, &ns_a, &expired, "SELECT 1").await;
    assert_eq!(status, 401);
    let res = iso_a
        .execute_wasm(run("go-a-2"), &go_a.config)
        .await
        .expect("A /db again");
    assert_eq!(res.body.as_deref(), Some(DB_OK.as_bytes()));
    assert_ne!(vault_token(&svc, &go_a).unwrap(), expired);

    // Intent binding: the service resolver gives the guest NO SQLD_TOKEN
    // (it never creates or semeia one), so the Go module answers its static
    // 503 before any outbound call — and nothing is created.
    let res = iso_c
        .execute_wasm(run("go-c-1"), &go_c.config)
        .await
        .expect("C /db");
    assert_eq!(res.status, 503);
    assert_eq!(res.body.as_deref(), Some(DB_503.as_bytes()));
    assert!(
        egress_events(&store)
            .iter()
            .all(|e| e.request_id.as_deref() != Some("go-c-1")),
        "no guard call was made (the guest had no credential to send)"
    );
    assert_eq!(
        svc.registry.get(&reg_owner(&go_c)).unwrap().unwrap().state,
        SqldRegistryState::Intent
    );
    assert!(vault_token(&svc, &go_c).is_none());
    // Not wired: this is the host component, not the product factory/CLI.
}

// ═════════════════════ fake data origin: guard enforcement ═════════════════════

/// The origin is a fake HTTP server (the service is REAL and Ready against
/// SQLD, so the namespace/token are the service's): the guard imposes the
/// namespace/Bearer of the SERVICE and strips the guest's; no redirect is
/// followed; the byte cap, the redirect and the deadline refuse statically.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the operator-approved real sqld 0.24.32 (EDGER_SQLD_BIN)"]
async fn real_service_credentials_are_imposed_on_a_fake_origin_and_limits_hold() {
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    let (user_base, admin_base) = (sqld.user_base.clone(), sqld.admin_base.clone());
    let outcome = tokio::spawn(async move {
        within(
            "fake origin scenario",
            fake_origin_scenario(user_base, admin_base),
        )
        .await
    })
    .await;
    let stop = sqld.shutdown().await;
    eprintln!("managed-evidence: fake-origin shutdown {stop}");
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
}

async fn fake_origin_scenario(user_base: String, admin_base: String) {
    let svc = build_svc(&admin_base, &user_base);
    let (store, guard) = store_and_guard();
    let a = worker_ref("fo-a", "1.0.0", Some(RBAC));
    let ns_a = ensure_ready(&svc, &a).await;
    let tok_a = vault_token(&svc, &a).unwrap();
    let make = |origin: &str, limits: EgressSettings| {
        ManagedDatabaseEgress::new(Arc::clone(&svc.service), Arc::clone(&guard), &a, origin)
            .unwrap()
            .wasm_transport(&a, limits)
            .unwrap()
    };

    // 1. impose: the guest's credentials are replaced by the service's.
    let ok = FakeOrigin::start(Mode::Ok).await;
    let t = make(&ok.base(), limits());
    let mut request = pipeline_request(
        &ok.base(),
        "SELECT 1",
        &[
            ("authorization", "Bearer guest-secret-1".into()),
            ("x-namespace-bin", "Z3Vlc3Q".into()),
            ("cookie", "guest=1".into()),
        ],
        "impose",
    );
    request.url.push_str("?q=guest-query-secret");
    let response = t.fetch(request, 4096).await.expect("fake origin answers");
    assert_eq!(response.status, 200);
    let heads = ok.heads.lock().unwrap().clone();
    assert_eq!(heads.len(), 1, "one upstream call");
    let value = |name: &str| -> Vec<String> {
        heads[0]
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .collect()
    };
    assert_eq!(value("authorization"), vec![format!("Bearer {tok_a}")]);
    assert_eq!(
        value("x-namespace-bin"),
        vec![STANDARD_NO_PAD.encode(&ns_a)]
    );
    assert!(value("cookie").is_empty());
    assert!(value("proxy-authorization").is_empty());
    let ev = event_for(&store, "impose");
    assert_eq!(ev.outcome.as_deref(), Some("ok"));
    let events_text = serde_json::to_string(&egress_events(&store)).unwrap();
    for secret in ["guest-secret-1", "guest-query-secret", &tok_a, "Z3Vlc3Q"] {
        assert!(!events_text.contains(secret), "event leak");
    }
    ok.stop().await;

    // 2. redirect: refused, never followed (the target gets no connection).
    let redirect = FakeOrigin::start(Mode::Redirect).await;
    let t = make(&redirect.base(), limits());
    assert!(t
        .fetch(
            pipeline_request(&redirect.base(), "SELECT 1", &[], "redir"),
            4096
        )
        .await
        .is_err());
    assert_eq!(
        event_for(&store, "redir").code.as_deref(),
        Some("EGRESS_DATABASE_DENIED")
    );
    assert_eq!(
        redirect.connections.load(Ordering::SeqCst),
        1,
        "one hop only"
    );
    redirect.stop().await;

    // 3. response byte cap: a body past the guest cap is a clean error.
    let big = FakeOrigin::start(Mode::Big).await;
    let t = make(&big.base(), limits());
    let err = t
        .fetch(pipeline_request(&big.base(), "SELECT 1", &[], "big"), 1000)
        .await;
    assert!(matches!(err, Err(WasmHttpError::ResponseTooLarge)));
    assert_eq!(
        event_for(&store, "big").outcome.as_deref(),
        Some("error"),
        "the cut is one error event"
    );
    big.stop().await;

    // 4. the total deadline: a hanging origin ends the call by the budget.
    let hang = FakeOrigin::start(Mode::Hang).await;
    let mut tight = limits();
    tight.headers_timeout_ms = 400;
    tight.total_timeout_ms = 800;
    let t = make(&hang.base(), tight);
    let started = std::time::Instant::now();
    assert!(t
        .fetch(
            pipeline_request(&hang.base(), "SELECT 1", &[], "hang"),
            4096
        )
        .await
        .is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        event_for(&store, "hang").outcome.as_deref(),
        Some("timeout")
    );
    hang.stop().await;

    // 5. the lookup deadline is inside the guard: an expired-and-unrenewable
    //    path still ends within the total budget (no hang, one event).
    assert!(vault_token(&svc, &a).is_some());
}

// ═════════════════════ real SQLD + Deno: the private socket ═════════════════════

const DENO_WORKER: &str = r#"
const db = Deno.env.get("SQLD_URL");
const staleToken = Deno.env.get("STALE_TOKEN") ?? "";
const other = Deno.env.get("OTHER_ORIGIN") ?? "";
async function pipeline(sql, headers) {
  const r = await fetch(db + "/v2/pipeline", {
    method: "POST",
    headers: { "content-type": "application/json", ...headers },
    body: JSON.stringify({ requests: [{ type: "execute", stmt: { sql } }, { type: "close" }] }),
  });
  return { status: r.status, err: r.headers.get("x-edger-egress-error"), text: await r.text() };
}
async function failure(f) {
  try { await f(); return "no-error"; } catch (e) { return e.name; }
}
Deno.serve(async (req) => {
  const u = new URL(req.url);
  const sql = u.searchParams.get("sql") ?? "SELECT 1";
  switch (u.pathname) {
    case "/q":
      return Response.json(await pipeline(sql, {}));
    case "/q-guest":
      return Response.json(await pipeline(sql, {
        authorization: "Bearer " + (u.searchParams.get("tok") ?? ""),
        "x-namespace-bin": u.searchParams.get("ns") ?? "",
      }));
    case "/q-stale-env":
      return Response.json(await pipeline(sql, { authorization: "Bearer " + staleToken }));
    case "/nondb": {
      const d = new URL(db);
      return Response.json({
        rawTcp: await failure(() => Deno.connect({ hostname: d.hostname, port: Number(d.port) })),
        otherHost: await failure(() => fetch(other + "/")),
        external: await failure(() => fetch("https://example.com/")),
      });
    }
    case "/bare":
      return Response.json({ fetch: await failure(() => pipeline("SELECT 1", { authorization: "Bearer " + staleToken })) });
    case "/pid":
      return Response.json({ pid: Deno.pid });
    default:
      return new Response("not found", { status: 404 });
  }
});
"#;

/// Wraps a real sink and records each served context (the process id).
struct RecSink {
    inner: Arc<dyn EgressListenerSink>,
    contexts: Mutex<Vec<EgressListenerContext>>,
}

impl EgressListenerSink for RecSink {
    fn serve(&self, listener: UnixListener, context: EgressListenerContext) -> EgressServeFuture {
        self.contexts.lock().unwrap().push(context.clone());
        self.inner.serve(listener, context)
    }
}

struct DenoFx {
    _dir: tempfile::TempDir,
    _cache: tempfile::TempDir,
    isolate: DenoProcessIsolate,
    config: edger_core::WorkerConfig,
}

fn deno_fx(worker: &WorkerRef, env: HashMap<String, String>) -> DenoFx {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.ts"), DENO_WORKER).unwrap();
    let mut config = worker.config.clone();
    config.worker_dir = Some(dir.path().to_path_buf());
    config.timeout_ms = 30_000;
    config.env = env;
    DenoFx {
        isolate: DenoProcessIsolate::new().with_per_worker_deno_cache_root(cache.path()),
        _dir: dir,
        _cache: cache,
        config,
    }
}

impl DenoFx {
    async fn get(&mut self, uri: &str) -> Value {
        let request = SerializedRequest {
            method: "GET".into(),
            uri: uri.into(),
            headers: vec![],
            body: None,
            request_id: "deno-test".into(),
            base_href: None,
        };
        let config = self.config.clone();
        let response = self
            .isolate
            .execute_fetch(request, &config)
            .await
            .unwrap_or_else(|e| panic!("{uri}: {e:?}"));
        assert_eq!(response.status, 200, "{uri}");
        serde_json::from_slice(response.body.as_deref().unwrap_or_default()).unwrap()
    }
}

fn pct(sql: &str) -> String {
    sql.replace(' ', "%20")
        .replace('\'', "%27")
        .replace('*', "%2A")
        .replace('+', "%2B")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real sqld 0.24.32 (EDGER_SQLD_BIN) and real Deno (EDGER_DENO_BIN)"]
async fn real_deno_reaches_its_database_through_the_private_socket() {
    let exe = std::env::var("EDGER_DENO_BIN").unwrap_or_else(|_| "deno".into());
    let out = std::process::Command::new(&exe)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("real Deno is required (EDGER_DENO_BIN): {exe}: {e}"));
    assert!(out.status.success());
    eprintln!(
        "managed-evidence: {}",
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
    );
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    let (user_base, admin_base) = (sqld.user_base.clone(), sqld.admin_base.clone());
    let outcome =
        tokio::spawn(
            async move { within("deno scenario", deno_scenario(user_base, admin_base)).await },
        )
        .await;
    let stop = sqld.shutdown().await;
    eprintln!("managed-evidence: deno shutdown {stop}");
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
}

async fn deno_scenario(user_base: String, admin_base: String) {
    let svc = build_svc(&admin_base, &user_base);
    let (store, guard) = store_and_guard();
    let a = worker_ref("dn-a", "1.0.0", Some(RBAC));
    let b = worker_ref("dn-b", "1.0.0", Some(RBAC));
    let (ns_a, ns_b) = (ensure_ready(&svc, &a).await, ensure_ready(&svc, &b).await);
    let (tok_a, tok_b) = (
        vault_token(&svc, &a).unwrap(),
        vault_token(&svc, &b).unwrap(),
    );
    let other = FakeOrigin::start(Mode::Ok).await;

    // Distinct data per app through the service authority (WASM transport).
    let egress_a =
        ManagedDatabaseEgress::new(Arc::clone(&svc.service), Arc::clone(&guard), &a, &user_base)
            .unwrap();
    let egress_b =
        ManagedDatabaseEgress::new(Arc::clone(&svc.service), Arc::clone(&guard), &b, &user_base)
            .unwrap();
    for (egress, w, value) in [(&egress_a, &a, "A-data"), (&egress_b, &b, "B-data")] {
        let t = egress.wasm_transport(w, limits()).unwrap();
        for (i, statement) in [
            "CREATE TABLE marker (v TEXT)".to_owned(),
            format!("INSERT INTO marker VALUES ('{value}')"),
        ]
        .iter()
        .enumerate()
        {
            sql(
                &t,
                &user_base,
                statement,
                &[],
                &format!("seed-{}-{i}", w.name),
            )
            .await
            .unwrap();
        }
    }

    // The stale env token: an EXPIRED own token the process "kept" from its
    // spawn (the guest sends it explicitly); the vault gets an expired one
    // too, so the first access renews through the service.
    let expired = expired_own_token(&svc, &a);
    svc.vault.set(&a.name, SQLD_TOKEN_SECRET, &expired).unwrap();
    let (status, _) = direct_pipeline(&user_base, &ns_a, &expired, "SELECT 1").await;
    assert_eq!(
        status, 401,
        "the process's old token is refused by SQLD directly"
    );

    let rec = Arc::new(RecSink {
        inner: egress_a.listener_sink(),
        contexts: Mutex::new(Vec::new()),
    });
    let mut fx = deno_fx(
        &a,
        HashMap::from([
            ("SQLD_URL".to_owned(), user_base.clone()),
            ("STALE_TOKEN".to_owned(), expired.clone()),
            ("OTHER_ORIGIN".to_owned(), other.base()),
        ]),
    );
    fx.isolate
        .set_managed_database_egress(
            rec.clone(),
            a.name.clone(),
            a.version.clone(),
            a.namespace.clone(),
            user_base.clone(),
        )
        .expect("arm the private seam");

    let result = std::panic::AssertUnwindSafe(async {
        let read = pct("SELECT v FROM marker");
        // The app fetches SQLD_URL: Ready database + NO general egress.
        let v = fx.get(&format!("/q?sql={read}")).await;
        assert_eq!(v["status"], 200, "{v}");
        assert_eq!(
            first_cell(v["text"].as_str().unwrap().as_bytes()).as_deref(),
            Some("A-data")
        );
        let pid0 = fx.get("/pid").await["pid"].as_u64().unwrap();

        // The guest sends B's token and namespace: A's data still.
        let v = fx
            .get(&format!(
                "/q-guest?sql={read}&tok={tok_b}&ns={}",
                STANDARD_NO_PAD.encode(&ns_b)
            ))
            .await;
        assert_eq!(v["status"], 200, "{v}");
        assert_eq!(
            first_cell(v["text"].as_str().unwrap().as_bytes()).as_deref(),
            Some("A-data")
        );

        // The process keeps its OLD (expired) env token and PID; the guard
        // uses the service's FRESH token (renewed in the vault).
        let v = fx.get(&format!("/q-stale-env?sql={read}")).await;
        assert_eq!(v["status"], 200, "{v}");
        assert_eq!(
            first_cell(v["text"].as_str().unwrap().as_bytes()).as_deref(),
            Some("A-data")
        );
        let renewed = vault_token(&svc, &a).unwrap();
        assert_ne!(renewed, expired, "the service renewed the token");
        assert_eq!(
            fx.get("/pid").await["pid"].as_u64().unwrap(),
            pid0,
            "no restart"
        );
        assert_eq!(
            rec.contexts.lock().unwrap().len(),
            1,
            "one socket, one process"
        );
        assert!(
            rec.contexts.lock().unwrap()[0].settings.is_none(),
            "no general egress"
        );

        // Raw TCP / another host / external are negated by the runtime.
        let v = fx.get("/nondb").await;
        for key in ["rawTcp", "otherHost", "external"] {
            assert_eq!(v[key], "NotCapable", "{key}: {v}");
        }
        assert_eq!(
            other.connections.load(Ordering::SeqCst),
            0,
            "the other origin saw nothing"
        );

        // A manual token denies (no reseed, no general fallback, preserved).
        svc.vault
            .set(&a.name, SQLD_TOKEN_SECRET, "manual-token-value")
            .unwrap();
        let v = fx.get(&format!("/q?sql={read}")).await;
        assert_eq!(v["err"], "EGRESS_DATABASE_AUTH_UNAVAILABLE", "{v}");
        assert_eq!(vault_token(&svc, &a).as_deref(), Some("manual-token-value"));
        // DELETE denies too and nothing is reseeded.
        svc.vault.delete(&a.name, SQLD_TOKEN_SECRET).unwrap();
        let v = fx.get(&format!("/q?sql={read}")).await;
        assert_eq!(v["err"], "EGRESS_DATABASE_AUTH_UNAVAILABLE", "{v}");
        assert!(vault_token(&svc, &a).is_none());
        // A remote key conflict denies without creating a namespace.
        svc.registry.mark_conflict(&reg_owner(&a)).unwrap();
        let v = fx.get(&format!("/q?sql={read}")).await;
        assert_eq!(v["err"], "EGRESS_DATABASE_DENIED", "{v}");
        assert_eq!(
            svc.service
                .metadata(&owner_of(&a))
                .await
                .unwrap()
                .unwrap()
                .sql_namespace,
            ns_a
        );
    })
    .await_unwind()
    .await;

    // Cleanup BEFORE re-raising: terminate the child (awaited), stop spies.
    let terminated = fx.isolate.terminate().await;
    other.stop().await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    terminated.expect("terminate");

    // Without the private setter a guest SQLD_URL/token/header creates NO
    // authority: the same worker, same env, nothing armed.
    let mut bare = deno_fx(
        &b,
        HashMap::from([
            ("SQLD_URL".to_owned(), user_base.clone()),
            ("STALE_TOKEN".to_owned(), tok_b.clone()),
            ("OTHER_ORIGIN".to_owned(), user_base.clone()),
        ]),
    );
    let result = std::panic::AssertUnwindSafe(async {
        let v = bare.get("/bare").await;
        assert_eq!(
            v["fetch"], "NotCapable",
            "a guest SQLD_URL/token/header with nothing armed creates no authority: {v}"
        );
    })
    .await_unwind()
    .await;
    let terminated = bare.isolate.terminate().await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    terminated.expect("terminate bare");
    assert_events_clean(&store, &[&tok_a, &tok_b, &expired]);
}

// A tiny catch-unwind adapter so cleanup always runs.
trait AwaitUnwind: Future + Sized {
    fn await_unwind(self) -> UnwindFut<Self>;
}
impl<F: Future> AwaitUnwind for std::panic::AssertUnwindSafe<F> {
    fn await_unwind(self) -> UnwindFut<Self> {
        UnwindFut(Box::pin(self))
    }
}
struct UnwindFut<F>(std::pin::Pin<Box<F>>);
impl<F: Future<Output = ()>> Future for UnwindFut<F> {
    type Output = std::thread::Result<()>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.0.as_mut().poll(cx))) {
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Ok(std::task::Poll::Ready(())) => std::task::Poll::Ready(Ok(())),
            Err(payload) => std::task::Poll::Ready(Err(payload)),
        }
    }
}

// ═════════════════════ None/Some + cap 1 on the managed socket ═════════════════════

struct TestPki {
    ca_der: Vec<u8>,
    server_der: Vec<u8>,
    server_key_der: Vec<u8>,
}

fn test_pki() -> &'static TestPki {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose, SanType,
    };
    static PKI: OnceLock<TestPki> = OnceLock::new();
    PKI.get_or_init(|| {
        let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "edger-managed-egress");
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let mut server_params = CertificateParams::new(Vec::<String>::new()).unwrap();
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

#[derive(Debug)]
struct StaticCert(Arc<CertifiedKey>);
impl rustls::server::ResolvesServerCert for StaticCert {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// A local HTTPS destination: `/ok` answers at once, `/sse` sends one chunk
/// then holds the response (and the guard's slot) until `release`.
struct TlsUpstream {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    release: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

async fn tls_upstream() -> TlsUpstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let pki = test_pki();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let key = PrivateKeyDer::try_from(pki.server_key_der.clone()).unwrap();
    let certified = CertifiedKey::from_der(
        vec![CertificateDer::from(pki.server_der.clone())],
        key,
        &provider,
    )
    .unwrap();
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(StaticCert(Arc::new(certified))));
    let acceptor = Arc::new(TlsAcceptor::from(Arc::new(config)));
    let connections = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let (c, r) = (Arc::clone(&connections), Arc::clone(&release));
    let task = tokio::spawn(async move {
        let mut conns = tokio::task::JoinSet::new();
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            c.fetch_add(1, Ordering::SeqCst);
            let (acceptor, r) = (Arc::clone(&acceptor), Arc::clone(&r));
            conns.spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 2048];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tls.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                if head.contains("GET /sse") {
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n")
                        .await;
                    let _ = tls.write_all(b"3\r\none\r\n").await;
                    let _ = tls.flush().await;
                    r.notified().await;
                    let _ = tls.write_all(b"3\r\ntwo\r\n0\r\n\r\n").await;
                } else {
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 12\r\nconnection: close\r\n\r\nhello-egress")
                        .await;
                }
                let _ = tls.shutdown().await;
            });
        }
        conns.abort_all();
        while conns.join_next().await.is_some() {}
    });
    TlsUpstream {
        addr,
        connections,
        release,
        task,
    }
}

fn tls_guard(store: &OperationalStore) -> Arc<EgressGuard> {
    let mut config = EgressGuardConfig::default();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(test_pki().ca_der.clone()))
        .unwrap();
    config.tls_roots = roots;
    // Test-only release of the loopback for the GENERAL path; the private
    // database authority never needed it.
    config.allow_cidrs = vec![EgressCidr::parse("127.0.0.0/8").expect("cidr")];
    Arc::new(EgressGuard::new(config).with_event_store(store.clone()))
}

struct RawResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl RawResponse {
    fn error_code(&self) -> Option<String> {
        self.headers
            .iter()
            .find(|(k, _)| k == "x-edger-egress-error")
            .map(|(_, v)| v.clone())
    }
}

fn split_head(buf: &[u8]) -> Option<(usize, u16, Headers)> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let mut scratch = [httparse::EMPTY_HEADER; 64];
    let mut res = httparse::Response::new(&mut scratch);
    httparse::Response::parse(&mut res, &buf[..end]).ok()?;
    Some((
        end,
        res.code?,
        res.headers
            .iter()
            .map(|h| {
                (
                    h.name.to_ascii_lowercase(),
                    String::from_utf8_lossy(h.value).into_owned(),
                )
            })
            .collect(),
    ))
}

async fn send_raw(
    stream: &mut UnixStream,
    method: &str,
    target: &str,
    body: &[u8],
    extra: &[(&str, &str)],
) {
    let mut head = format!(
        "{method} /guard HTTP/1.1\r\nHost: {EGRESS_GUARD_HOST}\r\nx-edger-egress-url: {target}\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
}

/// Read the head and (for a content-length/chunked answer) the whole body.
async fn read_raw(stream: &mut UnixStream) -> RawResponse {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let (end, status, headers) = split_head(&buf).expect("a response head");
    let raw = &buf[end..];
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.contains("chunked"));
    let body = if chunked { dechunk(raw) } else { raw.to_vec() };
    RawResponse {
        status,
        headers,
        body,
    }
}

fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(pos) = raw.windows(2).position(|w| w == b"\r\n") {
        let size =
            usize::from_str_radix(std::str::from_utf8(&raw[..pos]).unwrap_or("0").trim(), 16)
                .unwrap_or(0);
        if size == 0 || raw.len() < pos + 2 + size {
            break;
        }
        out.extend_from_slice(&raw[pos + 2..pos + 2 + size]);
        raw = &raw[(pos + 2 + size + 2).min(raw.len())..];
    }
    out
}

/// A served managed socket (the sink future runs as a task owned by the
/// test and ALWAYS aborted and awaited).
struct Served {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

fn serve_managed(
    egress: &ManagedDatabaseEgress,
    worker: &WorkerRef,
    general: Option<EgressSettings>,
    limits: EgressSettings,
) -> Served {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let task = tokio::spawn(egress.listener_sink().serve(
        listener,
        EgressListenerContext {
            worker: worker.name.clone(),
            version: worker.version.clone(),
            namespace: worker.namespace.clone(),
            process_id: format!("{}-1", worker.name),
            settings: general,
            limits,
        },
    ));
    Served {
        path,
        task,
        _dir: dir,
    }
}

impl Served {
    async fn call(
        &self,
        method: &str,
        target: &str,
        body: &[u8],
        extra: &[(&str, &str)],
    ) -> RawResponse {
        let mut stream = UnixStream::connect(&self.path).await.unwrap();
        send_raw(&mut stream, method, target, body, extra).await;
        read_raw(&mut stream).await
    }

    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the operator-approved real sqld 0.24.32 (EDGER_SQLD_BIN)"]
async fn real_managed_socket_none_some_coexist_and_cap_one_is_shared() {
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    let (user_base, admin_base) = (sqld.user_base.clone(), sqld.admin_base.clone());
    // The test (not the scenario) owns the proxy: whatever the scenario does
    // (pass, panic, time out) the proxy listener AND its connection tasks are
    // cancelled and awaited before the failure propagates, then the owned sqld
    // child is killed and reaped.
    let proxy = Arc::new(SqldProxy::start(&user_base, 200, PROXY_HEAD_DEADLINE).await);
    let (outcome, (), proxy_down) = run_with_owned_proxy(
        "none/some scenario",
        SCENARIO,
        &proxy,
        none_some_scenario(user_base, admin_base, Arc::clone(&proxy)),
        |_| (),
    )
    .await;
    let stop = sqld.shutdown().await;
    eprintln!("managed-evidence: none/some shutdown {stop}");
    eprintln!("managed-evidence: none/some proxy shutdown {proxy_down:?}");
    assert_proxy_clean(&proxy_down);
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
}

async fn none_some_scenario(user_base: String, admin_base: String, proxy: Arc<SqldProxy>) {
    let svc = build_svc(&admin_base, &user_base);
    let store = OperationalStore::default();
    let guard = tls_guard(&store);
    let a = worker_ref("ns-a", "1.0.0", Some(RBAC));
    let intent = worker_ref("ns-intent", "1.0.0", Some(RBAC));
    // c/d: fresh versions for the cap-1 section (the (worker, version) slot
    // map is created with the first serve's maxConcurrent).
    let c = worker_ref("ns-c", "1.0.0", Some(RBAC));
    let d = worker_ref("ns-d", "1.0.0", Some(RBAC));
    let e = worker_ref("ns-e", "1.0.0", Some(RBAC));
    for w in [&a, &c, &d, &e] {
        ensure_ready(&svc, w).await;
    }
    svc.registry
        .ensure_intent(&reg_owner(&intent), QUOTA)
        .unwrap();
    let up = tls_upstream().await;
    let https_ok = format!("https://127.0.0.1:{}/ok", up.addr.port());
    let https_sse = format!("https://127.0.0.1:{}/sse", up.addr.port());
    // The declared general egress: the fixture's ephemeral port is the only one allowed.
    let mut general = limits();
    general.ports = vec![up.addr.port()];
    let mk = |w: &WorkerRef| {
        ManagedDatabaseEgress::new(Arc::clone(&svc.service), Arc::clone(&guard), w, &user_base)
            .unwrap()
    };
    let (ea, ec, ed, ei) = (mk(&a), mk(&c), mk(&d), mk(&intent));
    let pipeline = |statement: &str| {
        json!({"requests": [{"type": "execute", "stmt": {"sql": statement}}, {"type": "close"}]})
            .to_string()
    };
    let db_url = format!("{user_base}/v2/pipeline");

    // ── settings None: the database works, any other destination is refused
    //    statically WITHOUT a connection (limits are defaults, not authority).
    let none = serve_managed(&ea, &a, None, limits());
    let r = none
        .call(
            "POST",
            &db_url,
            pipeline("SELECT 41 + 1").as_bytes(),
            &[("content-type", "application/json")],
        )
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(first_cell(&r.body).as_deref(), Some("42"));
    let r = none.call("GET", &https_ok, b"", &[]).await;
    assert_eq!(r.error_code().as_deref(), Some("EGRESS_BAD_TARGET"));
    assert_eq!(
        up.connections.load(Ordering::SeqCst),
        0,
        "None never opens the general path"
    );
    none.stop().await;

    // ── settings Some: general keeps its policy (HTTPS to the authorized
    //    local fixture) and the DB still goes ONLY to the authority.
    let some = serve_managed(&ea, &a, Some(general.clone()), general.clone());
    let r = some.call("GET", &https_ok, b"", &[]).await;
    assert_eq!(
        (r.status, r.body.as_slice()),
        (200, b"hello-egress".as_slice())
    );
    let r = some
        .call("POST", &db_url, pipeline("SELECT 7").as_bytes(), &[])
        .await;
    assert_eq!(first_cell(&r.body).as_deref(), Some("7"));
    // A plain-http non-DB destination stays refused by the general policy.
    let r = some.call("GET", "http://127.0.0.1:9/", b"", &[]).await;
    assert_eq!(r.error_code().as_deref(), Some("EGRESS_BAD_TARGET"));
    some.stop().await;

    // ── DB with an unavailable credential NEVER falls back to general, even
    //    with Some: the Intent binding is denied, the fixture sees nothing new.
    let before = up.connections.load(Ordering::SeqCst);
    let denied = serve_managed(&ei, &intent, Some(general.clone()), general.clone());
    let r = denied
        .call("POST", &db_url, pipeline("SELECT 1").as_bytes(), &[])
        .await;
    assert_eq!(r.error_code().as_deref(), Some("EGRESS_DATABASE_DENIED"));
    assert_ne!(r.status, 200);
    // userinfo/fragment on the DB origin is routed to the DB and refused THERE.
    let with_user = user_base.replace("http://", "http://u:p@");
    let some_i = serve_managed(&ea, &a, Some(general.clone()), general.clone());
    let r = some_i
        .call("POST", &format!("{with_user}/v2/pipeline"), b"{}", &[])
        .await;
    assert_eq!(r.error_code().as_deref(), Some("EGRESS_DATABASE_DENIED"));
    some_i.stop().await;
    denied.stop().await;
    assert_eq!(
        up.connections.load(Ordering::SeqCst),
        before,
        "no fallback to general"
    );

    // ── cap 1 is SHARED by DB and general; app B is not blocked by A.
    let mut cap = general.clone();
    cap.max_concurrent = 1;
    let sa = serve_managed(&ec, &c, Some(cap.clone()), cap.clone());
    let sb = serve_managed(&ed, &d, Some(cap.clone()), cap.clone());
    // A holds its slot with a streaming general call (first chunk read).
    let mut held = UnixStream::connect(&sa.path).await.unwrap();
    send_raw(&mut held, "GET", &https_sse, b"", &[]).await;
    let mut seen = Vec::new();
    let mut chunk = [0u8; 4096];
    while !seen.windows(3).any(|w| w == b"one") {
        let n = tokio::time::timeout(Duration::from_secs(10), held.read(&mut chunk))
            .await
            .expect("first chunk streams before the release")
            .unwrap();
        assert!(n > 0);
        seen.extend_from_slice(&chunk[..n]);
    }
    let r = sa
        .call("POST", &db_url, pipeline("SELECT 1").as_bytes(), &[])
        .await;
    assert_eq!(
        r.error_code().as_deref(),
        Some("EGRESS_LIMIT"),
        "DB is limited by A's general call"
    );
    let r = sb
        .call("POST", &db_url, pipeline("SELECT 5").as_bytes(), &[])
        .await;
    assert_eq!(
        first_cell(&r.body).as_deref(),
        Some("5"),
        "B is not blocked by A"
    );
    up.release.notify_waiters();
    let mut rest = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), held.read_to_end(&mut rest)).await;
    drop(held);
    // The slot is released when the streaming call ends.
    let mut ok = false;
    for _ in 0..50 {
        let r = sa
            .call("POST", &db_url, pipeline("SELECT 9").as_bytes(), &[])
            .await;
        if r.status == 200 {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ok, "the slot was released after the streaming call");

    // Reverse direction, REAL SQLD: a DB call holds the shared slot, a
    // general call is limited, and the slot is freed when the DB call ends
    // (phase 1) or is cancelled (phase 2).
    //
    // The DB call goes through the guard to a TRANSPARENT proxy in front of
    // the REAL SQLD (`SqldProxy`): it forwards the request bytes (service
    // credentials + namespace + SQL) upstream and the real answer comes back
    // from SQLD. After SQLD answered, the proxy HOLDS that answer behind a
    // barrier: the barrier holds the response/consumption, not SQL CPU. The
    // proxy signals `answered` only after the guard dialed it and SQLD
    // replied, i.e. after the DB call was admitted; the general call starts
    // only after that signal (no polling that could win the slot first).
    let ee = ManagedDatabaseEgress::new(
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        &e,
        &proxy.base(),
    )
    .unwrap();
    let se = serve_managed(&ee, &e, Some(cap.clone()), cap.clone());
    let proxied_db = format!("{}/v2/pipeline", proxy.base());
    let events_before = worker_events(&store, "ns-e").len();
    assert_eq!(events_before, 0);

    // Phase 1: hold, limit, release, the DB call completes with SQLD's answer.
    let mut long = UnixStream::connect(&se.path).await.unwrap();
    send_raw(
        &mut long,
        "POST",
        &proxied_db,
        pipeline("SELECT 41 + 1").as_bytes(),
        &[("content-type", "application/json")],
    )
    .await;
    tokio::time::timeout(READ_WAIT, proxy.answered.notified())
        .await
        .expect("SQLD answered the proxied DB call (admitted, response held)");
    assert_eq!(proxy.connections.load(Ordering::SeqCst), 1);
    {
        let records = proxy.records.lock().unwrap();
        assert_eq!(records.len(), 1, "one request reached the real SQLD");
        let record = &records[0];
        assert_eq!(record.path, "/v2/pipeline");
        assert!(record.body.contains("SELECT 41 + 1"), "the query arrived");
        assert!(
            record.authorization.starts_with("Bearer ") && !record.namespace.is_empty(),
            "service credentials and namespace were forwarded"
        );
        assert_eq!(record.upstream_status, Some(200), "SQLD itself answered");
    }
    assert!(!proxy.released.load(Ordering::SeqCst));
    let before_limit = up.connections.load(Ordering::SeqCst);
    let r = se.call("GET", &https_ok, b"", &[]).await;
    assert_eq!(
        r.error_code().as_deref(),
        Some("EGRESS_LIMIT"),
        "a general call is limited while the DB call holds the slot"
    );
    assert_eq!(
        up.connections.load(Ordering::SeqCst),
        before_limit,
        "the limited call dialed nothing"
    );
    proxy.release.notify_one();
    let r = tokio::time::timeout(READ_WAIT, read_raw(&mut long))
        .await
        .expect("the DB call completes after the release");
    assert_eq!(r.status, 200);
    assert_eq!(
        first_cell(&r.body).as_deref(),
        Some("42"),
        "the result came from the real SQLD through the proxy"
    );
    // The DB call's event is its end; the slot is free then (one attempt,
    // no retry loop that could hide a late release).
    let events = wait_events(&store, "ns-e", 2).await;
    assert_eq!(events[1].outcome.as_deref(), Some("ok"));
    let r = se.call("GET", &https_ok, b"", &[]).await;
    assert_eq!(r.status, 200, "completing the DB call released the slot");
    assert_eq!(r.body.as_slice(), b"hello-egress");

    // Phase 2: hold again, limit, then CANCEL the DB call.
    let mut cancelled = UnixStream::connect(&se.path).await.unwrap();
    send_raw(
        &mut cancelled,
        "POST",
        &proxied_db,
        pipeline("SELECT 6 * 7").as_bytes(),
        &[("content-type", "application/json")],
    )
    .await;
    tokio::time::timeout(READ_WAIT, proxy.answered.notified())
        .await
        .expect("SQLD answered the second proxied DB call");
    assert_eq!(proxy.records.lock().unwrap().len(), 2);
    assert_eq!(proxy.records.lock().unwrap()[1].upstream_status, Some(200));
    let before_limit = up.connections.load(Ordering::SeqCst);
    let r = se.call("GET", &https_ok, b"", &[]).await;
    assert_eq!(r.error_code().as_deref(), Some("EGRESS_LIMIT"));
    assert_eq!(up.connections.load(Ordering::SeqCst), before_limit);
    drop(cancelled); // cancel while SQLD's answer is still held
    let events = wait_events(&store, "ns-e", 5).await;
    assert_eq!(
        events[4].outcome.as_deref(),
        Some("error"),
        "cancelled DB call"
    );
    let r = se.call("GET", &https_ok, b"", &[]).await;
    assert_eq!(r.status, 200, "cancelling the DB call released the slot");
    assert_eq!(r.body.as_slice(), b"hello-egress");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while proxy.client_closed.load(Ordering::SeqCst) < 1 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        proxy.client_closed.load(Ordering::SeqCst),
        1,
        "the cancel reached the upstream leg (the guard closed it)"
    );
    assert_eq!(
        proxy.connections.load(Ordering::SeqCst),
        2,
        "no hidden retry"
    );
    let events = wait_events(&store, "ns-e", 6).await;
    let shape: Vec<_> = events
        .iter()
        .map(|e| (e.outcome.clone().unwrap_or_default(), e.code.clone()))
        .collect();
    eprintln!("managed-evidence: ns-e events {shape:?}");
    let limit = ("limit".to_owned(), Some("EGRESS_LIMIT".to_owned()));
    let ok = ("ok".to_owned(), None);
    assert_eq!(
        shape,
        vec![
            limit.clone(),
            ok.clone(), // DB 1 completed
            ok.clone(), // general after completion
            limit,
            ("error".to_owned(), None), // DB 2 cancelled
            ok,                         // general after cancel
        ]
    );
    assert_eq!(proxy.head_failures.load(Ordering::SeqCst), 0);
    se.stop().await;

    sa.stop().await;
    sb.stop().await;
    up.task.abort();
    let _ = up.task.await;
    let text = serde_json::to_string(&egress_events(&store)).unwrap();
    assert!(!text.contains("eyJ") && !text.contains("Bearer"));
}

// ═════════════ real Deno guest: DB body streaming, limits, coexistence ═════════════
//
// Levels: the guest is REAL Deno; the service (`ensure`, vault, ledger,
// token) is REAL against a REAL SQLD; the guard is the REAL shared
// `EgressGuard`. For the streaming and limit tests the DATABASE ORIGIN is a
// controlled fake HTTP server (`ModalOrigin`) standing in for the SQLD data
// listener: the body bytes there do NOT come from a real SQLD (only the
// credentials/namespace do). The coexistence test queries the real SQLD.

const READ_WAIT: Duration = Duration::from_secs(20);

/// One fake data origin with a path per behavior; `/stream` holds its second
/// chunk behind a barrier (`release`), bounded by `READ_WAIT`.
struct ModalOrigin {
    port: u16,
    connections: Arc<AtomicUsize>,
    release: Arc<Notify>,
    second_sent: Arc<AtomicBool>,
    barrier_timed_out: Arc<AtomicBool>,
    hang_closed: Arc<AtomicUsize>,
    heads: Arc<Mutex<Vec<(String, Headers)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl ModalOrigin {
    async fn start(redirect_to: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());
        let second_sent = Arc::new(AtomicBool::new(false));
        let barrier_timed_out = Arc::new(AtomicBool::new(false));
        let hang_closed = Arc::new(AtomicUsize::new(0));
        let heads: Arc<Mutex<Vec<(String, Headers)>>> = Arc::default();
        let (c, r, s2, bt, hc, h) = (
            Arc::clone(&connections),
            Arc::clone(&release),
            Arc::clone(&second_sent),
            Arc::clone(&barrier_timed_out),
            Arc::clone(&hang_closed),
            Arc::clone(&heads),
        );
        let task = tokio::spawn(async move {
            let mut conns = tokio::task::JoinSet::new();
            loop {
                let Ok((mut tcp, _)) = listener.accept().await else {
                    break;
                };
                c.fetch_add(1, Ordering::SeqCst);
                let (r, s2, bt, hc, h, redirect_to) = (
                    Arc::clone(&r),
                    Arc::clone(&s2),
                    Arc::clone(&bt),
                    Arc::clone(&hc),
                    Arc::clone(&h),
                    redirect_to.clone(),
                );
                conns.spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tcp.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let mut scratch = [httparse::EMPTY_HEADER; 64];
                    let mut req = httparse::Request::new(&mut scratch);
                    let _ = req.parse(&buf);
                    let path = req.path.unwrap_or("").to_owned();
                    h.lock().unwrap().push((
                        path.clone(),
                        req.headers
                            .iter()
                            .map(|x| {
                                (
                                    x.name.to_ascii_lowercase(),
                                    String::from_utf8_lossy(x.value).into_owned(),
                                )
                            })
                            .collect(),
                    ));
                    let block = [b'x'; 4096];
                    match path.as_str() {
                        "/redirect" => {
                            let head = format!("HTTP/1.1 302 Found\r\nlocation: {redirect_to}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                            let _ = tcp.write_all(head.as_bytes()).await;
                        }
                        "/big" => {
                            // No Content-Length: the cap is a mid-stream cut.
                            let _ = tcp
                                .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n")
                                .await;
                            for _ in 0..64 {
                                if tcp.write_all(&block).await.is_err() {
                                    return;
                                }
                            }
                        }
                        "/big-cl" => {
                            let _ = tcp
                                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 262144\r\nconnection: close\r\n\r\n")
                                .await;
                            for _ in 0..64 {
                                if tcp.write_all(&block).await.is_err() {
                                    return;
                                }
                            }
                        }
                        "/hang" => {
                            // No answer; ends when the guard closes the peer.
                            let closed = tokio::time::timeout(READ_WAIT, async {
                                loop {
                                    match tcp.read(&mut chunk).await {
                                        Ok(0) | Err(_) => break,
                                        Ok(_) => {}
                                    }
                                }
                            })
                            .await
                            .is_ok();
                            if closed {
                                hc.fetch_add(1, Ordering::SeqCst);
                            }
                            return;
                        }
                        "/stream" => {
                            let _ = tcp
                                .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nx-origin-marker: streamed\r\nconnection: close\r\n\r\n")
                                .await;
                            let _ = tcp.write_all(b"c\r\nfirst-chunk-\r\n").await;
                            let _ = tcp.flush().await;
                            // The barrier: the rest only after `release`
                            // (a stored permit also counts), bounded.
                            if tokio::time::timeout(READ_WAIT, r.notified()).await.is_err() {
                                bt.store(true, Ordering::SeqCst);
                                return;
                            }
                            s2.store(true, Ordering::SeqCst);
                            let _ = tcp.write_all(b"c\r\nsecond-chunk\r\n0\r\n\r\n").await;
                        }
                        _ => {
                            let _ = tcp
                                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                                .await;
                        }
                    }
                    let _ = tcp.shutdown().await;
                });
            }
            conns.abort_all();
            while conns.join_next().await.is_some() {}
        });
        Self {
            port,
            connections,
            release,
            second_sent,
            barrier_timed_out,
            hang_closed,
            heads,
            task,
        }
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// Largest upstream response head the proxy accepts before failing.
const PROXY_HEAD_CAP: usize = 16 * 1024;
/// Deadline for the COMPLETE upstream response head (status line + headers).
const PROXY_HEAD_DEADLINE: Duration = Duration::from_secs(10);

/// Why the proxy refused to treat an upstream answer as admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HeadFailure {
    /// The upstream closed before a complete head arrived.
    Eof,
    /// A read/write/connect error.
    Io,
    /// The head grew past `PROXY_HEAD_CAP` without completing.
    TooLarge,
    /// No complete head within the deadline.
    Deadline,
    /// Not an HTTP/1.x response head.
    Malformed,
    /// A complete head, but not the status the fixture expects.
    Status(u16),
}

/// Read the upstream response until `httparse` reports a COMPLETE head,
/// bounded by `cap` bytes and `deadline`. Returns the status and EVERY byte
/// received so far (head plus any body prefix) so the caller forwards them
/// unchanged. EOF, errors, an oversized or malformed head are failures.
async fn read_response_head(
    sqld: &mut tokio::net::TcpStream,
    cap: usize,
    deadline: Duration,
) -> Result<(u16, Vec<u8>), HeadFailure> {
    let limit = tokio::time::Instant::now() + deadline;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = match tokio::time::timeout_at(limit, sqld.read(&mut chunk)).await {
            Err(_) => return Err(HeadFailure::Deadline),
            Ok(Err(_)) => return Err(HeadFailure::Io),
            Ok(Ok(0)) => return Err(HeadFailure::Eof),
            Ok(Ok(n)) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        let mut scratch = [httparse::EMPTY_HEADER; 64];
        let mut res = httparse::Response::new(&mut scratch);
        match res.parse(&buf) {
            Err(_) => return Err(HeadFailure::Malformed),
            Ok(httparse::Status::Complete(head_len)) => {
                if head_len > cap {
                    return Err(HeadFailure::TooLarge);
                }
                return res.code.map(|c| (c, buf)).ok_or(HeadFailure::Malformed);
            }
            Ok(httparse::Status::Partial) => {
                if buf.len() > cap {
                    return Err(HeadFailure::TooLarge);
                }
            }
        }
    }
}

/// One request seen by the proxy and what the real SQLD answered.
struct ProxyRecord {
    path: String,
    authorization: String,
    namespace: String,
    body: String,
    upstream_status: Option<u16>,
    head_failure: Option<HeadFailure>,
}

/// Decrements the live-connection counter when the task ends OR is aborted
/// (the future's captures are dropped either way).
struct LiveGuard(Arc<AtomicUsize>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What the listener task drained when it was told to stop.
#[derive(Debug, Default)]
struct ProxyDrain {
    cancelled: usize,
    completed: usize,
    panicked: usize,
}

/// The result of `SqldProxy::shutdown`: awaited, not assumed.
#[derive(Debug)]
struct ProxyShutdown {
    /// `None` when `shutdown` had already run.
    drain: Option<ProxyDrain>,
    /// Own connection tasks still alive after the listener task was awaited.
    live_after: usize,
    /// A fresh connect to the proxy port was refused.
    listener_closed: bool,
}

/// A TRANSPARENT TCP proxy in front of the REAL SQLD (control fixture, not a
/// fake SQLD): the request bytes go upstream untouched and the answer is
/// SQLD's own. After a COMPLETE upstream response head with the expected
/// status arrives it signals `answered` and HOLDS every received byte behind
/// `release` (<= `READ_WAIT`), so the guard's slot ownership interval is
/// fixed by the held RESPONSE (not by SQL CPU). While held, a client-side
/// close is counted in `client_closed`.
///
/// Lifecycle: the listener task owns the connection `JoinSet`; `shutdown`
/// tells it to stop, and it cancels and JOINS every connection task before it
/// returns, then `shutdown` awaits the listener task itself. The owner (not
/// the scenario) calls `shutdown`, so a scenario that panics or times out
/// does not leave the proxy running.
struct SqldProxy {
    port: u16,
    connections: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
    answered: Arc<Notify>,
    release: Arc<Notify>,
    released: Arc<AtomicBool>,
    client_closed: Arc<AtomicUsize>,
    head_failures: Arc<AtomicUsize>,
    records: Arc<Mutex<Vec<ProxyRecord>>>,
    stop: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    task: Mutex<Option<tokio::task::JoinHandle<ProxyDrain>>>,
}

impl SqldProxy {
    async fn start(sqld_base: &str, expected_status: u16, head_deadline: Duration) -> Self {
        let upstream = sqld_base.trim_start_matches("http://").to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(AtomicUsize::new(0));
        let answered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let released = Arc::new(AtomicBool::new(false));
        let client_closed = Arc::new(AtomicUsize::new(0));
        let head_failures = Arc::new(AtomicUsize::new(0));
        let records: Arc<Mutex<Vec<ProxyRecord>>> = Arc::default();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (c, l, a, r, rl, cc, hf, rec) = (
            Arc::clone(&connections),
            Arc::clone(&live),
            Arc::clone(&answered),
            Arc::clone(&release),
            Arc::clone(&released),
            Arc::clone(&client_closed),
            Arc::clone(&head_failures),
            Arc::clone(&records),
        );
        let task = tokio::spawn(async move {
            let mut conns = tokio::task::JoinSet::new();
            let mut drain = ProxyDrain::default();
            loop {
                let mut client = tokio::select! {
                    // Also fires when the proxy value is dropped unstopped.
                    _ = &mut stop_rx => break,
                    Some(done) = conns.join_next(), if !conns.is_empty() => {
                        drain.completed += 1;
                        if matches!(done, Err(e) if e.is_panic()) {
                            drain.panicked += 1;
                        }
                        continue;
                    }
                    accepted = listener.accept() => match accepted {
                        Ok((client, _)) => client,
                        Err(_) => break,
                    },
                };
                c.fetch_add(1, Ordering::SeqCst);
                l.fetch_add(1, Ordering::SeqCst);
                let live_guard = LiveGuard(Arc::clone(&l));
                let (a, r, rl, cc, hf, rec, upstream) = (
                    Arc::clone(&a),
                    Arc::clone(&r),
                    Arc::clone(&rl),
                    Arc::clone(&cc),
                    Arc::clone(&hf),
                    Arc::clone(&rec),
                    upstream.clone(),
                );
                conns.spawn(async move {
                    let _live = live_guard;
                    // Read the whole request (head + content-length body).
                    let mut req = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let (head_end, content_length) = loop {
                        match client.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&chunk[..n]),
                        }
                        if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                            let mut scratch = [httparse::EMPTY_HEADER; 64];
                            let mut parsed = httparse::Request::new(&mut scratch);
                            let _ = parsed.parse(&req);
                            let len = parsed
                                .headers
                                .iter()
                                .find(|h| h.name.eq_ignore_ascii_case("content-length"))
                                .and_then(|h| std::str::from_utf8(h.value).ok()?.parse().ok())
                                .unwrap_or(0usize);
                            break (pos + 4, len);
                        }
                    };
                    while req.len() < head_end + content_length {
                        match client.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let mut scratch = [httparse::EMPTY_HEADER; 64];
                    let mut parsed = httparse::Request::new(&mut scratch);
                    let _ = parsed.parse(&req);
                    let header = |name: &str| -> String {
                        parsed
                            .headers
                            .iter()
                            .find(|h| h.name.eq_ignore_ascii_case(name))
                            .map(|h| String::from_utf8_lossy(h.value).into_owned())
                            .unwrap_or_default()
                    };
                    let index = {
                        let mut guard = rec.lock().unwrap();
                        guard.push(ProxyRecord {
                            path: parsed.path.unwrap_or("").to_owned(),
                            authorization: header("authorization"),
                            namespace: header("x-namespace-bin"),
                            body: String::from_utf8_lossy(&req[head_end..]).into_owned(),
                            upstream_status: None,
                            head_failure: None,
                        });
                        guard.len() - 1
                    };
                    let fail = |failure: HeadFailure| {
                        rec.lock().unwrap()[index].head_failure = Some(failure);
                        hf.fetch_add(1, Ordering::SeqCst);
                    };
                    // Forward untouched to the REAL SQLD.
                    let Ok(mut sqld) = tokio::net::TcpStream::connect(&upstream).await else {
                        return fail(HeadFailure::Io);
                    };
                    if sqld.write_all(&req).await.is_err() {
                        return fail(HeadFailure::Io);
                    }
                    // The COMPLETE head (bounded), every received byte kept.
                    let (status, first) =
                        match read_response_head(&mut sqld, PROXY_HEAD_CAP, head_deadline).await {
                            Ok(read) => read,
                            Err(failure) => return fail(failure),
                        };
                    rec.lock().unwrap()[index].upstream_status = Some(status);
                    if status != expected_status {
                        return fail(HeadFailure::Status(status));
                    }
                    // SQLD answered: tell the test, then hold the response.
                    a.notify_one();
                    let mut probe = [0u8; 1];
                    tokio::select! {
                        _ = r.notified() => { rl.store(true, Ordering::SeqCst); }
                        _ = tokio::time::sleep(READ_WAIT) => { return; }
                        read = client.read(&mut probe) => {
                            if matches!(read, Ok(0) | Err(_)) {
                                cc.fetch_add(1, Ordering::SeqCst);
                            }
                            return;
                        }
                    }
                    if client.write_all(&first).await.is_err() {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut sqld).await;
                });
            }
            // Cancel and JOIN every connection task before the listener task
            // ends (a bare drop of the set would abort without awaiting).
            conns.abort_all();
            while let Some(done) = conns.join_next().await {
                match done {
                    Err(e) if e.is_cancelled() => drain.cancelled += 1,
                    Err(_) => {
                        drain.completed += 1;
                        drain.panicked += 1;
                    }
                    Ok(()) => drain.completed += 1,
                }
            }
            drop(listener);
            drain
        });
        Self {
            port,
            connections,
            live,
            answered,
            release,
            released,
            client_closed,
            head_failures,
            records,
            stop: Mutex::new(Some(stop_tx)),
            task: Mutex::new(Some(task)),
        }
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Stop the listener, cancel and join the connection tasks, AWAIT the
    /// listener task, then report what is still alive. Idempotent.
    async fn shutdown(&self) -> ProxyShutdown {
        if let Some(tx) = self.stop.lock().unwrap().take() {
            let _ = tx.send(());
        }
        let task = self.task.lock().unwrap().take();
        let drain = match task {
            None => None,
            Some(mut task) => {
                match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                    Ok(Ok(drain)) => Some(drain),
                    Ok(Err(e)) => panic!("the proxy listener task failed: {e}"),
                    Err(_) => {
                        task.abort();
                        let _ = task.await;
                        panic!("the proxy listener task did not drain within 10 s");
                    }
                }
            }
        };
        let listener_closed = matches!(
            tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect(("127.0.0.1", self.port)),
            )
            .await,
            Ok(Err(_))
        );
        ProxyShutdown {
            drain,
            live_after: self.live.load(Ordering::SeqCst),
            listener_closed,
        }
    }
}

/// The OWNER of a proxy runs a scenario in a task under a time limit and
/// ALWAYS shuts the proxy down afterwards (panic, timeout or success): the
/// scenario only borrows the proxy through the `Arc`, so dropping its future
/// cannot detach the listener or the connections. `between` runs after the
/// scenario ended and BEFORE the shutdown (it must not panic: it observes
/// what a failed scenario left behind). Returns the scenario's join result,
/// what `between` observed and the awaited shutdown.
async fn run_with_owned_proxy<Fut, T>(
    what: &'static str,
    limit: Duration,
    proxy: &Arc<SqldProxy>,
    scenario: Fut,
    between: impl FnOnce(&SqldProxy) -> T,
) -> (Result<(), tokio::task::JoinError>, T, ProxyShutdown)
where
    Fut: Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(async move {
        tokio::time::timeout(limit, scenario)
            .await
            .unwrap_or_else(|_| panic!("{what} exceeded {limit:?}"))
    })
    .await;
    let observed = between(proxy);
    let shutdown = proxy.shutdown().await;
    (outcome, observed, shutdown)
}

/// The proxy fixture left nothing of its own behind.
fn assert_proxy_clean(down: &ProxyShutdown) {
    let drain = down.drain.as_ref().expect("the listener task was awaited");
    assert_eq!(drain.panicked, 0, "a proxy connection task panicked");
    assert_eq!(down.live_after, 0, "own proxy connections still alive");
    assert!(down.listener_closed, "the proxy listener still accepts");
}

/// A CONTROLLED upstream used ONLY to prove the proxy's response framing:
/// after the request head it writes the scripted fragments (TCP_NODELAY and a
/// pause between them, so each lands as its own read), then closes or stalls.
struct ScriptedUpstream {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedUpstream {
    async fn start(fragments: Vec<Vec<u8>>, stall_after: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            let _ = tcp.set_nodelay(true);
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match tcp.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            for fragment in fragments {
                if tcp.write_all(&fragment).await.is_err() || tcp.flush().await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
            if stall_after {
                tokio::time::sleep(READ_WAIT).await;
            }
        });
        Self { port, task }
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// What one scripted exchange through the proxy ended as.
struct Framing {
    /// The proxy signalled admission.
    admitted: bool,
    /// Every byte the client got back from the proxy.
    forwarded: Vec<u8>,
    status: Option<u16>,
    failure: Option<HeadFailure>,
    head_failures: usize,
    elapsed: Duration,
    down: ProxyShutdown,
}

/// Drive one scripted upstream answer through a proxy. The proxy and the
/// upstream are ALWAYS shut down (also when an assertion/timeout inside
/// panics) before the panic propagates.
async fn run_framing(
    fragments: &[&str],
    stall_after: bool,
    expected_status: u16,
    head_deadline: Duration,
) -> Framing {
    let upstream = ScriptedUpstream::start(
        fragments.iter().map(|s| s.as_bytes().to_vec()).collect(),
        stall_after,
    )
    .await;
    let proxy = Arc::new(SqldProxy::start(&upstream.base(), expected_status, head_deadline).await);
    let started = tokio::time::Instant::now();
    let mut outcome = None;
    let result = std::panic::AssertUnwindSafe(async {
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy.port))
            .await
            .unwrap();
        client
            .write_all(b"POST /v2/pipeline HTTP/1.1\r\nhost: x\r\ncontent-length: 2\r\n\r\n{}")
            .await
            .unwrap();
        // Admission or a head failure, whichever the proxy reaches first.
        let settle = tokio::time::Instant::now() + Duration::from_secs(5);
        let admitted = loop {
            if tokio::time::timeout(Duration::from_millis(20), proxy.answered.notified())
                .await
                .is_ok()
            {
                break true;
            }
            if proxy.head_failures.load(Ordering::SeqCst) > 0 {
                break false;
            }
            assert!(
                tokio::time::Instant::now() < settle,
                "neither admitted nor failed"
            );
        };
        let elapsed = started.elapsed();
        if admitted {
            proxy.release.notify_one();
        }
        let mut forwarded = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut forwarded))
            .await
            .expect("the client side ends")
            .ok();
        let (status, failure) = {
            let records = proxy.records.lock().unwrap();
            (records[0].upstream_status, records[0].head_failure.clone())
        };
        outcome = Some((admitted, forwarded, status, failure, elapsed));
    })
    .await_unwind()
    .await;
    let down = proxy.shutdown().await;
    let head_failures = proxy.head_failures.load(Ordering::SeqCst);
    upstream.stop().await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    let (admitted, forwarded, status, failure, elapsed) = outcome.expect("the exchange ran");
    Framing {
        admitted,
        forwarded,
        status,
        failure,
        head_failures,
        elapsed,
        down,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_reassembles_a_response_head_split_across_reads() {
    // Each case is a scripted upstream answer in fragments that land as
    // separate reads. The status line AND the headers are split (first piece
    // "HT"), and a body prefix may ride with the end of the head.
    let cases: [(&str, &[&str]); 3] = [
        (
            "status line and headers split, body prefix in the last head fragment",
            &[
                "HT",
                "TP/1.1 2",
                "00 OK\r\nContent-Le",
                "ngth: 11\r\nX-A: b\r\n",
                "\r\nhello",
                " world",
            ],
        ),
        (
            "one byte per read",
            &[
                "H", "T", "T", "P", "/", "1", ".", "1", " ", "2", "0", "0", " ", "O", "K", "\r",
                "\n", "\r", "\n", "o", "k",
            ],
        ),
        (
            "control: the whole head and body in one read",
            &["HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"],
        ),
    ];
    for (name, fragments) in cases {
        let got = run_framing(fragments, false, 200, PROXY_HEAD_DEADLINE).await;
        assert!(got.admitted, "{name}: admitted after the complete head");
        assert_eq!(
            got.status,
            Some(200),
            "{name}: status from the COMPLETE head"
        );
        assert_eq!(got.failure, None, "{name}");
        assert_eq!(got.head_failures, 0, "{name}");
        assert_eq!(
            got.forwarded,
            fragments.concat().into_bytes(),
            "{name}: every upstream byte is forwarded unchanged"
        );
        assert_proxy_clean(&got.down);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_never_admits_an_incomplete_malformed_or_unexpected_response_head() {
    let pad = "a".repeat(5000);
    let pad_fragments = [
        "HTTP/1.1 200 OK\r\nX-Pad: ",
        pad.as_str(),
        pad.as_str(),
        pad.as_str(),
        pad.as_str(),
    ];
    // (name, fragments, expected failure). Upstream closes after the script.
    let cases: [(&str, &[&str], HeadFailure); 6] = [
        (
            "EOF inside the status line",
            &["HT", "TP/1.1 20"],
            HeadFailure::Eof,
        ),
        (
            "EOF inside the headers",
            &["HTTP/1.1 200 OK\r\n", "Content-Le"],
            HeadFailure::Eof,
        ),
        ("EOF before any byte", &[], HeadFailure::Eof),
        (
            "head above the byte cap without terminating",
            &pad_fragments,
            HeadFailure::TooLarge,
        ),
        (
            "not an HTTP head",
            &["XXXX 200 OK\r\n\r\n"],
            HeadFailure::Malformed,
        ),
        (
            "a complete head with another status",
            &["HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n"],
            HeadFailure::Status(500),
        ),
    ];
    for (name, fragments, expected) in cases {
        let got = run_framing(fragments, false, 200, PROXY_HEAD_DEADLINE).await;
        assert!(!got.admitted, "{name}: must not signal admission");
        assert_eq!(got.failure.as_ref(), Some(&expected), "{name}");
        assert_eq!(got.head_failures, 1, "{name}");
        assert!(
            got.forwarded.is_empty(),
            "{name}: nothing was forwarded to the client"
        );
        assert_proxy_clean(&got.down);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_head_deadline_fails_without_hanging() {
    // The upstream sends a partial status line and then stalls (it would hold
    // for READ_WAIT); the proxy must give up at its own, shorter deadline.
    let deadline = Duration::from_millis(500);
    let got = run_framing(&["HTTP/1.1 2"], true, 200, deadline).await;
    assert!(!got.admitted);
    assert_eq!(got.failure, Some(HeadFailure::Deadline));
    assert_eq!(got.head_failures, 1);
    assert!(got.forwarded.is_empty());
    assert!(
        got.elapsed >= deadline && got.elapsed < Duration::from_secs(5),
        "deadline respected, no hang: {:?}",
        got.elapsed
    );
    assert_proxy_clean(&got.down);
}

#[derive(Clone, Copy, Debug)]
enum Forced {
    Panic,
    Timeout,
}

/// What a forced scenario failure left behind, observed before and after the
/// owner's cleanup.
#[derive(Debug)]
struct ForcedReport {
    mode: Forced,
    failure_message: String,
    live_before_cleanup: usize,
    released_before_cleanup: bool,
    down: ProxyShutdown,
    port_rebound: bool,
    retained_client_closed: bool,
    client_closed_counter: usize,
}

const FORCED_PANIC: &str = "forced scenario failure";

/// Forced failure with a connection RETAINED at the barrier: the REAL SQLD
/// answers (a pipeline request whose body `{}` is not a valid pipeline is
/// refused with 400, a real answer), the proxy admits and HOLDS it, the scenario parks the client socket where
/// the scenario's own failure cannot drop it, and then panics (or times out).
async fn forced_failure(user_base: &str, mode: Forced) -> ForcedReport {
    let proxy = Arc::new(SqldProxy::start(user_base, 400, PROXY_HEAD_DEADLINE).await);
    let parked: Arc<Mutex<Option<tokio::net::TcpStream>>> = Arc::default();
    let scenario = {
        let (proxy, parked) = (Arc::clone(&proxy), Arc::clone(&parked));
        async move {
            let mut client = tokio::net::TcpStream::connect(("127.0.0.1", proxy.port))
                .await
                .unwrap();
            client
                .write_all(b"POST /v2/pipeline HTTP/1.1\r\nhost: x\r\ncontent-length: 2\r\n\r\n{}")
                .await
                .unwrap();
            if tokio::time::timeout(Duration::from_secs(10), proxy.answered.notified())
                .await
                .is_err()
            {
                let seen: Vec<_> = proxy
                    .records
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|r| (r.upstream_status, r.head_failure.clone()))
                    .collect();
                panic!("the real SQLD did not answer as expected: {seen:?}");
            }
            *parked.lock().unwrap() = Some(client);
            match mode {
                Forced::Panic => panic!("{FORCED_PANIC}"),
                Forced::Timeout => std::future::pending::<()>().await,
            }
        }
    };
    let limit = match mode {
        Forced::Panic => Duration::from_secs(15),
        Forced::Timeout => Duration::from_secs(2),
    };
    let (outcome, (live_before_cleanup, released_before_cleanup), down) =
        run_with_owned_proxy("forced scenario", limit, &proxy, scenario, |p| {
            (
                p.live.load(Ordering::SeqCst),
                p.released.load(Ordering::SeqCst),
            )
        })
        .await;
    let failure_message = match outcome {
        Ok(()) => "scenario unexpectedly succeeded".to_owned(),
        Err(e) if e.is_panic() => {
            let payload = e.into_panic();
            payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default()
        }
        Err(e) => format!("not a panic: {e}"),
    };
    // The listener is gone: its port can be bound again.
    let port_rebound = TcpListener::bind(("127.0.0.1", proxy.port)).await.is_ok();
    // The cleanup tore the held connection down (not the client): the
    // retained client now reads EOF/reset.
    let mut retained = parked.lock().unwrap().take().expect("a parked client");
    let mut byte = [0u8; 1];
    let retained_client_closed = matches!(
        tokio::time::timeout(Duration::from_secs(5), retained.read(&mut byte)).await,
        Ok(Ok(0) | Err(_))
    );
    ForcedReport {
        mode,
        failure_message,
        live_before_cleanup,
        released_before_cleanup,
        down,
        port_rebound,
        retained_client_closed,
        client_closed_counter: proxy.client_closed.load(Ordering::SeqCst),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the operator-approved real sqld 0.24.32 (EDGER_SQLD_BIN)"]
async fn proxy_cleanup_cancels_and_awaits_listener_and_held_connection_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    let user_base = sqld.user_base.clone();
    let mut reports = Vec::new();
    let result = std::panic::AssertUnwindSafe(async {
        for mode in [Forced::Panic, Forced::Timeout] {
            reports.push(forced_failure(&user_base, mode).await);
        }
    })
    .await_unwind()
    .await;
    let stop = sqld.shutdown().await;
    eprintln!("managed-evidence: forced-failure shutdown {stop}");
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    for report in &reports {
        eprintln!("managed-evidence: forced-failure {report:?}");
        let mode = report.mode;
        let expected = match mode {
            Forced::Panic => FORCED_PANIC,
            Forced::Timeout => "forced scenario exceeded",
        };
        assert!(
            report.failure_message.contains(expected),
            "{mode:?}: {}",
            report.failure_message
        );
        assert_eq!(
            report.live_before_cleanup, 1,
            "{mode:?}: the connection was still held when the scenario failed"
        );
        assert!(!report.released_before_cleanup, "{mode:?}");
        let drain = report.down.drain.as_ref().expect("listener awaited");
        assert_eq!(
            (drain.cancelled, drain.completed, drain.panicked),
            (1, 0, 0),
            "{mode:?}: the held connection task was cancelled and joined"
        );
        assert_proxy_clean(&report.down);
        assert!(report.port_rebound, "{mode:?}: the port was released");
        assert!(
            report.retained_client_closed,
            "{mode:?}: the held connection was closed by the cleanup"
        );
        assert_eq!(
            report.client_closed_counter, 0,
            "{mode:?}: the proxy did not see a client-side close"
        );
    }
}

const DENO_DB_WORKER: &str = r#"
const db = Deno.env.get("SQLD_URL");
async function probe(path) {
  const out = { status: null, egressError: null, got: 0, fetchError: null, readError: null };
  let r;
  try { r = await fetch(db + path); } catch (e) { out.fetchError = e.name; return out; }
  out.status = r.status;
  out.egressError = r.headers.get("x-edger-egress-error");
  try {
    const reader = r.body.getReader();
    for (;;) { const { done, value } = await reader.read(); if (done) break; out.got += value.length; }
  } catch (e) { out.readError = e.name; }
  return out;
}
async function failure(f) {
  try { await f(); return "no-error"; } catch (e) { return e.name; }
}
Deno.serve(async (req) => {
  const u = new URL(req.url);
  switch (u.pathname) {
    case "/relay": {
      const r = await fetch(db + u.searchParams.get("path"));
      return new Response(r.body, { status: r.status, headers: r.headers });
    }
    case "/probe":
      return Response.json(await probe(u.searchParams.get("path")));
    case "/db": {
      const r = await fetch(db + "/v2/pipeline", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ requests: [{ type: "execute", stmt: { sql: u.searchParams.get("sql") } }, { type: "close" }] }),
      });
      return Response.json({ status: r.status, err: r.headers.get("x-edger-egress-error"), text: await r.text() });
    }
    case "/general": {
      // The EXPLICIT protocol of the general guard (unchanged contract).
      const sock = Deno.env.get("EDGER_EGRESS_SOCKET");
      const host = Deno.env.get("EDGER_EGRESS_HOST");
      if (!sock || !host) return Response.json({ sock: sock ?? null, host: host ?? null });
      const client = Deno.createHttpClient({ proxy: { transport: "unix", path: sock } });
      const r = await fetch(`http://${host}/`, {
        headers: { "x-edger-egress-url": u.searchParams.get("target") },
        client,
      });
      return Response.json({ status: r.status, err: r.headers.get("x-edger-egress-error"), text: await r.text() });
    }
    case "/direct":
      return Response.json({ direct: await failure(() => fetch(u.searchParams.get("target"))) });
    case "/pid":
      return Response.json({ pid: Deno.pid });
    default:
      return new Response("not found", { status: 404 });
  }
});
"#;

fn enc(url: &str) -> String {
    url.replace(':', "%3A").replace('/', "%2F")
}

fn deno_db_fx(worker: &WorkerRef, sqld_url: &str, egress: Option<EgressSettings>) -> DenoFx {
    let mut fx = deno_fx(
        worker,
        HashMap::from([("SQLD_URL".to_owned(), sqld_url.to_owned())]),
    );
    std::fs::write(fx._dir.path().join("index.ts"), DENO_DB_WORKER).unwrap();
    fx.config.egress = egress;
    fx
}

fn arm(fx: &mut DenoFx, egress: &ManagedDatabaseEgress, w: &WorkerRef, origin: &str) {
    fx.isolate
        .set_managed_database_egress(
            egress.listener_sink(),
            w.name.clone(),
            w.version.clone(),
            w.namespace.clone(),
            origin.to_owned(),
        )
        .expect("arm the private seam");
}

/// Real Deno binary check + a real SQLD for the service, then `scenario`.
async fn with_real_sqld_and_deno<F, Fut>(label: &'static str, scenario: F)
where
    F: FnOnce(String, String) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let exe = std::env::var("EDGER_DENO_BIN").unwrap_or_else(|_| "deno".into());
    let out = std::process::Command::new(&exe)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("real Deno is required (EDGER_DENO_BIN): {exe}: {e}"));
    assert!(out.status.success());
    eprintln!(
        "managed-evidence: {}",
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
    );
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    eprintln!("managed-evidence: {label} sqld pid={}", sqld.pid);
    let (user_base, admin_base) = (sqld.user_base.clone(), sqld.admin_base.clone());
    let outcome = tokio::spawn(async move {
        within(label, scenario(user_base, admin_base)).await;
    })
    .await;
    let stop = sqld.shutdown().await;
    eprintln!("managed-evidence: {label} shutdown {stop}");
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
}

fn worker_events(store: &OperationalStore, worker: &str) -> Vec<OperationalEvent> {
    let mut events: Vec<_> = egress_events(store)
        .into_iter()
        .filter(|e| e.worker.as_deref() == Some(worker))
        .collect();
    events.sort_by_key(|e| e.id);
    events
}

/// Wait (bounded) until the guard recorded `count` events for the worker,
/// then assert it is EXACTLY that many.
async fn wait_events(
    store: &OperationalStore,
    worker: &str,
    count: usize,
) -> Vec<OperationalEvent> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let events = worker_events(store, worker);
        if events.len() >= count || tokio::time::Instant::now() >= deadline {
            assert_eq!(events.len(), count, "exact event count for {worker}");
            return events;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real sqld 0.24.32 (EDGER_SQLD_BIN) and real Deno (EDGER_DENO_BIN)"]
async fn real_deno_streams_the_database_body_before_the_origin_finishes() {
    with_real_sqld_and_deno("deno db stream", deno_db_stream_scenario).await;
}

async fn deno_db_stream_scenario(user_base: String, admin_base: String) {
    let svc = build_svc(&admin_base, &user_base);
    let (store, guard) = store_and_guard();
    let w = worker_ref("dp-stream", "1.0.0", Some(RBAC));
    let ns = ensure_ready(&svc, &w).await;
    let tok = vault_token(&svc, &w).unwrap();
    let origin = ModalOrigin::start("http://127.0.0.1:1/x".into()).await;
    let egress = ManagedDatabaseEgress::new(
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        &w,
        &origin.base(),
    )
    .unwrap();
    // settings None: the plain managed database path (default limits).
    let mut fx = deno_db_fx(&w, &origin.base(), None);
    arm(&mut fx, &egress, &w, &origin.base());

    let result = std::panic::AssertUnwindSafe(async {
        // Warm: the spawn (and the Deno cache) is not part of the barrier wait.
        fx.get("/pid").await;
        let request = SerializedRequest {
            method: "GET".into(),
            uri: "/relay?path=/stream".into(),
            headers: vec![],
            body: None,
            request_id: "dp-stream-1".into(),
            base_href: None,
        };
        let config = fx.config.clone();
        let response =
            tokio::time::timeout(READ_WAIT, fx.isolate.execute_fetch_stream(request, &config))
                .await
                .expect("the head arrives BEFORE the barrier is released")
                .expect("execute_fetch_stream");
        let WorkerResponse::Streamed(mut stream) = response else {
            panic!("the guest answered a buffered response, not a stream");
        };
        assert_eq!(stream.status, 200);
        assert!(
            stream
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("x-origin-marker") && v == "streamed"),
            "the origin head reached the consumer: {:?}",
            stream.headers
        );
        // The first chunk arrives while the origin still holds the rest.
        let mut got: Vec<u8> = Vec::new();
        while !got.windows(12).any(|w| w == b"first-chunk-") {
            let item = tokio::time::timeout(READ_WAIT, stream.body.next())
                .await
                .expect("the first chunk streams BEFORE the barrier is released")
                .expect("the stream did not end early")
                .expect("chunk");
            got.extend_from_slice(&item);
        }
        assert_eq!(
            got, b"first-chunk-",
            "only the first chunk before the barrier"
        );
        assert!(
            !origin.second_sent.load(Ordering::SeqCst),
            "the barrier was still closed when the consumer had head + chunk"
        );
        assert!(!origin.barrier_timed_out.load(Ordering::SeqCst));
        // Release; the rest and the END arrive, the body is exact.
        origin.release.notify_one();
        while let Some(item) = tokio::time::timeout(READ_WAIT, stream.body.next())
            .await
            .expect("the rest of the body arrives after the release")
        {
            got.extend_from_slice(&item.expect("chunk after release"));
        }
        assert_eq!(got, b"first-chunk-second-chunk");
        assert!(origin.second_sent.load(Ordering::SeqCst));
        assert_eq!(origin.connections.load(Ordering::SeqCst), 1);
        {
            let heads = origin.heads.lock().unwrap();
            assert_eq!(heads.len(), 1);
            let (path, headers) = &heads[0];
            assert_eq!(path, "/stream");
            let value = |name: &str| -> Vec<&str> {
                headers
                    .iter()
                    .filter(|(k, _)| k == name)
                    .map(|(_, v)| v.as_str())
                    .collect()
            };
            assert_eq!(value("authorization"), vec![format!("Bearer {tok}")]);
            assert_eq!(value("x-namespace-bin"), vec![STANDARD_NO_PAD.encode(&ns)]);
        }
        let events = wait_events(&store, "dp-stream", 1).await;
        assert_eq!(events[0].outcome.as_deref(), Some("ok"));
        assert_eq!(events[0].status, Some(200));
        assert_eq!(events[0].code, None);
        assert_events_clean(&store, &[&tok]);
    })
    .await_unwind()
    .await;

    // Cleanup BEFORE re-raising: the owned Deno child and the origin.
    let terminated = fx.isolate.terminate().await;
    origin.stop().await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    terminated.expect("terminate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real sqld 0.24.32 (EDGER_SQLD_BIN) and real Deno (EDGER_DENO_BIN)"]
async fn real_deno_database_path_limits_redirect_cap_and_deadline() {
    with_real_sqld_and_deno("deno db limits", deno_db_limits_scenario).await;
}

async fn deno_db_limits_scenario(user_base: String, admin_base: String) {
    let svc = build_svc(&admin_base, &user_base);
    let (store, guard) = store_and_guard();
    let w = worker_ref("dp-limits", "1.0.0", Some(RBAC));
    ensure_ready(&svc, &w).await;
    let tok = vault_token(&svc, &w).unwrap();
    let redirect_target = FakeOrigin::start(Mode::Ok).await;
    let origin = ModalOrigin::start(format!("{}/followed", redirect_target.base())).await;
    let egress = ManagedDatabaseEgress::new(
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        &w,
        &origin.base(),
    )
    .unwrap();
    // The declared limits of a Deno guest travel in `config.egress` (the
    // sink context's limits ARE that value); that also makes the general
    // `settings` Some, which these DB-path cases never use.
    let mut tight = limits();
    tight.ports = vec![443];
    tight.headers_timeout_ms = 1_000;
    tight.total_timeout_ms = 3_000;
    tight.max_response_bytes = 10_000;
    tight.max_concurrent = 1;
    let mut fx = deno_db_fx(&w, &origin.base(), Some(tight));
    arm(&mut fx, &egress, &w, &origin.base());

    let result = std::panic::AssertUnwindSafe(async {
        fx.get("/pid").await;

        // control: a small answer works and takes the (only) slot briefly.
        let v = fx.get("/probe?path=/ok").await;
        assert_eq!(v["status"], 200, "{v}");
        assert_eq!(v["got"], 2, "{v}");
        assert!(v["readError"].is_null(), "{v}");
        let events = wait_events(&store, "dp-limits", 1).await;
        assert_eq!(events[0].outcome.as_deref(), Some("ok"));

        // 3xx: refused, never followed (the redirect target sees nothing).
        let v = fx.get("/probe?path=/redirect").await;
        eprintln!("managed-evidence: redirect => {v}");
        assert_eq!(v["status"], 403, "{v}");
        assert_eq!(v["egressError"], "EGRESS_DATABASE_DENIED", "{v}");
        let events = wait_events(&store, "dp-limits", 2).await;
        assert_eq!(events[1].code.as_deref(), Some("EGRESS_DATABASE_DENIED"));
        assert_eq!(
            redirect_target.connections.load(Ordering::SeqCst),
            0,
            "the Location was never followed"
        );
        assert!(redirect_target.heads.lock().unwrap().is_empty());

        // Over the cap, no Content-Length: cut with an error, never a clean
        // truncated success (a read error / error answer, <= cap bytes).
        let v = fx.get("/probe?path=/big").await;
        eprintln!("managed-evidence: big => {v}");
        assert_eq!(v["status"], 200, "the head was already sent: {v}");
        assert!(
            !v["readError"].is_null(),
            "the cut is a body read ERROR, not a clean truncated success: {v}"
        );
        assert!(v["got"].as_u64().unwrap() <= 10_000, "{v}");
        let events = wait_events(&store, "dp-limits", 3).await;
        assert_eq!(events[2].outcome.as_deref(), Some("error"));

        // Over the cap, declared Content-Length.
        let v = fx.get("/probe?path=/big-cl").await;
        eprintln!("managed-evidence: big-cl => {v}");
        assert_eq!(v["status"], 502, "{v}");
        assert_eq!(v["egressError"], "EGRESS_RESPONSE_TOO_LARGE", "{v}");
        assert!(v["readError"].is_null() && v["fetchError"].is_null(), "{v}");
        assert!(v["got"].as_u64().unwrap() <= 10_000, "{v}");
        let events = wait_events(&store, "dp-limits", 4).await;
        assert_eq!(events[3].outcome.as_deref(), Some("error"));

        // A hung origin ends under the deadline (headers 1 s / total 3 s).
        let started = std::time::Instant::now();
        let v = fx.get("/probe?path=/hang").await;
        let took = started.elapsed();
        eprintln!("managed-evidence: hang => {v} in {took:?}");
        assert!(took < Duration::from_secs(10), "deadline held: {took:?}");
        assert_eq!(v["status"], 504, "{v}");
        assert_eq!(v["egressError"], "EGRESS_TIMEOUT", "{v}");
        let events = wait_events(&store, "dp-limits", 5).await;
        assert_eq!(events[4].outcome.as_deref(), Some("timeout"));
        // The guard closed the upstream connection of the hung origin.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while origin.hang_closed.load(Ordering::SeqCst) == 0
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(origin.hang_closed.load(Ordering::SeqCst), 1, "peer closed");

        // The slot (max_concurrent 1) is free again after every failure.
        let v = fx.get("/probe?path=/ok").await;
        assert_eq!(v["status"], 200, "slot released: {v}");
        assert_eq!(v["got"], 2);
        let events = wait_events(&store, "dp-limits", 6).await;
        assert_eq!(events[5].outcome.as_deref(), Some("ok"));
        assert!(
            events
                .iter()
                .all(|e| e.code.as_deref() != Some("EGRESS_LIMIT")),
            "no call was ever limited: the slot was released each time"
        );
        assert_eq!(
            origin.connections.load(Ordering::SeqCst),
            6,
            "one upstream connection per call, none retried"
        );
        assert_events_clean(&store, &[&tok]);
    })
    .await_unwind()
    .await;

    let terminated = fx.isolate.terminate().await;
    origin.stop().await;
    redirect_target.stop().await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    terminated.expect("terminate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real sqld 0.24.32 (EDGER_SQLD_BIN) and real Deno (EDGER_DENO_BIN)"]
async fn real_deno_guest_database_and_explicit_general_egress_coexist() {
    with_real_sqld_and_deno("deno coexist", deno_coexist_scenario).await;
}

async fn deno_coexist_scenario(user_base: String, admin_base: String) {
    let svc = build_svc(&admin_base, &user_base);
    let store = OperationalStore::default();
    let guard = tls_guard(&store);
    let some_w = worker_ref("co-some", "1.0.0", Some(RBAC));
    let none_w = worker_ref("co-none", "1.0.0", Some(RBAC));
    ensure_ready(&svc, &some_w).await;
    ensure_ready(&svc, &none_w).await;
    let tok_some = vault_token(&svc, &some_w).unwrap();
    let tok_none = vault_token(&svc, &none_w).unwrap();
    let up = tls_upstream().await;
    let https_ok = enc(&format!("https://127.0.0.1:{}/ok", up.addr.port()));
    let other = FakeOrigin::start(Mode::Ok).await;
    let mut general = limits();
    general.ports = vec![up.addr.port()];

    let mk = |w: &WorkerRef| {
        ManagedDatabaseEgress::new(Arc::clone(&svc.service), Arc::clone(&guard), w, &user_base)
            .unwrap()
    };
    let (e_some, e_none) = (mk(&some_w), mk(&none_w));
    let mut some = deno_db_fx(&some_w, &user_base, Some(general.clone()));
    let mut none = deno_db_fx(&none_w, &user_base, None);
    arm(&mut some, &e_some, &some_w, &user_base);
    arm(&mut none, &e_none, &none_w, &user_base);
    let read = pct("SELECT 41 + 1");
    let cell = |v: &Value| first_cell(v["text"].as_str().unwrap().as_bytes());

    let result = std::panic::AssertUnwindSafe(async {
        // ── config.egress Some: the private DB and the explicit general
        //    protocol both work from the SAME guest process.
        let pid = some.get("/pid").await["pid"].as_u64().unwrap();
        let v = some.get(&format!("/db?sql={read}")).await;
        assert_eq!(v["status"], 200, "{v}");
        assert_eq!(cell(&v).as_deref(), Some("42"), "real SQLD answered: {v}");
        assert_eq!(
            up.connections.load(Ordering::SeqCst),
            0,
            "DB never touches the general upstream"
        );

        let v = some.get(&format!("/general?target={https_ok}")).await;
        assert_eq!(v["status"], 200, "{v}");
        assert_eq!(v["text"], "hello-egress", "{v}");
        assert_eq!(
            up.connections.load(Ordering::SeqCst),
            1,
            "exactly the explicit call"
        );

        // A plain-http non-DB target stays refused by the general policy.
        let v = some
            .get(&format!("/general?target={}", enc(&other.base())))
            .await;
        assert_eq!(v["err"], "EGRESS_BAD_TARGET", "{v}");
        assert_eq!(other.connections.load(Ordering::SeqCst), 0);

        // A bare fetch of a non-DB URL is NOT proxied automatically: the
        // runtime denies it and the upstream sees no new connection.
        let v = some.get(&format!("/direct?target={https_ok}")).await;
        assert_eq!(v["direct"], "NotCapable", "{v}");
        assert_eq!(
            up.connections.load(Ordering::SeqCst),
            1,
            "no automatic proxy for a non-DB fetch"
        );
        // The DB still works after the general calls (same process).
        let v = some.get(&format!("/db?sql={read}")).await;
        assert_eq!(cell(&v).as_deref(), Some("42"), "{v}");
        assert_eq!(
            some.get("/pid").await["pid"].as_u64().unwrap(),
            pid,
            "one process"
        );

        // ── control config.egress None: DB works, the general explicit
        //    protocol is refused statically and nothing is dialed.
        let v = none.get(&format!("/db?sql={read}")).await;
        assert_eq!(cell(&v).as_deref(), Some("42"), "{v}");
        let before = up.connections.load(Ordering::SeqCst);
        let v = none.get(&format!("/general?target={https_ok}")).await;
        assert_eq!(
            v["err"], "EGRESS_BAD_TARGET",
            "None denies the general path: {v}"
        );
        assert_ne!(v["status"], 200, "{v}");
        assert_eq!(
            up.connections.load(Ordering::SeqCst),
            before,
            "None dials nothing"
        );
        let v = none.get(&format!("/direct?target={https_ok}")).await;
        assert_eq!(v["direct"], "NotCapable", "{v}");
        assert_eq!(up.connections.load(Ordering::SeqCst), before);

        // Exact guard events: Some -> db ok, general ok, general http denied,
        // db ok; None -> db ok, general https denied.
        let events = wait_events(&store, "co-some", 4).await;
        let events_none = wait_events(&store, "co-none", 2).await;
        let shape = |events: &[OperationalEvent]| -> Vec<(Option<String>, Option<String>)> {
            events
                .iter()
                .map(|e| (e.outcome.clone(), e.code.clone()))
                .collect()
        };
        eprintln!("managed-evidence: co-some events {:?}", shape(&events));
        eprintln!("managed-evidence: co-none events {:?}", shape(&events_none));
        assert_eq!(events[0].outcome.as_deref(), Some("ok"));
        assert_eq!(events[1].outcome.as_deref(), Some("ok"));
        assert_eq!(events[2].code.as_deref(), Some("EGRESS_BAD_TARGET"));
        assert_eq!(events[3].outcome.as_deref(), Some("ok"));
        assert_eq!(events_none[0].outcome.as_deref(), Some("ok"));
        assert_eq!(events_none[1].code.as_deref(), Some("EGRESS_BAD_TARGET"));
        assert_events_clean(&store, &[&tok_some, &tok_none]);
    })
    .await_unwind()
    .await;

    let t1 = some.isolate.terminate().await;
    let t2 = none.isolate.terminate().await;
    other.stop().await;
    up.task.abort();
    let _ = up.task.await;
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    t1.expect("terminate some");
    t2.expect("terminate none");
}
