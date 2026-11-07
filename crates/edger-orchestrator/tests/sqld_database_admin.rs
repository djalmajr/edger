#![cfg(unix)]
//! (EDG-54 / 15e) Admin API root do banco SQLD gerenciado + a função PURA de
//! configuração host (`apply_managed_database_config`).
//!
//! Níveis de prova (o relatório os mantém separados):
//!
//! * HTTP em processo: o `Router` REAL de `build_pipeline` (axum `oneshot`),
//!   com índice, registry, cofre, backup e `SqldProvisionService` reais em
//!   diretórios temporários 0700. O fluxo PUT → GET `ready` → POST backup passa
//!   por `service.ensure`/`service.backup` reais (nenhum `mark_ready` manual).
//! * SQLD: um servidor TCP controlado (NÃO é o SQLD real) que conta as chamadas;
//!   "zero chamadas" prova que nada remoto aconteceu antes da recusa.
//! * Função pura: testes diretos da API pública, sem I/O.
//!
//! Não prova CLI real, SQLD real, grant/renovação em runtime, SSE nem fábrica.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use edger_core::{
    create_worker_ref, ExecutionKind, WorkerManifest, WorkerOrigin, WorkerRef, WorkerVisibility,
};
use edger_isolation::MockIsolate;
use edger_orchestrator::app_secrets::AppSecretVault;
use edger_orchestrator::managed_database_config::{
    apply_managed_database_config, ManagedDatabaseConfigError, SQLD_URL_ENV,
};
use edger_orchestrator::sqld_admin::SqldAdminClient;
use edger_orchestrator::sqld_backup::SqldBackupStore;
use edger_orchestrator::sqld_provision::{
    SqldProvisionService, SqldSystemClock, SQLD_TOKEN_SECRET,
};
use edger_orchestrator::sqld_registry::{
    SqldProvisionRegistry, SqldRegistryOwner, SqldRegistryState, MIN_QUOTA_BYTES,
};
use edger_orchestrator::{
    build_pipeline, ApiKeyService, ConsoleAuthService, ControlAuth, ControlAuthConfig,
    ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::{Service, ServiceExt};
use zeroize::Zeroizing;

const ADMIN_KEY: &str = "canary-admin-sqld-root-api";
const LEDGER_MASTER: [u8; 32] = [0x42; 32];
const VAULT_MASTER: [u8; 32] = [0x24; 32];
const QUOTA: u64 = MIN_QUOTA_BYTES;
const BUDGET: u64 = 8 * MIN_QUOTA_BYTES;
const ROOT_KEY: &str = "test-root-sqld-api";
const ROOT_PASSWORD: &str = "Str0ng!Passw0rd";
const OP_PASSWORD: &str = "Op3rator!Passw0rd";
const FAKE_CREDENTIAL: &str = "canary-visitor-credential-zz9";
const MANUAL_TOKEN: &str = "canary-manual-token-value-q7";

// ───────────────────────── controlled SQLD (TCP) ─────────────────────────

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    auth: Option<String>,
    ns_bin: Option<String>,
    body: Vec<u8>,
}

struct Ns {
    jwt_key: Value,
    max_pages: i64,
}

#[derive(Default)]
struct FakeState {
    namespaces: HashMap<String, Ns>,
    log: Vec<Req>,
    /// `Some(code)`: CREATE answers `code` WITHOUT registering the namespace.
    create_status: Option<u16>,
}

struct Fake {
    state: Arc<Mutex<FakeState>>,
    admin_base: String,
    data_base: String,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Fake {
    async fn start() -> Self {
        let state = Arc::new(Mutex::new(FakeState::default()));
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
    let (mut auth, mut ns_bin, mut len) = (None, None, 0usize);
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
    Dump,
}

fn ns_from_path(path: &str) -> Option<&str> {
    path.strip_prefix("/v1/namespaces/")?.split('/').next()
}

/// Admin API (needs `Basic <ADMIN_KEY>`; 401 otherwise), data pipeline
/// (`PRAGMA max_page_count` honest per namespace) and `/dump`.
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
                if let Some(code) = st.create_status {
                    Act::Empty(code)
                } else {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                    let pages = body["max_db_size"].as_u64().unwrap_or(0) / 4096;
                    st.namespaces.insert(
                        ns,
                        Ns {
                            jwt_key: body["jwt_key"].clone(),
                            max_pages: pages as i64,
                        },
                    );
                    Act::Empty(200)
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
            match pages {
                None => Act::Empty(404),
                Some(p) => Act::Json(
                    200,
                    json!({"results": [{"type": "ok", "response": {"type": "execute", "result": {
                        "rows": [[{"type": "integer", "value": p.to_string()}]]}}},
                        {"type": "ok", "response": {"type": "close"}}]}),
                ),
            }
        } else if req.path == "/dump" {
            Act::Dump
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
        Act::Dump => respond(&mut stream, 200, None, b"BEGIN TRANSACTION;\nCOMMIT;\n").await,
    }
    Some(())
}

// ───────────────────────── fixture ─────────────────────────

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn private_dir(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// A Deno-compatible manifest: `allowNet: []` explicit.
fn deno(name: &str, version: &str) -> WorkerManifest {
    WorkerManifest {
        name: name.into(),
        version: Some(version.into()),
        allow_net: Some(vec![]),
        ..Default::default()
    }
}

struct Install {
    manifest: WorkerManifest,
    origin: WorkerOrigin,
    staged: bool,
}

impl Install {
    fn user(manifest: WorkerManifest) -> Self {
        Self {
            manifest,
            origin: WorkerOrigin::User,
            staged: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    /// Static root key + API keys + seeded console (root password).
    Configured,
    /// No credential of any kind (open mode).
    Open,
}

struct Fixture {
    dir: tempfile::TempDir,
    fake: Fake,
    state: OrchestratorState,
    app: Router,
    registry: Arc<SqldProvisionRegistry>,
    vault: Arc<AppSecretVault>,
    origin: String,
}

impl Fixture {
    async fn new(installs: Vec<Install>) -> Self {
        Self::build(installs, AuthMode::Configured, true).await
    }

    async fn build(installs: Vec<Install>, mode: AuthMode, with_service: bool) -> Self {
        Self::build_with(move |_| installs, mode, with_service).await
    }

    /// `installs` learns the host data origin (the fake picks a random port).
    async fn new_with(installs: impl FnOnce(&str) -> Vec<Install>) -> Self {
        Self::build_with(installs, AuthMode::Configured, true).await
    }

    async fn build_with(
        installs: impl FnOnce(&str) -> Vec<Install>,
        mode: AuthMode,
        with_service: bool,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        private_dir(dir.path(), "ledger");
        private_dir(dir.path(), "backups");
        let fake = Fake::start().await;
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
                .expect("vault"),
        );
        let service = Arc::new(
            SqldProvisionService::new(
                Arc::clone(&registry),
                Arc::clone(&vault),
                SqldAdminClient::new(&fake.admin_base, Zeroizing::new(ADMIN_KEY.to_owned()))
                    .unwrap(),
                SqldBackupStore::new(&fake.data_base, dir.path().join("backups")).unwrap(),
                &fake.data_base,
                Arc::new(SqldSystemClock),
            )
            .expect("service builds"),
        );
        let origin = format!("{}/", fake.data_base);

        let mut index = ManifestIndex::new();
        for (n, install) in installs(&origin).into_iter().enumerate() {
            let worker_dir = dir.path().join(format!("w{n}"));
            std::fs::create_dir(&worker_dir).unwrap();
            if install.staged {
                std::fs::write(worker_dir.join(".edger-revision"), "rev-1\nstaged=true\n").unwrap();
            }
            index
                .insert_with_origin(worker_dir, install.manifest, install.origin)
                .expect("index insert");
        }

        let auth = match mode {
            AuthMode::Configured => {
                let console = Arc::new(ConsoleAuthService::in_memory().unwrap());
                console.seed_root_if_empty(Some(ROOT_PASSWORD)).unwrap();
                ControlAuth::with_static_key(ROOT_KEY)
                    .with_key_service(Arc::new(ApiKeyService::in_memory().unwrap()))
                    .with_console_service(console)
            }
            AuthMode::Open => {
                let auth = ControlAuth::new(ControlAuthConfig::default());
                assert!(auth.is_open());
                auth
            }
        };
        let server = ServerState::new_unready();
        let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
        server.mark_ready(pool.clone());
        if with_service {
            server.set_sqld_managed(Arc::clone(&service), origin.clone());
        }
        let state = OrchestratorState {
            server,
            pool,
            index,
            auth,
        };
        let app = build_pipeline(state.clone());
        Self {
            dir,
            fake,
            state,
            app,
            registry,
            vault,
            origin,
        }
    }

    /// One HTTP exchange through the real router (real peer address injected
    /// like the production listener). Every response of the three database
    /// routes must carry `Cache-Control: no-store`.
    async fn call(&self, method: &str, uri: &str, headers: &[(&str, &str)], body: Body) -> Reply {
        let mut make = self
            .app
            .clone()
            .into_make_service_with_connect_info::<SocketAddr>();
        let service = make
            .call(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                54321,
            ))
            .await
            .unwrap();
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = service.oneshot(builder.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let response_headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let path = uri.split('?').next().unwrap_or(uri);
        let segments: Vec<&str> = path.split('/').collect();
        let is_database_route = segments.len() >= 6
            && segments[..4] == ["", "api", "admin", "workers"]
            && ((segments.len() == 6 && segments[5] == "database")
                || (segments.len() == 7 && segments[5..] == ["database", "backup"]));
        if is_database_route {
            assert_eq!(
                response_headers
                    .get("cache-control")
                    .and_then(|v| v.to_str().ok()),
                Some("no-store"),
                "{method} {uri} -> {status}: {text}"
            );
        }
        Reply {
            status,
            json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            text,
        }
    }

    /// Root via the static key (Bearer).
    async fn root(&self, method: &str, uri: &str, body: Value) -> Reply {
        let bearer = format!("Bearer {ROOT_KEY}");
        let payload = if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        };
        self.call(
            method,
            uri,
            &[
                ("authorization", bearer.as_str()),
                ("content-type", "application/json"),
            ],
            payload,
        )
        .await
    }

    fn owner_record(&self, worker: &str, ns: Option<&str>) -> Option<SqldRegistryState> {
        self.registry
            .get(&SqldRegistryOwner {
                worker,
                rbac_namespace: ns,
            })
            .unwrap()
            .map(|r| r.state)
    }

    fn backup_files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path().join("backups"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".sql"))
            .collect();
        names.sort();
        names
    }

    async fn finish(self) {
        let Fixture {
            fake,
            dir,
            app,
            state,
            registry,
            vault,
            ..
        } = self;
        drop((app, state, registry, vault));
        fake.stop().await;
        drop(dir);
    }
}

struct Reply {
    status: StatusCode,
    json: Value,
    text: String,
}

fn put_uri(name: &str) -> String {
    format!("/api/admin/workers/{name}/database")
}

fn backup_uri(name: &str) -> String {
    format!("/api/admin/workers/{name}/database/backup")
}

fn quota_body() -> Value {
    json!({ "quotaBytes": QUOTA })
}

/// A body that records how many chunks the server pulled from it.
fn counting_body(chunks: Vec<Vec<u8>>, polled: Arc<AtomicUsize>) -> Body {
    let stream = futures_util::stream::iter(chunks).map(move |chunk| {
        polled.fetch_add(1, Ordering::SeqCst);
        Ok::<_, std::convert::Infallible>(Bytes::from(chunk))
    });
    Body::from_stream(stream)
}

fn assert_static_error(reply: &Reply, status: StatusCode, code: &str) {
    assert_eq!(reply.status, status, "{}", reply.text);
    assert_eq!(reply.json["code"], code, "{}", reply.text);
    assert!(
        reply.json.as_object().is_some_and(|o| o.len() == 2),
        "{}",
        reply.text
    );
    for forbidden in [
        ROOT_KEY,
        ADMIN_KEY,
        FAKE_CREDENTIAL,
        MANUAL_TOKEN,
        "sqld",
        "SqldProvision",
        "Debug",
        "127.0.0.1",
    ] {
        assert!(
            !reply.text.contains(forbidden),
            "{forbidden} leaked: {}",
            reply.text
        );
    }
}

const ALL_ROUTES: [(&str, bool); 3] = [("GET", false), ("PUT", true), ("POST", true)];

fn route_uri(method: &str, name: &str) -> String {
    if method == "POST" {
        backup_uri(name)
    } else {
        put_uri(name)
    }
}

fn route_body(method: &str) -> Body {
    match method {
        "PUT" => Body::from(quota_body().to_string()),
        _ => Body::empty(),
    }
}

// ───────────────────────── auth ─────────────────────────

#[tokio::test]
async fn missing_or_invalid_credentials_are_401_with_no_remote_call_in_configured_mode() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let bogus = format!("Bearer {FAKE_CREDENTIAL}");
    let egk = format!("Bearer egk_{FAKE_CREDENTIAL}");
    let ses = format!("Bearer ses-{FAKE_CREDENTIAL}");
    let variants: Vec<Vec<(&str, &str)>> = vec![
        vec![],
        vec![("authorization", bogus.as_str())],
        vec![("authorization", egk.as_str())],
        vec![("authorization", ses.as_str())],
        vec![("x-edger-internal", "true")],
        vec![
            ("x-edger-internal", "true"),
            ("authorization", bogus.as_str()),
        ],
        // The control header is for the invoke route only; it never authorizes this API.
        vec![("x-edger-control-authorization", "Bearer test-root-sqld-api")],
    ];
    for (method, _) in ALL_ROUTES {
        for headers in &variants {
            let reply = fx
                .call(
                    method,
                    &route_uri(method, "app"),
                    headers,
                    route_body(method),
                )
                .await;
            assert_static_error(&reply, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
        }
    }
    assert_eq!(fx.fake.total(), 0);
    assert_eq!(fx.owner_record("app", None), None);
    fx.finish().await;
}

#[tokio::test]
async fn open_mode_never_authorizes_the_database_routes() {
    let fx = Fixture::build(
        vec![Install::user(deno("app", "1.0.0"))],
        AuthMode::Open,
        true,
    )
    .await;
    let bogus = format!("Bearer {FAKE_CREDENTIAL}");
    let egk = format!("Bearer egk_{FAKE_CREDENTIAL}");
    let variants: Vec<Vec<(&str, &str)>> = vec![
        vec![],
        vec![("authorization", bogus.as_str())],
        vec![("authorization", egk.as_str())],
        vec![("x-edger-internal", "true")],
        vec![
            ("x-edger-internal", "true"),
            ("authorization", bogus.as_str()),
        ],
        vec![("x-api-key", FAKE_CREDENTIAL)],
    ];
    for (method, _) in ALL_ROUTES {
        for headers in &variants {
            let reply = fx
                .call(
                    method,
                    &route_uri(method, "app"),
                    headers,
                    route_body(method),
                )
                .await;
            assert_static_error(&reply, StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
        }
    }
    assert_eq!(fx.fake.total(), 0);
    assert_eq!(fx.owner_record("app", None), None);
    // Control: the open mode itself still exists for the OLD routes (the new
    // routes are the ones that refuse it).
    let old = fx
        .call("GET", "/api/admin/workers", &[], Body::empty())
        .await;
    assert_eq!(old.status, StatusCode::OK, "{}", old.text);
    fx.finish().await;
}

#[tokio::test]
async fn auth_runs_before_the_body_is_consumed() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let bogus = format!("Bearer {FAKE_CREDENTIAL}");
    for headers in [vec![], vec![("authorization", bogus.as_str())]] {
        for method in ["PUT", "POST"] {
            let polled = Arc::new(AtomicUsize::new(0));
            let body = counting_body(vec![vec![b' '; 600], vec![b' '; 600]], Arc::clone(&polled));
            let reply = fx
                .call(method, &route_uri(method, "app"), &headers, body)
                .await;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.text);
            assert_eq!(polled.load(Ordering::SeqCst), 0, "body read before auth");
        }
    }
    assert_eq!(fx.fake.total(), 0);
    fx.finish().await;
}

/// Creates a non-root API key (role `admin`, every permission) with the root
/// key and returns its raw value.
async fn non_root_admin_key(fx: &Fixture) -> String {
    let created = fx
        .root(
            "POST",
            "/api/admin/keys",
            json!({"name": "adm", "role": "admin",
                   "permissions": edger_core::security::PERMISSION_CATALOG,
                   "namespaces": ["*"], "workers": ["*"]}),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    created.json["rawKey"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn non_root_admin_keys_are_403_before_the_body_and_the_service() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let key = non_root_admin_key(&fx).await;
    let bearer = format!("Bearer {key}");
    for (method, _) in ALL_ROUTES {
        let reply = fx
            .call(
                method,
                &route_uri(method, "app"),
                &[("authorization", bearer.as_str())],
                route_body(method),
            )
            .await;
        assert_static_error(&reply, StatusCode::FORBIDDEN, "FORBIDDEN");
    }
    // x-edger-internal does not turn a non-root into a root.
    let internal = fx
        .call(
            "PUT",
            &put_uri("app"),
            &[
                ("authorization", bearer.as_str()),
                ("x-edger-internal", "true"),
            ],
            Body::from(quota_body().to_string()),
        )
        .await;
    assert_eq!(internal.status, StatusCode::FORBIDDEN, "{}", internal.text);
    // Oversized body from a non-root: refused as 403 WITHOUT reading it.
    let polled = Arc::new(AtomicUsize::new(0));
    let reply = fx
        .call(
            "PUT",
            &put_uri("app"),
            &[("authorization", bearer.as_str())],
            counting_body(vec![vec![b' '; 5000]], Arc::clone(&polled)),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(polled.load(Ordering::SeqCst), 0);
    assert_eq!(fx.fake.total(), 0);
    assert_eq!(fx.owner_record("app", None), None);
    fx.finish().await;
}

#[tokio::test]
async fn temporary_password_session_hits_the_password_gate_first() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let created = fx
        .root(
            "POST",
            "/api/admin/users",
            json!({"username": "opuser", "password": OP_PASSWORD, "role": "operator",
                   "permissions": edger_core::security::PERMISSION_CATALOG,
                   "namespaces": ["*"], "workers": ["*"]}),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let login = fx
        .call(
            "POST",
            "/api/admin/login",
            &[
                ("host", "edger.local"),
                ("content-type", "application/json"),
            ],
            Body::from(json!({"username": "opuser", "password": OP_PASSWORD}).to_string()),
        )
        .await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.text);
    assert_eq!(login.json["mustChangePassword"], true);
    let bearer = format!("Bearer {}", login.json["token"].as_str().unwrap());
    for (method, _) in ALL_ROUTES {
        let reply = fx
            .call(
                method,
                &route_uri(method, "app"),
                &[("authorization", bearer.as_str())],
                route_body(method),
            )
            .await;
        assert_static_error(&reply, StatusCode::FORBIDDEN, "PASSWORD_CHANGE_REQUIRED");
    }
    assert_eq!(fx.fake.total(), 0);
    fx.finish().await;
}

#[tokio::test]
async fn real_root_console_session_and_root_bearer_are_accepted() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let login = fx
        .call(
            "POST",
            "/api/admin/login",
            &[
                ("host", "edger.local"),
                ("content-type", "application/json"),
            ],
            Body::from(json!({"username": "root", "password": ROOT_PASSWORD}).to_string()),
        )
        .await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.text);
    assert_eq!(login.json["mustChangePassword"], false, "{}", login.text);
    let session = format!("Bearer {}", login.json["token"].as_str().unwrap());
    let via_session = fx
        .call(
            "GET",
            &put_uri("app"),
            &[("authorization", session.as_str())],
            Body::empty(),
        )
        .await;
    assert_eq!(via_session.status, StatusCode::OK, "{}", via_session.text);
    assert_eq!(
        via_session.json,
        json!({"database": null, "activation": "nextSpawn"})
    );
    let via_bearer = fx.root("GET", &put_uri("app"), Value::Null).await;
    assert_eq!(via_bearer.status, StatusCode::OK, "{}", via_bearer.text);
    // A mutation with the root session works end to end too.
    let put = fx
        .call(
            "PUT",
            &put_uri("app"),
            &[
                ("authorization", session.as_str()),
                ("content-type", "application/json"),
            ],
            Body::from(quota_body().to_string()),
        )
        .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.text);
    assert_eq!(put.json["database"]["state"], "ready");
    fx.finish().await;
}

// ───────────────────────── origin / CSRF ─────────────────────────

#[tokio::test]
async fn cross_origin_root_mutations_are_403_before_ensure_or_backup() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let bearer = format!("Bearer {ROOT_KEY}");
    let hostile: Vec<Vec<(&str, &str)>> = vec![
        vec![("origin", "http://evil.example"), ("host", "edger.local")],
        vec![
            ("origin", "http://edger.local.evil.example"),
            ("host", "edger.local"),
        ],
        // Browser-originated without an Origin header: required, so refused.
        vec![("sec-fetch-site", "cross-site"), ("host", "edger.local")],
        vec![("origin", "http://edger.local")], // no host header
    ];
    for method in ["PUT", "POST"] {
        for extra in &hostile {
            let mut headers = vec![("authorization", bearer.as_str())];
            headers.extend(extra.iter().copied());
            let reply = fx
                .call(
                    method,
                    &route_uri(method, "app"),
                    &headers,
                    route_body(method),
                )
                .await;
            assert_static_error(&reply, StatusCode::FORBIDDEN, "CSRF_DENIED");
        }
    }
    assert_eq!(fx.fake.total(), 0, "cross-origin reached the service");
    assert_eq!(fx.owner_record("app", None), None);
    // Control: same-origin browser request passes the same guard.
    let ok = fx
        .call(
            "PUT",
            &put_uri("app"),
            &[
                ("authorization", bearer.as_str()),
                ("origin", "http://edger.local"),
                ("host", "edger.local"),
            ],
            Body::from(quota_body().to_string()),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    fx.finish().await;
}

// ───────────────────────── owner / route shape ─────────────────────────

#[tokio::test]
async fn absent_core_and_encoded_scoped_owners_come_only_from_the_index() {
    let fx = Fixture::new(vec![
        Install::user(deno("@acme/app", "1.0.0")),
        Install {
            manifest: WorkerManifest {
                name: "cpanel".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
            origin: WorkerOrigin::CoreBundled,
            staged: false,
        },
    ])
    .await;
    for (method, _) in ALL_ROUTES {
        // Absent: 404 (never an owner invented from the path).
        let reply = fx
            .root(method, &route_uri(method, "ghost"), body_for(method))
            .await;
        assert_static_error(&reply, StatusCode::NOT_FOUND, "NOT_FOUND");
        // Hostile names: still absent from the index.
        for hostile in [
            "%2e%2e",
            "%00",
            "%40acme",
            "@acme%2Fghost",
            "app%2F..%2Fapp",
        ] {
            let reply = fx
                .root(method, &route_uri(method, hostile), body_for(method))
                .await;
            assert!(
                matches!(
                    reply.status,
                    StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
                ),
                "{hostile}: {}",
                reply.text
            );
        }
        // Core: 403.
        let reply = fx
            .root(method, &route_uri(method, "cpanel"), body_for(method))
            .await;
        assert_static_error(&reply, StatusCode::FORBIDDEN, "FORBIDDEN");
    }
    assert_eq!(fx.fake.total(), 0);
    assert_eq!(fx.owner_record("cpanel", None), None);
    // Unencoded slash is a different route shape, not the scoped worker.
    let raw = fx
        .root("GET", "/api/admin/workers/@acme/app/database", Value::Null)
        .await;
    assert!(
        matches!(
            raw.status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ),
        "{}",
        raw.text
    );
    // Encoded `@scope/app` resolves to the full name + RBAC namespace.
    let put = fx
        .root("PUT", &put_uri("%40acme%2Fapp"), quota_body())
        .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.text);
    assert_eq!(
        fx.owner_record("@acme/app", Some("@acme")),
        Some(SqldRegistryState::Ready)
    );
    assert_eq!(fx.owner_record("@acme/app", None), None);
    assert_eq!(fx.owner_record("app", Some("@acme")), None);
    let get = fx.root("GET", &put_uri("%40acme%2Fapp"), Value::Null).await;
    assert_eq!(get.json["database"]["state"], "ready");
    fx.finish().await;
}

fn body_for(method: &str) -> Value {
    if method == "PUT" {
        quota_body()
    } else {
        Value::Null
    }
}

#[tokio::test]
async fn core_overlay_reference_of_the_same_name_denies_even_with_a_user_version() {
    let fx = Fixture::new(vec![
        Install {
            manifest: WorkerManifest {
                name: "cpanel".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
            origin: WorkerOrigin::CoreBundled,
            staged: false,
        },
        Install {
            manifest: WorkerManifest {
                name: "cpanel".into(),
                version: Some("2.0.0".into()),
                ..Default::default()
            },
            origin: WorkerOrigin::CoreOverlay,
            staged: false,
        },
    ])
    .await;
    let reply = fx.root("PUT", &put_uri("cpanel"), quota_body()).await;
    assert_static_error(&reply, StatusCode::FORBIDDEN, "FORBIDDEN");
    assert_eq!(fx.fake.total(), 0);
    fx.finish().await;
}

#[tokio::test]
async fn query_strings_and_extra_routes_are_refused() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    for query in [
        "?version=1.0.0",
        "?namespace=other",
        "?quotaBytes=16777216",
        "?",
        "?url=http://evil.example",
    ] {
        for (method, _) in ALL_ROUTES {
            let uri = format!("{}{query}", route_uri(method, "app"));
            let reply = fx.root(method, &uri, body_for(method)).await;
            assert_static_error(&reply, StatusCode::BAD_REQUEST, "INVALID_SQLD_REQUEST");
        }
    }
    // No DELETE, no PATCH, no GET on backup, no download route.
    for (method, uri) in [
        ("DELETE", put_uri("app")),
        ("PATCH", put_uri("app")),
        ("POST", put_uri("app")),
        ("GET", backup_uri("app")),
        ("PUT", backup_uri("app")),
        ("DELETE", backup_uri("app")),
    ] {
        let reply = fx.root(method, &uri, Value::Null).await;
        assert_eq!(
            reply.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {uri}"
        );
    }
    for uri in [
        "/api/admin/workers/app/database/download",
        "/api/admin/workers/app/database/backup/file.sql",
        "/api/admin/workers/app/database/backups",
    ] {
        let reply = fx.root("GET", uri, Value::Null).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{uri}");
    }
    assert_eq!(fx.fake.total(), 0);
    assert_eq!(fx.owner_record("app", None), None);
    fx.finish().await;
}

#[tokio::test]
async fn service_not_registered_is_503_without_building_anything() {
    let fx = Fixture::build(
        vec![Install::user(deno("app", "1.0.0"))],
        AuthMode::Configured,
        false,
    )
    .await;
    for (method, _) in ALL_ROUTES {
        let reply = fx
            .root(method, &route_uri(method, "app"), body_for(method))
            .await;
        assert_static_error(&reply, StatusCode::SERVICE_UNAVAILABLE, "SQLD_UNAVAILABLE");
    }
    assert_eq!(fx.fake.total(), 0);
    assert!(fx.state.server.sqld_service().is_none());
    fx.finish().await;
}

#[tokio::test]
async fn server_state_registers_the_service_once_first_wins() {
    let fx = Fixture::new(vec![]).await;
    let first = Arc::clone(fx.state.server.sqld_service().expect("registered"));
    let other = Fixture::new(vec![]).await;
    let second = Arc::clone(other.state.server.sqld_service().unwrap());
    fx.state
        .server
        .set_sqld_managed(Arc::clone(&second), "http://other.example/");
    assert!(Arc::ptr_eq(fx.state.server.sqld_service().unwrap(), &first));
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(fx.state.server.sqld_data_origin(), Some(fx.origin.as_str()));
    other.finish().await;
    fx.finish().await;
}

// ───────────────────────── request body ─────────────────────────

#[tokio::test]
async fn put_body_shape_is_validated_before_the_service() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let bad_bodies: Vec<String> = vec![
        String::new(),
        "not json".into(),
        "null".into(),
        "[]".into(),
        "{}".into(),
        r#"{"quotaBytes":"16777216"}"#.into(),
        r#"{"quotaBytes":-1}"#.into(),
        r#"{"quotaBytes":16777216.5}"#.into(),
        r#"{"quotaBytes":null}"#.into(),
        r#"{"quotaBytes":0}"#.into(),
        r#"{"quotaBytes":4096}"#.into(),
        format!(r#"{{"quotaBytes":{}}}"#, QUOTA + 1),
        format!(r#"{{"quotaBytes":{}}}"#, 1u64 << 63),
        format!(r#"{{"quotaBytes":{}}}"#, u64::MAX),
        format!("[{QUOTA}]"),
        format!(r#"{{"quotaBytes":{QUOTA},"namespace":"x"}}"#),
        format!(r#"{{"quotaBytes":{QUOTA},"version":"1.0.0"}}"#),
        format!(r#"{{"quotaBytes":{QUOTA},"url":"http://evil.example"}}"#),
        format!(r#"{{"quotaBytes":{QUOTA},"jwt":"x","owner":"y"}}"#),
        format!(r#"{{"quotaBytes":{QUOTA},"quotaBytes":{QUOTA}}}"#),
    ];
    for body in bad_bodies {
        let reply = fx
            .call(
                "PUT",
                &put_uri("app"),
                &[
                    ("authorization", &format!("Bearer {ROOT_KEY}")),
                    ("content-type", "application/json"),
                ],
                Body::from(body.clone()),
            )
            .await;
        assert_static_error(&reply, StatusCode::BAD_REQUEST, "INVALID_SQLD_REQUEST");
        assert_eq!(fx.fake.total(), 0, "body {body:?} reached the service");
    }
    assert_eq!(fx.owner_record("app", None), None);
    fx.finish().await;
}

#[tokio::test]
async fn quota_over_the_ledger_budget_is_400_and_creates_nothing() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let reply = fx
        .root(
            "PUT",
            &put_uri("app"),
            json!({"quotaBytes": BUDGET + MIN_QUOTA_BYTES}),
        )
        .await;
    assert_static_error(&reply, StatusCode::BAD_REQUEST, "INVALID_SQLD_REQUEST");
    assert_eq!(fx.fake.count("POST", "/create"), 0);
    assert_eq!(fx.owner_record("app", None), None);
    fx.finish().await;
}

#[tokio::test]
async fn body_cap_is_1024_by_header_and_by_stream_before_buffering() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let bearer = format!("Bearer {ROOT_KEY}");
    // Header over the cap: refused without pulling a single chunk.
    let polled = Arc::new(AtomicUsize::new(0));
    let reply = fx
        .call(
            "PUT",
            &put_uri("app"),
            &[
                ("authorization", bearer.as_str()),
                ("content-length", "1025"),
            ],
            counting_body(vec![vec![b' '; 1025]], Arc::clone(&polled)),
        )
        .await;
    assert_static_error(
        &reply,
        StatusCode::PAYLOAD_TOO_LARGE,
        "INVALID_SQLD_REQUEST",
    );
    assert_eq!(polled.load(Ordering::SeqCst), 0, "buffered despite header");
    // Unsized (chunked) body: abandoned at the first chunk crossing the cap.
    let polled = Arc::new(AtomicUsize::new(0));
    let reply = fx
        .call(
            "PUT",
            &put_uri("app"),
            &[("authorization", bearer.as_str())],
            counting_body(
                vec![
                    vec![b' '; 600],
                    vec![b' '; 600],
                    vec![b' '; 600],
                    vec![b' '; 600],
                ],
                Arc::clone(&polled),
            ),
        )
        .await;
    assert_static_error(
        &reply,
        StatusCode::PAYLOAD_TOO_LARGE,
        "INVALID_SQLD_REQUEST",
    );
    assert!(
        polled.load(Ordering::SeqCst) <= 2,
        "pulled {} chunks past the cap",
        polled.load(Ordering::SeqCst)
    );
    // Same cap on the backup route.
    let reply = fx
        .call(
            "POST",
            &backup_uri("app"),
            &[("authorization", bearer.as_str())],
            counting_body(vec![vec![b' '; 2048]], Arc::new(AtomicUsize::new(0))),
        )
        .await;
    assert_eq!(
        reply.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        reply.text
    );
    // A malformed content-length is a 400, not a panic.
    let reply = fx
        .call(
            "PUT",
            &put_uri("app"),
            &[
                ("authorization", bearer.as_str()),
                ("content-length", "abc"),
            ],
            Body::from(quota_body().to_string()),
        )
        .await;
    assert!(
        matches!(reply.status, StatusCode::BAD_REQUEST),
        "{}",
        reply.text
    );
    assert_eq!(fx.fake.total(), 0);
    // Boundary control: EXACTLY 1024 bytes of valid JSON is accepted.
    let exact = format!(
        "{}{}",
        quota_body(),
        " ".repeat(1024 - quota_body().to_string().len())
    );
    assert_eq!(exact.len(), 1024);
    let ok = fx
        .call(
            "PUT",
            &put_uri("app"),
            &[("authorization", bearer.as_str())],
            Body::from(exact),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    fx.finish().await;
}

#[tokio::test]
async fn backup_body_is_empty_or_empty_object_only() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let bearer = format!("Bearer {ROOT_KEY}");
    let put = fx.root("PUT", &put_uri("app"), quota_body()).await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.text);
    let before = fx.fake.count("GET", "/dump");
    for bad in [
        r#"{"url":"http://evil.example"}"#,
        r#"{"namespace":"x"}"#,
        "[]",
        "null",
        "0",
        "garbage",
    ] {
        let reply = fx
            .call(
                "POST",
                &backup_uri("app"),
                &[("authorization", bearer.as_str())],
                Body::from(bad),
            )
            .await;
        assert_static_error(&reply, StatusCode::BAD_REQUEST, "INVALID_SQLD_REQUEST");
    }
    assert_eq!(
        fx.fake.count("GET", "/dump"),
        before,
        "bad body reached the dump"
    );
    for good in ["", "{}", " { } "] {
        let reply = fx
            .call(
                "POST",
                &backup_uri("app"),
                &[("authorization", bearer.as_str())],
                Body::from(good),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{good:?}: {}", reply.text);
    }
    fx.finish().await;
}

// ───────────────────────── flow ─────────────────────────

fn assert_backup_meta(meta: &Value) {
    let object = meta.as_object().expect("backup object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["bytes", "fileName", "retention", "sha256"], "{meta}");
    let file = meta["fileName"].as_str().unwrap();
    assert!(
        !file.contains('/') && !file.contains('\\') && file.ends_with(".sql"),
        "{file}"
    );
    assert!(meta["bytes"].as_u64().unwrap() > 0);
    let sha = meta["sha256"].as_str().unwrap();
    assert!(
        sha.len() == 64
            && sha
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert!(matches!(
        meta["retention"].as_str(),
        Some("complete" | "incomplete")
    ));
}

#[tokio::test]
async fn root_put_get_backup_flow_runs_the_real_service() {
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let before = fx.state.index.worker_refs();

    // GET before: no binding.
    let get = fx.root("GET", &put_uri("app"), Value::Null).await;
    assert_eq!(get.status, StatusCode::OK);
    assert_eq!(
        get.json,
        json!({"database": null, "activation": "nextSpawn"})
    );
    assert_eq!(fx.fake.total(), 0, "GET must not call SQLD");

    // PUT: real ensure (probe, CREATE, proof GET, quota proof, dump) -> ready.
    let put = fx.root("PUT", &put_uri("app"), quota_body()).await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.text);
    assert_eq!(put.json["activation"], "nextSpawn");
    assert_eq!(put.json["database"]["state"], "ready");
    assert_eq!(put.json["database"]["quotaBytes"], QUOTA);
    let namespace = put.json["database"]["sqlNamespace"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!namespace.is_empty());
    assert_eq!(
        put.json["database"].as_object().unwrap().len(),
        3,
        "{}",
        put.text
    );
    assert_backup_meta(&put.json["initialBackup"]);
    assert_eq!(put.json.as_object().unwrap().len(), 3);
    assert_eq!(fx.fake.count("POST", "/create"), 1);
    assert_eq!(fx.fake.count("GET", "/dump"), 1);
    assert_eq!(fx.owner_record("app", None), Some(SqldRegistryState::Ready));
    // The service (not the API) stored the token in the vault; it is never echoed.
    let token = fx
        .vault
        .get("app", SQLD_TOKEN_SECRET)
        .expect("token stored");
    for leaked in [token.as_str(), ADMIN_KEY, "jwt", "BEGIN TRANSACTION"] {
        assert!(!put.text.contains(leaked), "{leaked} leaked: {}", put.text);
    }
    let dir_text = fx.dir.path().to_string_lossy().into_owned();
    assert!(!put.text.contains(&dir_text), "absolute path leaked");

    // GET after: ready, same metadata, still no remote call.
    let calls = fx.fake.total();
    let get = fx.root("GET", &put_uri("app"), Value::Null).await;
    assert_eq!(get.status, StatusCode::OK);
    assert_eq!(get.json["database"], put.json["database"]);
    assert_eq!(get.json.as_object().unwrap().len(), 2);
    assert_eq!(fx.fake.total(), calls);

    // POST backup: second artifact through the real service.
    let backup = fx.root("POST", &backup_uri("app"), Value::Null).await;
    assert_eq!(backup.status, StatusCode::OK, "{}", backup.text);
    assert_eq!(backup.json.as_object().unwrap().len(), 1);
    assert_backup_meta(&backup.json["backup"]);
    assert_eq!(fx.fake.count("GET", "/dump"), 2);
    assert!(!backup.text.contains(&dir_text));
    assert_eq!(fx.backup_files().len(), 2);

    // Idempotent PUT: 200, same binding, no new CREATE nor initial backup.
    let again = fx.root("PUT", &put_uri("app"), quota_body()).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.json["database"], put.json["database"]);
    assert_eq!(again.json["initialBackup"], Value::Null);
    assert_eq!(fx.fake.count("POST", "/create"), 1);
    assert_eq!(fx.fake.count("GET", "/dump"), 2);

    // Different quota: 409, nothing changes.
    let other = fx
        .root("PUT", &put_uri("app"), json!({"quotaBytes": QUOTA * 2}))
        .await;
    assert_static_error(&other, StatusCode::CONFLICT, "SQLD_CONFLICT");
    assert_eq!(fx.fake.count("POST", "/create"), 1);

    // The API never touched the installed references (no rewrite, no recycle).
    assert_eq!(fx.state.index.worker_refs(), before);
    fx.finish().await;
}

// ───────────────────────── error mapping ─────────────────────────

#[tokio::test]
async fn service_errors_map_to_static_statuses() {
    // Backup of an owner without a binding: 404.
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    let reply = fx.root("POST", &backup_uri("app"), Value::Null).await;
    assert_static_error(&reply, StatusCode::NOT_FOUND, "NOT_FOUND");
    fx.finish().await;

    // Pre-existing manual SQLD_TOKEN: 409 without overwrite or any remote CREATE.
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    fx.vault
        .set("app", SQLD_TOKEN_SECRET, MANUAL_TOKEN)
        .unwrap();
    let reply = fx.root("PUT", &put_uri("app"), quota_body()).await;
    assert_static_error(&reply, StatusCode::CONFLICT, "SQLD_CONFLICT");
    assert_eq!(fx.fake.count("POST", "/create"), 0);
    assert_eq!(
        fx.vault.get("app", SQLD_TOKEN_SECRET).map(|v| (*v).clone()),
        Some(MANUAL_TOKEN.to_owned())
    );
    fx.finish().await;

    // Backend failure (CREATE 500, namespace never visible): 503; a following
    // backup of the Intent owner is 409 (not ready); retry after recovery works.
    let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0"))]).await;
    fx.fake.with(|s| s.create_status = Some(500));
    let reply = fx.root("PUT", &put_uri("app"), quota_body()).await;
    assert_static_error(&reply, StatusCode::SERVICE_UNAVAILABLE, "SQLD_UNAVAILABLE");
    assert_eq!(
        fx.owner_record("app", None),
        Some(SqldRegistryState::Intent)
    );
    let reply = fx.root("POST", &backup_uri("app"), Value::Null).await;
    assert_static_error(&reply, StatusCode::CONFLICT, "SQLD_CONFLICT");
    let get = fx.root("GET", &put_uri("app"), Value::Null).await;
    assert_eq!(get.json["database"]["state"], "intent", "{}", get.text);
    fx.fake.with(|s| s.create_status = None);
    let reply = fx.root("PUT", &put_uri("app"), quota_body()).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json["database"]["state"], "ready");
    fx.finish().await;
}

// ───────────────────────── compatibility of all versions ─────────────────────────

fn with_env(mut manifest: WorkerManifest, key: &str, value: &str) -> WorkerManifest {
    manifest
        .env
        .get_or_insert_with(Default::default)
        .insert(key.into(), value.into());
    manifest
}

#[tokio::test]
async fn incompatible_non_default_versions_block_put_before_any_remote_or_reserve() {
    let wrong_url = |v: &str| with_env(deno("app", v), "SQLD_URL", "http://other.example/");
    let cases: Vec<(&str, Install)> = vec![
        (
            "disabled, other URL",
            Install::user(WorkerManifest {
                enabled: Some(false),
                ..wrong_url("0.9.0")
            }),
        ),
        (
            "staged public, other URL",
            Install {
                manifest: WorkerManifest {
                    visibility: WorkerVisibility::Public,
                    ..wrong_url("3.0.0")
                },
                origin: WorkerOrigin::User,
                staged: true,
            },
        ),
        (
            "lower version, SQLD_TOKEN in env",
            Install::user(with_env(deno("app", "0.1.0"), "SQLD_TOKEN", "x")),
        ),
        (
            "lower version, SQLD_TOKEN in publicEnv",
            Install::user(WorkerManifest {
                public_env: vec!["SQLD_TOKEN".into()],
                ..deno("app", "0.2.0")
            }),
        ),
        (
            "lower version, SQLD_TOKEN padded with ASCII whitespace in publicEnv",
            Install::user(WorkerManifest {
                public_env: vec![" SQLD_TOKEN\t".into()],
                ..deno("app", "0.21.0")
            }),
        ),
        (
            "lower version, SQLD_TOKEN padded with Unicode whitespace in publicEnv",
            Install::user(WorkerManifest {
                public_env: vec!["\u{00A0}\u{2003}SQLD_TOKEN\u{3000}\u{2028}".into()],
                ..deno("app", "0.22.0")
            }),
        ),
        (
            "lower version, SQLD_URL secret",
            Install::user(WorkerManifest {
                secrets: Some(vec!["SQLD_URL".into()]),
                ..deno("app", "0.3.0")
            }),
        ),
        (
            "lower version, Deno without allowNet",
            Install::user(WorkerManifest {
                allow_net: None,
                ..deno("app", "0.4.0")
            }),
        ),
        (
            "lower version, Deno with non-empty allowNet",
            Install::user(WorkerManifest {
                allow_net: Some(vec!["example.com".into()]),
                ..deno("app", "0.5.0")
            }),
        ),
    ];
    for (label, bad) in cases {
        let fx = Fixture::new(vec![Install::user(deno("app", "1.0.0")), bad]).await;
        let before = fx.state.index.worker_refs();
        assert_eq!(before.len(), 2, "{label}");
        let reply = fx.root("PUT", &put_uri("app"), quota_body()).await;
        assert_static_error(&reply, StatusCode::CONFLICT, "SQLD_CONFLICT");
        assert_eq!(fx.fake.total(), 0, "{label}: remote call");
        assert_eq!(fx.owner_record("app", None), None, "{label}: reserved");
        assert!(fx.vault.get("app", SQLD_TOKEN_SECRET).is_none(), "{label}");
        assert_eq!(fx.state.index.worker_refs(), before, "{label}: rewritten");
        // GET still reads the binding and the diagnostics path stays open.
        let get = fx.root("GET", &put_uri("app"), Value::Null).await;
        assert_eq!(
            get.json,
            json!({"database": null, "activation": "nextSpawn"}),
            "{label}"
        );
        // Backup is not gated by the config check (it needs a Ready owner): 404.
        let backup = fx.root("POST", &backup_uri("app"), Value::Null).await;
        assert_eq!(backup.status, StatusCode::NOT_FOUND, "{label}");
        fx.finish().await;
    }
}

#[tokio::test]
async fn compatible_versions_of_every_runtime_mode_allow_put() {
    let fx = Fixture::new_with(|origin| {
        let plain = |version: &str, kind: &str| WorkerManifest {
            name: "app".into(),
            version: Some(version.into()),
            kind: Some(kind.into()),
            ..Default::default()
        };
        vec![
            Install::user(deno("app", "1.0.0")),
            // Deno with the exact host origin already declared: kept as is.
            Install::user(with_env(deno("app", "0.6.0"), "SQLD_URL", origin)),
            // WASM and SPA: no allowNet requirement.
            Install::user(plain("0.8.0", "wasm")),
            Install::user(plain("0.7.0", "spa")),
        ]
    })
    .await;
    assert_eq!(fx.state.index.worker_refs().len(), 4);
    let reply = fx.root("PUT", &put_uri("app"), quota_body()).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json["database"]["state"], "ready");
    fx.finish().await;
}

// ───────────────────────── pure helper ─────────────────────────

const HOST: &str = "http://127.0.0.1:8080/";

fn worker_ref(kind: ExecutionKind, edit: impl FnOnce(&mut WorkerManifest)) -> WorkerRef {
    let mut manifest = deno("app", "1.0.0");
    edit(&mut manifest);
    let mut worker = create_worker_ref(PathBuf::from("/w/app"), manifest).expect("worker ref");
    worker.kind = kind;
    worker
}

fn deno_ref(edit: impl FnOnce(&mut WorkerManifest)) -> WorkerRef {
    worker_ref(ExecutionKind::FetchHandler, edit)
}

#[test]
fn pure_apply_sets_url_and_token_name_on_a_clone_only() {
    let worker = deno_ref(|m| {
        m.secrets = Some(vec!["OTHER".into()]);
        m.public_env = vec!["PUBLIC".into()];
        m.env = Some(HashMap::from([("KEEP".to_owned(), "1".to_owned())]));
    });
    let snapshot = worker.clone();
    let out = apply_managed_database_config(&worker, HOST).expect("applies");
    assert_eq!(worker, snapshot, "input mutated");
    assert_eq!(out.env.get(SQLD_URL_ENV).map(String::as_str), Some(HOST));
    assert_eq!(out.env.get("KEEP").map(String::as_str), Some("1"));
    assert_eq!(out.secrets, ["OTHER", SQLD_TOKEN_SECRET]);
    assert!(!out.env.contains_key(SQLD_TOKEN_SECRET));
    assert_eq!(out.public_env, ["PUBLIC"]);
    assert_eq!(out.allow_net, Some(vec![]), "no network is granted");
    // Everything else is byte-for-byte the original.
    let mut restored = out.clone();
    restored.env = worker.config.env.clone();
    restored.secrets = worker.config.secrets.clone();
    assert_eq!(restored, worker.config);
}

#[test]
fn pure_apply_url_cases() {
    let host_variants = [("absent", None), ("empty", Some(""))];
    for (label, value) in host_variants {
        let worker = deno_ref(|m| {
            if let Some(v) = value {
                m.env = Some(HashMap::from([("SQLD_URL".to_owned(), v.to_owned())]));
            }
        });
        let out = apply_managed_database_config(&worker, HOST).expect(label);
        assert_eq!(
            out.env.get("SQLD_URL").map(String::as_str),
            Some(HOST),
            "{label}"
        );
    }
    // Equal origin (any normalization-equivalent spelling): the ORIGINAL value is kept.
    for same in [
        "http://127.0.0.1:8080/",
        "http://127.0.0.1:8080",
        "HTTP://127.0.0.1:8080/",
    ] {
        let worker = deno_ref(|m| {
            m.env = Some(HashMap::from([("SQLD_URL".to_owned(), same.to_owned())]));
        });
        let out = apply_managed_database_config(&worker, HOST).expect(same);
        assert_eq!(out.env.get("SQLD_URL").map(String::as_str), Some(same));
    }
    // Different / invalid / hostile: static conflict, never overwritten.
    for bad in [
        "http://127.0.0.1:8081/",
        "https://127.0.0.1:8080/",
        "http://other.example:8080/",
        "http://user:pw@127.0.0.1:8080/",
        "http://user@127.0.0.1:8080/",
        "http://127.0.0.1:8080/path",
        "http://127.0.0.1:8080/?q=1",
        "http://127.0.0.1:8080/#frag",
        "http://127.0.0.1:8080/?",
        "http://127.0.0.1:8080/#",
        "http://127.0.0.1:8080//",
        "http://127.0.0.1:8080/ ",
        " http://127.0.0.1:8080/",
        "http://127.0.0.1:8080/\n",
        "http://127.0.0.1:8080\t",
        "not a url",
        "127.0.0.1:8080",
        "ftp://127.0.0.1:8080/",
        "file:///etc/passwd",
        "http://127.0.0.1.evil.example:8080/",
        "http://127.0.0.1:8080@evil.example/",
        "http://",
        "   ",
    ] {
        let worker = deno_ref(|m| {
            m.env = Some(HashMap::from([("SQLD_URL".to_owned(), bad.to_owned())]));
        });
        assert_eq!(
            apply_managed_database_config(&worker, HOST),
            Err(ManagedDatabaseConfigError::UrlConflict),
            "{bad:?}"
        );
    }
}

#[test]
fn pure_apply_invalid_host_origin_is_refused() {
    let worker = deno_ref(|_| {});
    for bad in [
        "",
        "http://u:p@127.0.0.1:8080/",
        "http://127.0.0.1:8080/x",
        "http://127.0.0.1:8080/?q",
        "http://127.0.0.1:8080/#f",
        "garbage",
        "ftp://h/",
    ] {
        assert_eq!(
            apply_managed_database_config(&worker, bad),
            Err(ManagedDatabaseConfigError::InvalidHostOrigin),
            "{bad:?}"
        );
    }
}

#[test]
fn pure_apply_token_and_url_secret_conflicts() {
    let env_token = deno_ref(|m| {
        m.env = Some(HashMap::from([("SQLD_TOKEN".to_owned(), String::new())]));
    });
    assert_eq!(
        apply_managed_database_config(&env_token, HOST),
        Err(ManagedDatabaseConfigError::TokenEnvConflict)
    );
    let public_token = deno_ref(|m| m.public_env = vec!["A".into(), "SQLD_TOKEN".into()]);
    assert_eq!(
        apply_managed_database_config(&public_token, HOST),
        Err(ManagedDatabaseConfigError::TokenPublicEnvConflict)
    );
    // Aliases padded with whitespace that `str::trim` removes (the contract of
    // the publicEnv consumer) are the same name.
    for alias in [
        " SQLD_TOKEN",
        "SQLD_TOKEN\t",
        " \r\n SQLD_TOKEN \t",
        "\u{00A0}SQLD_TOKEN",
        "SQLD_TOKEN\u{2003}",
        "\u{3000}\u{2028}SQLD_TOKEN\u{2029}",
    ] {
        assert_eq!(alias.trim(), "SQLD_TOKEN", "fixture {alias:?}");
        let mut padded = deno_ref(|_| {});
        padded.config.public_env = vec!["A".into(), alias.into()];
        let before = padded.clone();
        assert_eq!(
            apply_managed_database_config(&padded, HOST),
            Err(ManagedDatabaseConfigError::TokenPublicEnvConflict),
            "{alias:?}"
        );
        assert_eq!(padded, before, "{alias:?}: input mutated");
    }
    // Control: no case folding or other normalization is invented.
    let mut near = deno_ref(|_| {});
    near.config.public_env = vec![
        "sqld_token".into(),
        " SQLD_TOKEN_X ".into(),
        "SQLD TOKEN".into(),
    ];
    assert!(apply_managed_database_config(&near, HOST).is_ok());
    let url_secret = deno_ref(|m| m.secrets = Some(vec!["SQLD_URL".into()]));
    assert_eq!(
        apply_managed_database_config(&url_secret, HOST),
        Err(ManagedDatabaseConfigError::UrlSecretConflict)
    );
    // SQLD_URL secret conflicts even when the env value equals the host origin
    // (the vault would override it later).
    // (a manifest cannot declare both; the config of a ref can, so build it directly)
    let mut both = deno_ref(|_| {});
    both.config.secrets = vec!["SQLD_URL".into()];
    both.config.env.insert("SQLD_URL".into(), HOST.into());
    assert_eq!(
        apply_managed_database_config(&both, HOST),
        Err(ManagedDatabaseConfigError::UrlSecretConflict)
    );
}

#[test]
fn pure_apply_dedupes_the_token_secret_name() {
    let worker = deno_ref(|m| {
        m.secrets = Some(vec!["SQLD_TOKEN".into(), "B".into()]);
    });
    let out = apply_managed_database_config(&worker, HOST).unwrap();
    assert_eq!(out.secrets, ["SQLD_TOKEN", "B"]);
    // Applying twice (re-entrancy on the result) is stable.
    let mut again = worker.clone();
    again.config = out.clone();
    assert_eq!(apply_managed_database_config(&again, HOST).unwrap(), out);
}

#[test]
fn pure_apply_allow_net_rules_per_runtime_mode() {
    for (label, allow) in [
        ("none", None),
        ("non-empty", Some(vec!["example.com".to_owned()])),
        ("wildcard", Some(vec!["*".to_owned()])),
    ] {
        for kind in [
            ExecutionKind::FetchHandler,
            ExecutionKind::RoutesTable,
            ExecutionKind::Fullstack {
                adapter: "hono".into(),
            },
        ] {
            let worker = worker_ref(kind.clone(), |m| m.allow_net = allow.clone());
            assert_eq!(
                apply_managed_database_config(&worker, HOST),
                Err(ManagedDatabaseConfigError::DenoAllowNetRequired),
                "{label} {kind:?}"
            );
        }
        // WASM, fullstack WASM and SPA: no allowNet requirement, no network granted.
        for kind in [
            ExecutionKind::WasmModule { entry: None },
            ExecutionKind::Fullstack {
                adapter: "wasm".into(),
            },
            ExecutionKind::StaticSpa { inject_base: true },
        ] {
            let worker = worker_ref(kind.clone(), |m| m.allow_net = allow.clone());
            let out = apply_managed_database_config(&worker, HOST)
                .unwrap_or_else(|e| panic!("{label} {kind:?}: {e}"));
            assert_eq!(out.allow_net, allow, "{label} {kind:?}: network changed");
            assert!(out.secrets.iter().any(|s| s == SQLD_TOKEN_SECRET));
        }
    }
    // Control: Deno with an explicit empty allowNet passes.
    for kind in [
        ExecutionKind::FetchHandler,
        ExecutionKind::RoutesTable,
        ExecutionKind::Fullstack {
            adapter: "nextjs".into(),
        },
    ] {
        let worker = worker_ref(kind, |m| m.allow_net = Some(vec![]));
        assert!(apply_managed_database_config(&worker, HOST).is_ok());
    }
}
