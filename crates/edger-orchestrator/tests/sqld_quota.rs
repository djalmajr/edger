#![cfg(unix)]
//! (EDG-54 / 14q) Managed quota and isolation against the REAL SQLD 0.24.32.
//!
//! This is preparation for the future provisioning service, not the service:
//! it proves, with the real local binary, that the per-namespace
//! `max_db_size` the production admin client sends is enforced by the server
//! (`PRAGMA max_page_count`), that the application credential (the JWT the
//! production registry signs, `p.rw.ns` + `p.ddl.ns` for ONE namespace)
//! cannot raise it nor reach another namespace, that ATTACH / VACUUM INTO do
//! not touch the filesystem, and that a dump taken with the same credential
//! restores into an isolated SQLite. It does NOT prove WAL/PVC/backup quota,
//! host RAM/disk release, or the quota of any production database, and the
//! dump check proves the format/permission only (it is not the atomic backup
//! of the future service).
//!
//! The real test is `#[ignore]` and NEVER skips: a missing `EDGER_SQLD_BIN`
//! or binary is a failure when run with `--ignored`. Run (WSL Ubuntu-26.04):
//!
//!     EDGER_SQLD_BIN=/root/.local/share/edger-go-wasm/runtimes/sqld-0.24.32/bin/sqld \
//!       timeout --kill-after=5 300 \
//!       cargo test -p edger-orchestrator --test sqld_quota -- --ignored --nocapture
//!
//! Hygiene: the child env is cleared, its logs live in the private tempdir
//! and are scanned for the admin key and every JWT, the HTTP clients use no
//! proxy, no redirects and bounded bodies/time, and the child is stopped
//! with an explicit kill + wait on success AND failure (the scenario runs in
//! its own task so an assertion panic is captured, the child is shut down,
//! and only then the panic is propagated).

use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use edger_orchestrator::sqld_admin::{
    SqldAdminClient, SqldCreateOutcome, SqldNamespaceConfig, SqldRemoteKey,
};
use edger_orchestrator::sqld_registry::{
    SqldProvisionRegistry, SqldRegistryError, SqldRegistryOwner, SqldRegistryState,
    MIN_QUOTA_BYTES, QUOTA_ALIGN_BYTES,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use tokio::process::Command;
use zeroize::Zeroizing;

/// Outer bound of the whole scenario.
const SCENARIO_DEADLINE: Duration = Duration::from_secs(240);
const STARTUP_DEADLINE: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
const TOKEN_TTL: u64 = 600;
/// Admin key of the disposable server (a fixture literal, never a real key).
const ADMIN_KEY: &str = "canary-admin-sqld-quota";
const PROBE_NS: &str = "edger_probe";
/// Control namespace served by the GLOBAL key only (no per-app `jwt_key`).
const CONTROL_NS: &str = "ctrlglobal";
const PUB_KEY_FILE: &str = "ed25519-global.pub";
const MASTER: [u8; 32] = [0x6b; 32];
const QUOTA: u64 = MIN_QUOTA_BYTES; // 16 MiB
const BUDGET: u64 = 2 * MIN_QUOTA_BYTES; // covers A and B
const PAGE_SIZE: u64 = 4096;
const EXPECT_PAGES: i64 = (QUOTA / PAGE_SIZE) as i64; // 4096
/// One attempted INSERT: 1 MiB (< the 5 MB `max_row_size` default).
const BLOB_BYTES: u64 = 1024 * 1024;
/// At most 32 attempts of 1 MiB = 32 MiB of attempted data.
const MAX_FILL_ATTEMPTS: u64 = 32;
/// Hard bound for the dump body (the 15 MiB of zeroblob dump as hex text).
const DUMP_CAP: usize = 96 * 1024 * 1024;
const LOG_READ_MAX: usize = 1 << 20;

// ───────────────────────── pure helpers (ordinary tests) ─────────────────────────

/// `x-namespace-bin`: base64 STANDARD alphabet WITHOUT padding.
fn no_pad(ns: &str) -> String {
    STANDARD_NO_PAD.encode(ns.as_bytes())
}

/// Outcome of the first result of a `/v2/pipeline` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Ok(Vec<Vec<Value>>),
    Err { code: String, message: String },
    Http(u16),
}

fn classify(status: u16, value: &Value) -> Step {
    if status != 200 {
        return Step::Http(status);
    }
    let first = value.pointer("/results/0");
    match first.and_then(|r| r.get("type")).and_then(Value::as_str) {
        Some("ok") => {
            let rows = value
                .pointer("/results/0/response/result/rows")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .map(|row| row.as_array().cloned().unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default();
            Step::Ok(rows)
        }
        Some("error") => Step::Err {
            code: first
                .and_then(|r| r.pointer("/error/code"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            message: first
                .and_then(|r| r.pointer("/error/message"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        },
        _ => Step::Http(status),
    }
}

fn is_denied(step: &Step) -> bool {
    !matches!(step, Step::Ok(_))
}

fn parse_denied(step: &Step) -> bool {
    matches!(step, Step::Err { code, .. } if code == "SQL_PARSE_ERROR")
}

fn int_cell(cell: &Value) -> Option<i64> {
    if cell.get("type").and_then(Value::as_str) != Some("integer") {
        return None;
    }
    cell.get("value")?.as_str()?.parse().ok()
}

/// The single integer of a one-row/one-column answer.
fn single_int(step: &Step) -> Option<i64> {
    match step {
        Step::Ok(rows) if rows.len() == 1 && rows[0].len() == 1 => int_cell(&rows[0][0]),
        _ => None,
    }
}

fn pipeline_body(request: Value) -> String {
    json!({ "requests": [request, { "type": "close" }] }).to_string()
}

fn execute_body(sql: &str) -> String {
    pipeline_body(json!({ "type": "execute", "stmt": { "sql": sql } }))
}

fn describe_body(sql: &str) -> String {
    pipeline_body(json!({ "type": "describe", "sql": sql }))
}

/// Appends `chunk` to `buf` only while the total stays within `cap`.
fn append_capped(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    if buf.len().saturating_add(chunk.len()) > cap {
        return false;
    }
    buf.extend_from_slice(chunk);
    true
}

fn has_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Reads at most `max` bytes (+1 to detect overflow) of a child log.
fn bounded_read(path: &Path, max: usize) -> Vec<u8> {
    let file = std::fs::File::open(path).expect("the child log opens");
    let mut out = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut out)
        .expect("the child log reads");
    assert!(out.len() <= max, "the child log exceeds the read bound");
    out
}

fn jwt_payload(token: &str) -> Value {
    let payload = token.split('.').nth(1).expect("a JWT has a payload");
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .expect("payload is base64url");
    serde_json::from_slice(&bytes).expect("payload is JSON")
}

fn assert_scoped_claims(claims: &Value, ns: &str, now: i64) {
    let exp = claims["exp"].as_i64().expect("finite integer exp");
    assert!(exp > now && exp <= now + TOKEN_TTL as i64 + 5, "finite exp");
    let obj = claims.as_object().expect("claims object");
    assert_eq!(obj.len(), 2, "only exp + p");
    let p = claims["p"].as_object().expect("p object");
    assert_eq!(p.len(), 2, "p has exactly rw + ddl");
    for kind in ["rw", "ddl"] {
        assert_eq!(
            p[kind],
            json!({ "ns": [ns] }),
            "p.{kind}.ns is exactly the namespace"
        );
    }
}

fn open_registry(dir: &Path) -> SqldProvisionRegistry {
    let private = dir.join("ledger");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&private)
        .expect("the private ledger dir is created 0700");
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700))
        .expect("the ledger dir is 0700 regardless of umask");
    SqldProvisionRegistry::open(&private.join("ledger.db"), Zeroizing::new(MASTER), BUDGET)
        .expect("the production ledger opens with a budget covering A and B")
}

fn owner_a() -> SqldRegistryOwner<'static> {
    SqldRegistryOwner {
        worker: "quota-app-a",
        rbac_namespace: Some("nsq"),
    }
}

fn owner_b() -> SqldRegistryOwner<'static> {
    SqldRegistryOwner {
        worker: "quota-app-b",
        rbac_namespace: Some("nsq"),
    }
}

#[test]
fn header_codec_is_standard_no_pad_and_not_url_safe() {
    // These inputs use the alphabet positions where the two codecs differ.
    let tricky = ">>>???~~~";
    assert_eq!(no_pad(tricky), "Pj4+Pz8/fn5+");
    assert_ne!(no_pad(tricky), URL_SAFE_NO_PAD.encode(tricky));
    // A real SQL namespace (70 bytes, 70 % 3 == 1) would pad with `==`.
    let ns = format!("edger_{}", "ab".repeat(32));
    assert_eq!(ns.len(), 70);
    assert!(!no_pad(&ns).contains('='));
    assert!(STANDARD.encode(ns.as_bytes()).ends_with("=="));
    // For edger_<hex> names (only [0-9a-f_]) the 62/63 positions never occur,
    // so STANDARD and URL_SAFE coincide there: this fixture cannot tell them
    // apart live, the unit above pins the alphabet instead.
    assert_eq!(no_pad(&ns), URL_SAFE_NO_PAD.encode(ns.as_bytes()));
}

#[test]
fn classify_separates_ok_protocol_error_and_http_denials() {
    let ok = json!({"results":[{"type":"ok","response":{"type":"execute","result":{
        "rows":[[{"type":"integer","value":"4096"}]]}}},{"type":"ok","response":{"type":"close"}}]});
    assert_eq!(single_int(&classify(200, &ok)), Some(4096));
    let full = json!({"results":[{"type":"error","error":{"message":"database or disk is full","code":"SQLITE_FULL"}}]});
    assert_eq!(
        classify(200, &full),
        Step::Err {
            code: "SQLITE_FULL".into(),
            message: "database or disk is full".into()
        }
    );
    assert!(is_denied(&classify(200, &full)));
    assert_eq!(classify(401, &Value::Null), Step::Http(401));
    assert!(is_denied(&classify(404, &Value::Null)));
    // A wrongly typed integer is not a number.
    assert_eq!(int_cell(&json!({"type":"text","value":"4096"})), None);
}

#[test]
fn bounded_helpers_refuse_overflow_and_find_secrets() {
    let mut buf = Vec::new();
    assert!(append_capped(&mut buf, b"abcd", 6));
    assert!(!append_capped(&mut buf, b"efg", 6), "7 > 6 is refused");
    assert_eq!(buf, b"abcd", "a refused chunk is not appended");
    assert!(append_capped(&mut buf, b"ef", 6));
    assert!(has_bytes(b"xx canary-admin yy", b"canary-admin"));
    assert!(!has_bytes(b"xx", b""));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    std::fs::write(&path, vec![b'a'; 16]).unwrap();
    assert_eq!(bounded_read(&path, 16).len(), 16);
    let over = std::panic::catch_unwind(|| bounded_read(&path, 15));
    assert!(over.is_err(), "a log over the bound is refused");
}

#[test]
fn production_registry_ledger_matches_the_quota_the_fixture_sends() {
    let dir = tempfile::tempdir().unwrap();
    let registry = open_registry(dir.path());
    let a = registry.ensure_intent(&owner_a(), QUOTA).unwrap();
    let b = registry.ensure_intent(&owner_b(), QUOTA).unwrap();
    assert_eq!(QUOTA % QUOTA_ALIGN_BYTES, 0);
    assert_eq!(EXPECT_PAGES, 4096, "16 MiB / 4096 B pages");
    for r in [&a, &b] {
        assert_eq!(r.quota_bytes, QUOTA);
        assert_eq!(r.state, SqldRegistryState::Intent);
        let hex = r
            .sql_namespace
            .strip_prefix("edger_")
            .expect("edger_ prefix");
        assert_eq!(hex.len(), 64);
        assert!(hex
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
    }
    assert_ne!(a.sql_namespace, b.sql_namespace);
    assert_ne!(a.public_key, b.public_key, "one key per app");
    // Budget invariant: a third 16 MiB reservation exceeds the 32 MiB budget.
    let third = SqldRegistryOwner {
        worker: "quota-app-c",
        rbac_namespace: Some("nsq"),
    };
    assert_eq!(
        registry.ensure_intent(&third, QUOTA).unwrap_err(),
        SqldRegistryError::BudgetExceeded
    );
    // Control: the idempotent re-reservation of A still works.
    assert_eq!(registry.ensure_intent(&owner_a(), QUOTA).unwrap(), a);
    // No token before the remote key is proven; scoped claims afterwards.
    assert_eq!(
        registry
            .sign_scoped_token(&owner_a(), 1_000, TOKEN_TTL)
            .unwrap_err(),
        SqldRegistryError::NotProven
    );
    registry.mark_proven(&owner_a(), a.public_key).unwrap();
    let token = registry
        .sign_scoped_token(&owner_a(), 1_000, TOKEN_TTL)
        .unwrap();
    let claims = jwt_payload(token.expose_jwt());
    assert_eq!(claims["exp"], json!(1_000 + TOKEN_TTL));
    assert_eq!(claims["p"]["rw"]["ns"], json!([a.sql_namespace]));
    assert_eq!(claims["p"]["ddl"]["ns"], json!([a.sql_namespace]));
}

// ───────────────────────── real server fixture ─────────────────────────

struct GlobalKeys {
    pub_pem: String,
    priv_pem: Zeroizing<String>,
}

fn generate_global_keys() -> GlobalKeys {
    let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)
        .expect("ring generates an Ed25519 key pair offline");
    GlobalKeys {
        pub_pem: pair.public_key_pem(),
        priv_pem: Zeroizing::new(pair.serialize_pem()),
    }
}

/// A token signed by the GLOBAL fixture key (same claims shape).
fn sign_global_token(keys: &GlobalKeys, ns: &str, now: i64) -> String {
    let claims = json!({
        "exp": now + TOKEN_TTL as i64,
        "p": { "rw": { "ns": [ns] }, "ddl": { "ns": [ns] } }
    });
    encode(
        &Header::new(Algorithm::EdDSA),
        &claims,
        &EncodingKey::from_ed_pem(keys.priv_pem.as_bytes()).expect("valid Ed25519 PEM"),
    )
    .expect("EdDSA signing succeeds offline")
}

/// Two DISTINCT ephemeral loopback ports for one process. Both probe
/// listeners stay open until both ports are known and only then are released:
/// two calls to a one-port helper could get the same port back from the
/// kernel, because the first probe is already closed when the second binds.
async fn ephemeral_port_pair() -> (u16, u16) {
    let first = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds the first loopback port probe");
    let second = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds the second loopback port probe");
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
        .expect("the probe client builds")
}

/// The REAL sqld child. The pid is retained; every path ends in `shutdown`
/// (kill + wait), `kill_on_drop` is only the last-resort fallback.
struct Sqld {
    child: Option<tokio::process::Child>,
    pid: u32,
    user_port: u16,
    admin_port: u16,
}

impl Sqld {
    fn spawn(bin: &str, dir: &Path, pub_pem: &str, user_port: u16, admin_port: u16) -> Self {
        let pub_path = dir.join(PUB_KEY_FILE);
        std::fs::write(&pub_path, pub_pem.as_bytes()).expect("writes the global public key");
        let stdout = std::fs::File::create(dir.join("sqld-stdout.log")).expect("stdout log");
        let stderr = std::fs::File::create(dir.join("sqld-stderr.log")).expect("stderr log");
        let child = Command::new(bin)
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
        let pid = child.id().expect("a freshly spawned child has a pid");
        Self {
            child: Some(child),
            pid,
            user_port,
            admin_port,
        }
    }

    /// Waits for the admin gate (404 for an unknown namespace under the right
    /// key), checking the child exit every round. Returns `Err` (never panics)
    /// so the caller can still run `shutdown`.
    async fn wait_ready(&mut self, client: &reqwest::Client) -> Result<(), String> {
        let url = format!(
            "http://127.0.0.1:{}/v1/namespaces/{PROBE_NS}/config",
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

    /// Explicit kill + wait; asserts the owned pid no longer exists.
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

/// Bounded body read (aborts at `cap`).
async fn read_capped(mut resp: reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("body: {e}"))? {
        if !append_capped(&mut buf, &chunk, cap) {
            return Err(format!("body exceeds the {cap} byte cap"));
        }
    }
    Ok(buf)
}

struct Ctx {
    user_base: String,
    admin_base: String,
    dir: PathBuf,
    client: reqwest::Client,
    global: GlobalKeys,
}

impl Ctx {
    async fn json_req(&self, req: reqwest::RequestBuilder) -> (u16, Value) {
        let resp = req.send().await.expect("the request completes");
        let status = resp.status().as_u16();
        let bytes = read_capped(resp, 256 * 1024).await.expect("bounded body");
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    async fn admin(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<&str>,
    ) -> (u16, Value) {
        let url = format!("{}{path}", self.admin_base);
        let mut req = match method {
            "GET" => self.client.get(url),
            _ => self.client.post(url),
        };
        if let Some(key) = key {
            req = req.header("authorization", format!("Basic {key}"));
        }
        if let Some(body) = body {
            req = req
                .header("content-type", "application/json")
                .body(body.to_owned());
        }
        self.json_req(req).await
    }

    /// One `/v2/pipeline` request; `ns_header` is the raw header value.
    async fn pipeline_raw(
        &self,
        ns_header: Option<String>,
        token: Option<&str>,
        body: String,
    ) -> (u16, Value) {
        let mut req = self
            .client
            .post(format!("{}/v2/pipeline", self.user_base))
            .header("content-type", "application/json")
            .body(body);
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        if let Some(ns) = ns_header {
            req = req.header("x-namespace-bin", ns);
        }
        self.json_req(req).await
    }

    async fn sql_as(&self, ns: &str, token: &str, sql: &str) -> Step {
        let (status, value) = self
            .pipeline_raw(Some(no_pad(ns)), Some(token), execute_body(sql))
            .await;
        classify(status, &value)
    }

    async fn describe_as(&self, ns: &str, token: &str, sql: &str) -> Step {
        let (status, value) = self
            .pipeline_raw(Some(no_pad(ns)), Some(token), describe_body(sql))
            .await;
        classify(status, &value)
    }

    async fn max_pages(&self, ns: &str, token: &str) -> Option<i64> {
        single_int(&self.sql_as(ns, token, "PRAGMA max_page_count").await)
    }

    async fn page_count(&self, ns: &str, token: &str) -> Option<i64> {
        single_int(
            &self
                .sql_as(ns, token, "SELECT * FROM pragma_page_count")
                .await,
        )
    }
}

fn expect_ok(step: Step, label: &str) -> Vec<Vec<Value>> {
    match step {
        Step::Ok(rows) => rows,
        other => panic!("{label}: expected ok, got {other:?}"),
    }
}

fn expect_full(step: &Step, label: &str) {
    match step {
        Step::Err { code, message } => {
            assert!(
                code == "SQLITE_FULL" || message.to_lowercase().contains("full"),
                "{label}: expected SQLITE_FULL, got code={code:?} message={message:?}"
            );
        }
        other => panic!("{label}: expected SQLITE_FULL protocol error, got {other:?}"),
    }
}

/// Fills `ns` with 1 MiB zeroblob rows until SQLITE_FULL (at most
/// `MAX_FILL_ATTEMPTS`); returns the number of rows that succeeded.
async fn fill_until_full(ctx: &Ctx, ns: &str, token: &str) -> u64 {
    let insert = format!("INSERT INTO t(b) VALUES (zeroblob({BLOB_BYTES}))");
    let mut ok = 0;
    for attempt in 1..=MAX_FILL_ATTEMPTS {
        match ctx.sql_as(ns, token, &insert).await {
            Step::Ok(_) => ok += 1,
            full => {
                expect_full(&full, "fill");
                eprintln!("quota-evidence: SQLITE_FULL at attempt {attempt}: {full:?}");
                return ok;
            }
        }
    }
    panic!("no SQLITE_FULL within {MAX_FILL_ATTEMPTS} MiB of attempted data");
}

async fn scenario(ctx: Ctx) {
    let now = chrono::Utc::now().timestamp();
    let dir = tempfile::tempdir_in(&ctx.dir).expect("scenario scratch");
    let registry = open_registry(dir.path());

    // Production registry + admin client.
    let rec_a = registry.ensure_intent(&owner_a(), QUOTA).expect("intent A");
    let rec_b = registry.ensure_intent(&owner_b(), QUOTA).expect("intent B");
    let (ns_a, ns_b) = (rec_a.sql_namespace.clone(), rec_b.sql_namespace.clone());
    let admin = SqldAdminClient::new(&ctx.admin_base, Zeroizing::new(ADMIN_KEY.to_owned()))
        .expect("production admin client");

    // 1. Admin auth is closed: anonymous and wrong key refused, right key ok.
    let cfg_path = format!("/v1/namespaces/{PROBE_NS}/config");
    assert_eq!(
        ctx.admin("GET", &cfg_path, None, None).await.0,
        401,
        "anonymous admin refused"
    );
    assert_eq!(
        ctx.admin("GET", &cfg_path, Some("canary-admin-wrong"), None)
            .await
            .0,
        401,
        "wrong admin key refused"
    );
    admin
        .probe_admin_auth()
        .await
        .expect("production probe_admin_auth");

    // Wrong key must not create (and the namespace must stay absent).
    let wrong_create = ctx
        .admin(
            "POST",
            &format!("/v1/namespaces/{ns_a}/create"),
            Some("canary-admin-wrong"),
            Some("{}"),
        )
        .await;
    assert_eq!(wrong_create.0, 401);
    assert_eq!(
        admin.read_config(&ns_a).await.expect("config"),
        SqldNamespaceConfig::Missing,
        "a refused create left no namespace"
    );

    for record in [&rec_a, &rec_b] {
        assert_eq!(
            admin.create_namespace(record).await.expect("create"),
            SqldCreateOutcome::Created
        );
    }
    // Fresh remote pubkey must equal the registry key; then mark_proven.
    for (owner, record) in [(owner_a(), &rec_a), (owner_b(), &rec_b)] {
        match admin
            .read_config(&record.sql_namespace)
            .await
            .expect("fresh config")
        {
            SqldNamespaceConfig::Present {
                key: SqldRemoteKey::Single(remote),
            } => {
                assert_eq!(
                    remote, record.public_key,
                    "remote pubkey == registry pubkey"
                );
            }
            other => panic!("unexpected remote config: {other:?}"),
        }
        let proven = registry
            .mark_proven(&owner, record.public_key)
            .expect("mark_proven with the observed key");
        assert_eq!(proven.state, SqldRegistryState::Proven);
        registry.mark_ready(&owner).expect("mark_ready");
    }
    let raw_cfg = ctx
        .admin(
            "GET",
            &format!("/v1/namespaces/{ns_a}/config"),
            Some(ADMIN_KEY),
            None,
        )
        .await;
    assert_eq!(raw_cfg.0, 200);
    assert_eq!(
        raw_cfg.1["allow_attach"],
        json!(false),
        "allow_attach false on the real server"
    );
    eprintln!(
        "quota-evidence: raw config max_db_size field = {}",
        raw_cfg.1["max_db_size"]
    );

    let tok_a = registry
        .sign_scoped_token(&owner_a(), now, TOKEN_TTL)
        .expect("token A");
    let tok_b = registry
        .sign_scoped_token(&owner_b(), now, TOKEN_TTL)
        .expect("token B");
    let (ta, tb) = (tok_a.expose_jwt(), tok_b.expose_jwt());
    assert_scoped_claims(&jwt_payload(ta), &ns_a, now);
    assert_scoped_claims(&jwt_payload(tb), &ns_b, now);

    // Control namespace for the GLOBAL key (no per-app jwt_key).
    let (st, _) = ctx
        .admin(
            "POST",
            &format!("/v1/namespaces/{CONTROL_NS}/create"),
            Some(ADMIN_KEY),
            Some("{}"),
        )
        .await;
    assert_eq!(st, 200, "control namespace created");
    let global_ctrl = sign_global_token(&ctx.global, CONTROL_NS, now);
    expect_ok(
        ctx.sql_as(CONTROL_NS, &global_ctrl, "SELECT 1").await,
        "global key works on its own namespace",
    );

    // PRAGMA max_page_count exact 4096 for both.
    assert_eq!(
        ctx.max_pages(&ns_a, ta).await,
        Some(EXPECT_PAGES),
        "A max_page_count"
    );
    assert_eq!(
        ctx.max_pages(&ns_b, tb).await,
        Some(EXPECT_PAGES),
        "B max_page_count"
    );

    // 2. Fill A until SQLITE_FULL; SELECT keeps working.
    expect_ok(
        ctx.sql_as(&ns_a, ta, "CREATE TABLE t(id INTEGER PRIMARY KEY, b BLOB)")
            .await,
        "create t",
    );
    let filled = fill_until_full(&ctx, &ns_a, ta).await;
    assert_eq!(filled, 15, "15 x 1 MiB fit in 4096 pages, the 16th is FULL");
    assert!(
        filled * BLOB_BYTES <= QUOTA,
        "data stored stays within the quota"
    );
    let count = single_int(&ctx.sql_as(&ns_a, ta, "SELECT count(*) FROM t").await);
    assert_eq!(count, Some(filled as i64), "SELECT works after FULL");
    let pages = ctx.page_count(&ns_a, ta).await.expect("page_count");
    assert!(
        pages <= EXPECT_PAGES && pages * 4096 > (filled * BLOB_BYTES) as i64,
        "pages={pages}"
    );
    eprintln!(
        "quota-evidence: A rows={filled} bytes={} pages={pages}/{EXPECT_PAGES}",
        filled * BLOB_BYTES
    );

    // 3. Attempts to raise the limit are refused; state unchanged.
    let raise = [
        "PRAGMA max_page_count=1000000",
        "PRAGMA main.max_page_count=1000000",
        "PRAGMA max_page_count(1000000)",
        "PRAGMA main.max_page_count(1000000)",
        "EXPLAIN PRAGMA max_page_count=1000000",
        "SELECT 1; PRAGMA max_page_count=1000000",
    ];
    for sql in raise {
        let step = ctx.sql_as(&ns_a, ta, sql).await;
        eprintln!("quota-evidence: execute {sql:?} -> {step:?}");
        assert!(is_denied(&step), "{sql}: must be denied, got {step:?}");
        // Observed on the pinned server: refused at parse time, before SQLite.
        let expected = if sql.starts_with("SELECT 1;") {
            matches!(&step, Step::Err { code, .. } if code == "SQL_MANY_STATEMENTS")
        } else {
            parse_denied(&step)
        };
        assert!(expected, "{sql}: unexpected refusal kind {step:?}");
        assert_eq!(
            ctx.max_pages(&ns_a, ta).await,
            Some(EXPECT_PAGES),
            "after {sql}"
        );
    }
    let described = ctx
        .describe_as(&ns_a, ta, "PRAGMA max_page_count=1000000")
        .await;
    eprintln!("quota-evidence: describe SET -> {described:?}");
    assert_eq!(
        ctx.max_pages(&ns_a, ta).await,
        Some(EXPECT_PAGES),
        "after describe"
    );
    // Table-valued pragma only queries.
    for sql in [
        "SELECT * FROM pragma_max_page_count",
        "SELECT * FROM pragma_max_page_count('main')",
        "SELECT * FROM pragma_max_page_count(1000000)",
    ] {
        let step = ctx.sql_as(&ns_a, ta, sql).await;
        eprintln!("quota-evidence: {sql:?} -> {step:?}");
        if let Step::Ok(_) = &step {
            assert_eq!(single_int(&step), Some(EXPECT_PAGES), "{sql} only reads");
        }
        assert_eq!(
            ctx.max_pages(&ns_a, ta).await,
            Some(EXPECT_PAGES),
            "after {sql}"
        );
    }
    let again = ctx
        .sql_as(
            &ns_a,
            ta,
            &format!("INSERT INTO t(b) VALUES (zeroblob({BLOB_BYTES}))"),
        )
        .await;
    expect_full(&again, "insert after raise attempts");
    assert_eq!(
        single_int(&ctx.sql_as(&ns_a, ta, "SELECT count(*) FROM t").await),
        Some(filled as i64)
    );

    // 4. Isolation between namespaces and keys.
    let dnied = |step: Step, label: &str| {
        eprintln!("quota-evidence: {label} -> {step:?}");
        assert!(is_denied(&step), "{label}: must be denied, got {step:?}");
    };
    let cross = |step: Step| assert_eq!(step, Step::Http(401), "cross-namespace/key token is 401");
    dnied(
        ctx.sql_as(&ns_b, ta, "SELECT count(*) FROM t").await,
        "token A on B",
    );
    cross(ctx.sql_as(&ns_b, ta, "SELECT count(*) FROM t").await);
    cross(ctx.sql_as(&ns_a, tb, "SELECT count(*) FROM t").await);
    let global_a = sign_global_token(&ctx.global, &ns_a, now);
    cross(ctx.sql_as(&ns_a, &global_a, "SELECT count(*) FROM t").await);
    // Header codec: padded form is refused, the NO_PAD one works.
    let padded = STANDARD.encode(ns_a.as_bytes());
    assert!(padded.ends_with("=="));
    let (st, v) = ctx
        .pipeline_raw(Some(padded), Some(ta), execute_body("SELECT 1"))
        .await;
    assert_eq!(
        classify(st, &v),
        Step::Http(400),
        "padded x-namespace-bin is refused"
    );
    let (st, v) = ctx
        .pipeline_raw(Some(no_pad(&ns_a)), Some(ta), execute_body("SELECT 1"))
        .await;
    expect_ok(classify(st, &v), "NO_PAD header control");
    // Control: B still writes/reads after A is full.
    expect_ok(
        ctx.sql_as(&ns_b, tb, "CREATE TABLE t(id INTEGER PRIMARY KEY, b BLOB)")
            .await,
        "B create",
    );
    expect_ok(
        ctx.sql_as(
            &ns_b,
            tb,
            &format!("INSERT INTO t(b) VALUES (zeroblob({BLOB_BYTES}))"),
        )
        .await,
        "B insert",
    );
    assert_eq!(
        single_int(&ctx.sql_as(&ns_b, tb, "SELECT count(*) FROM t").await),
        Some(1)
    );
    assert_eq!(ctx.max_pages(&ns_b, tb).await, Some(EXPECT_PAGES));

    // 5. ATTACH / VACUUM INTO leave no file behind.
    let targets = dir.path().join("targets");
    std::fs::create_dir(&targets).expect("exclusive target dir");
    {
        // Control: the directory is writable by this user (so an absent file
        // means the statement did not run, not that the dir was unwritable).
        let probe = targets.join("control.tmp");
        std::fs::write(&probe, b"x").expect("target dir is writable");
        std::fs::remove_file(&probe).expect("control removed");
    }
    let attach_file = targets.join("attach-target.db");
    let vacuum_file = targets.join("vacuum-target.db");
    let hostile = [
        format!("ATTACH DATABASE '{}' AS x", attach_file.display()),
        format!("ATTACH '{}' AS x", attach_file.display()),
        format!("ATTACH \"{ns_b}\" AS x"),
        format!("ATTACH DATABASE '{ns_b}' AS x"),
        format!("VACUUM INTO '{}'", vacuum_file.display()),
        format!("VACUUM main INTO '{}'", vacuum_file.display()),
        "VACUUM".to_owned(),
    ];
    for sql in &hostile {
        let step = ctx.sql_as(&ns_a, ta, sql).await;
        dnied(step.clone(), sql);
        if sql.starts_with("ATTACH \"") {
            // Observed: the registered-namespace form reaches the permission
            // check and is refused (403): rw has no AttachRead right.
            assert_eq!(step, Step::Http(403), "ATTACH <namespace> needs AttachRead");
        } else {
            assert!(
                parse_denied(&step),
                "{sql}: refused at parse time, got {step:?}"
            );
        }
    }
    assert!(!attach_file.exists(), "ATTACH created no file");
    assert!(!vacuum_file.exists(), "VACUUM INTO created no file");
    assert_eq!(
        std::fs::read_dir(&targets).unwrap().count(),
        0,
        "targets dir stays empty"
    );
    assert_eq!(
        ctx.max_pages(&ns_a, ta).await,
        Some(EXPECT_PAGES),
        "after ATTACH/VACUUM"
    );
    // B untouched by the cross-namespace ATTACH attempts.
    assert_eq!(
        single_int(&ctx.sql_as(&ns_b, tb, "SELECT count(*) FROM t").await),
        Some(1)
    );

    // 6. Dump of A with the rw token restores into an isolated SQLite.
    let dump_req = |ns: &str, token: Option<&str>| {
        let mut req = ctx
            .client
            .get(format!("{}/dump", ctx.user_base))
            .header("x-namespace-bin", no_pad(ns));
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        req
    };
    assert_eq!(
        dump_req(&ns_a, None)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        401,
        "anonymous dump refused"
    );
    let foreign = dump_req(&ns_a, Some(tb))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert!(foreign >= 400, "token B cannot dump A (status {foreign})");
    let resp = dump_req(&ns_a, Some(ta))
        .send()
        .await
        .expect("dump request");
    assert_eq!(resp.status().as_u16(), 200, "dump with token rw");
    let bytes = read_capped(resp, DUMP_CAP).await.expect("bounded dump");
    eprintln!("quota-evidence: dump bytes={}", bytes.len());
    let text = String::from_utf8(bytes).expect("dump is UTF-8 SQL text");
    let restore_path = dir.path().join("restored.db");
    let restored = rusqlite::Connection::open(&restore_path).expect("isolated sqlite");
    restored
        .pragma_update(None, "max_page_count", 2 * EXPECT_PAGES)
        .expect("bound the restore db");
    restored.execute_batch(&text).expect("the dump restores");
    let integrity: String = restored
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity_check");
    assert_eq!(integrity, "ok");
    let (rows, bytes_sum): (i64, i64) = restored
        .query_row("SELECT count(*), sum(length(b)) FROM t", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("restored data");
    assert_eq!(rows, filled as i64, "restored row count");
    assert_eq!(bytes_sum as u64, filled * BLOB_BYTES, "restored bytes");
    // The restore lives in an isolated file: it carries no sqld limit.
    drop(restored);
    eprintln!("quota-evidence: restore rows={rows} bytes={bytes_sum}");
}

/// Runs `fut` in its own task under `deadline`. The task handle is kept: on
/// expiry the task is aborted AND awaited (so its future and everything it
/// owns are dropped) before this returns. `Err(None)` = deadline expired;
/// `Err(Some(e))` = the task panicked or was cancelled.
/// An abort lands at an await point: the scenario must not block a thread.
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

struct DropFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn expired_scenario_is_aborted_and_joined_before_run_owned_returns() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let dropped = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let guard = DropFlag(dropped.clone());
    let out = run_owned(Duration::from_millis(50), async move {
        let _guard = guard;
        started_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    })
    .await;
    assert!(matches!(out, Err(None)), "deadline expiry is Err(None)");
    assert!(started_rx.await.is_ok(), "the task started before expiry");
    assert!(
        dropped.load(Ordering::SeqCst),
        "the task's guard must be gone when run_owned returns (no detached task)"
    );
}

#[tokio::test]
async fn run_owned_propagates_normal_result_and_panic() {
    assert!(matches!(
        run_owned(Duration::from_secs(5), async { 7u8 }).await,
        Ok(7)
    ));
    let out = run_owned(Duration::from_secs(5), async {
        panic!("scenario boom");
    })
    .await;
    match out {
        Err(Some(join)) if join.is_panic() => {
            let payload = join.into_panic();
            assert_eq!(payload.downcast_ref::<&str>(), Some(&"scenario boom"));
        }
        other => panic!("expected a propagated panic, got ok={}", other.is_ok()),
    }
}

#[tokio::test]
#[ignore = "requires the operator-approved real sqld 0.24.32 (EDGER_SQLD_BIN)"]
async fn real_sqld_enforces_managed_quota_and_isolation() {
    let bin = std::env::var("EDGER_SQLD_BIN").unwrap_or_else(|_| {
        panic!("EDGER_SQLD_BIN must name the real sqld binary (/root/.local/share/edger-go-wasm/runtimes/sqld-0.24.32/bin/sqld); this test never skips")
    });
    assert!(
        Path::new(&bin).is_file(),
        "EDGER_SQLD_BIN does not name a file: the test never skips"
    );

    let global = generate_global_keys();
    let dir = tempfile::tempdir().expect("private fixture tempdir");
    let (user_port, admin_port) = ephemeral_port_pair().await;
    assert_ne!(user_port, admin_port);
    let mut sqld = Sqld::spawn(&bin, dir.path(), &global.pub_pem, user_port, admin_port);
    eprintln!("quota-evidence: sqld pid={}", sqld.pid);

    let client = http_client();
    let ready = sqld.wait_ready(&client).await;
    let outcome = match ready {
        Ok(()) => {
            let ctx = Ctx {
                user_base: format!("http://127.0.0.1:{}", sqld.user_port),
                admin_base: format!("http://127.0.0.1:{}", sqld.admin_port),
                dir: dir.path().to_path_buf(),
                client,
                global,
            };
            run_owned(SCENARIO_DEADLINE, scenario(ctx)).await
        }
        Err(message) => {
            let stop = sqld.shutdown().await;
            panic!("sqld not ready: {message}; shutdown={stop:?}");
        }
    };
    // Shutdown BEFORE propagating any scenario failure.
    let stop = sqld.shutdown().await;
    eprintln!("quota-evidence: shutdown {stop:?}");
    // Logs: no admin key, no JWT.
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
        Err(None) => panic!(
            "scenario exceeded {SCENARIO_DEADLINE:?} (a timeout is never accepted as a quota)"
        ),
    }
    stop.expect("the owned sqld child is killed, reaped and gone");
    assert!(leaks.is_empty(), "child logs carry a secret: {leaks:?}");
}
