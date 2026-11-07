//! EDG-53 11b: the fullstack `wasm` adapter through the REAL `edger` binary.
//!
//! The binary is launched as a child process (`CARGO_BIN_EXE_edger`) on a
//! loopback ephemeral port with every directory (workers, core, overlay,
//! api-keys, secrets, tmp) inside one tempdir and an `env_clear` + minimal
//! whitelist environment. Two synthetic versions of the same app are
//! installed through `POST /api/admin/workers/install`; every request below
//! is plain TCP via `reqwest` and the API backend is a real WAT guest run by
//! the production `RuntimeIsolateFactory`. `EDGER_DENO_BIN` points at a path
//! that does not exist, so a request that reached a JS process could never
//! succeed: the API answers prove no Deno was involved.
#![cfg(unix)]

use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use reqwest::header::{HeaderMap, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE};
use reqwest::{Client, Method, StatusCode};
use sha2::{Digest, Sha256};
use url::Url;

const APP: &str = "wasm-fullstack";
const VHOST: &str = "fullstack.fixture.test";
/// Local canary root key: it only exists inside this test's child.
const ROOT_KEY: &str = "edg53-11b-local-root-canary";
const ASSET: &str = "assets/app-a1b2c3d4.js";
const LOG_TAIL_BYTES: usize = 2048;

// ---------------------------------------------------------------------------
// Fixtures: WAT guest + synthetic packages
// ---------------------------------------------------------------------------

/// Constant guest: always answers `status` + `body` (JSON). It uses the
/// existing ABI (`edger_alloc` / `edger_handle` returning `ptr | len << 32`
/// of the response frame `status:u16 pad:u16 hlen:u32 blen:u32 headers body`).
fn guest_wat(status: u16, body: &str) -> String {
    const FRAME_AT: usize = 65536;
    let headers = r#"[["content-type","application/json"]]"#;
    let mut frame = Vec::new();
    frame.extend_from_slice(&status.to_le_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&(headers.len() as u32).to_le_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(headers.as_bytes());
    frame.extend_from_slice(body.as_bytes());
    let mut escaped = String::new();
    for byte in &frame {
        write!(escaped, "\\{byte:02x}").unwrap();
    }
    format!(
        r#"(module
  (memory (export "memory") 3)
  (data (i32.const {FRAME_AT}) "{escaped}")
  (func (export "edger_alloc") (param i32) (result i32) i32.const 1024)
  (func (export "edger_handle") (param i32) (param i32) (result i64)
    i64.const {FRAME_AT}
    i64.const {len}
    i64.const 32
    i64.shl
    i64.or)
)"#,
        len = frame.len(),
    )
}

fn js_body(version: &str) -> Vec<u8> {
    let mut body = format!("/* wasm-fullstack {version} */\nconst v = '{version}';\n");
    let mut index = 0;
    while body.len() < 8192 {
        body.push_str(&format!(
            "export const c{index} = 'padding padding padding';\n"
        ));
        index += 1;
    }
    body.into_bytes()
}

fn index_html(version: &str) -> String {
    format!("<!doctype html><html><head></head><body>SPA-SHELL-{version}</body></html>")
}

fn api_body(version: &str, not_found: bool) -> String {
    if not_found {
        format!(r#"{{"app":"{APP}","version":"{version}","error":"guest-not-found"}}"#)
    } else {
        format!(r#"{{"app":"{APP}","version":"{version}"}}"#)
    }
}

/// Which pieces of the synthetic package are shipped (refusal cases drop one).
#[derive(Clone, Copy)]
struct Shape {
    backend: bool,
    client_index: bool,
    /// A valid-looking top-level `entrypoint` plus a root `index.html`: such a
    /// package must still be refused when the nested contract is incomplete.
    decoy_top_level: bool,
}

const FULL: Shape = Shape {
    backend: true,
    client_index: true,
    decoy_top_level: false,
};

/// One synthetic package: manifest (nested contract only) + real WAT +
/// `dist/` (index + asset).
fn package(version: &str, status: u16) -> Vec<u8> {
    package_shaped(version, status, FULL)
}

fn package_shaped(version: &str, status: u16, shape: Shape) -> Vec<u8> {
    let top_level = if shape.decoy_top_level {
        "entrypoint: decoy.ts\n"
    } else {
        ""
    };
    let manifest = format!(
        "name: {APP}\nversion: \"{version}\"\nkind: fullstack\nadapter: wasm\nclientDir: dist\n\
         {top_level}wasmBackend:\n  entrypoint: backend/index.wat\n  apiPrefix: /api\n\
         hosts:\n  - {VHOST}\nnameRoute: true\n"
    );
    let wat = guest_wat(status, &api_body(version, status == 404));
    let asset_path = format!("dist/{ASSET}");
    let mut files: Vec<(&str, Vec<u8>)> = vec![
        ("manifest.yaml", manifest.into_bytes()),
        (&asset_path, js_body(version)),
    ];
    if shape.backend {
        files.push(("backend/index.wat", wat.into_bytes()));
    }
    if shape.client_index {
        files.push(("dist/index.html", index_html(version).into_bytes()));
    }
    if shape.decoy_top_level {
        files.push(("decoy.ts", b"export default {};".to_vec()));
        files.push(("index.html", b"<html>root</html>".to_vec()));
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in files {
            writer.start_file(name, options).unwrap();
            writer.write_all(&contents).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

// ---------------------------------------------------------------------------
// The real process
// ---------------------------------------------------------------------------

struct Edger {
    child: Child,
    addr: SocketAddr,
    pid: u32,
    root: PathBuf,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
    deno_missing: PathBuf,
}

fn free_loopback_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn log_tail(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_default();
    let from = bytes.len().saturating_sub(LOG_TAIL_BYTES);
    String::from_utf8_lossy(&bytes[from..]).into_owned()
}

impl Edger {
    fn launch(root: &Path) -> Edger {
        let workers = root.join("workers");
        let core = root.join("core");
        let overlay = root.join("overlay");
        let state = root.join("state");
        let home = root.join("home");
        let tmp = root.join("tmp");
        for dir in [&workers, &core, &overlay, &state, &home, &tmp] {
            fs::create_dir_all(dir).unwrap();
        }
        let port = free_loopback_port();
        let stdout_log = root.join("edger.stdout.log");
        let stderr_log = root.join("edger.stderr.log");
        let deno_missing = root.join("no-deno-here/deno");
        assert!(!deno_missing.exists());

        let mut command = Command::new(env!("CARGO_BIN_EXE_edger"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("TMPDIR", &tmp)
            .env("EDGER_BIND", "127.0.0.1")
            .env("PORT", port.to_string())
            .env("RUNTIME_WORKER_DIRS", &workers)
            .env("EDGER_CORE_WORKER_DIR", &core)
            .env("EDGER_CORE_WORKER_OVERLAY_DIR", &overlay)
            .env("EDGER_API_KEYS_DB", state.join("api-keys.db"))
            .env("EDGER_APP_SECRETS_DB", state.join("app-secrets.db"))
            .env("ROOT_API_KEY", ROOT_KEY)
            .env("EDGER_DENO_BIN", &deno_missing)
            .env("EDGER_CRON_ENABLED", "false")
            .stdin(Stdio::null())
            .stdout(fs::File::create(&stdout_log).unwrap())
            .stderr(fs::File::create(&stderr_log).unwrap());
        let child = command.spawn().expect("spawn the edger binary");
        let pid = child.id();
        Edger {
            child,
            addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            pid,
            root: root.to_path_buf(),
            stdout_log,
            stderr_log,
            deno_missing,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn diagnostics(&self) -> String {
        format!(
            "child pid {} addr {}\n--- stdout tail ---\n{}\n--- stderr tail ---\n{}",
            self.pid,
            self.addr,
            log_tail(&self.stdout_log),
            log_tail(&self.stderr_log),
        )
    }

    async fn wait_ready(&mut self, client: &Client) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "edger exited before ready: {status}\n{}",
                    self.diagnostics()
                );
            }
            if let Ok(response) = client.get(self.url("/readyz")).send().await {
                if response.status() == StatusCode::OK {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "edger not ready in 60s\n{}",
                self.diagnostics()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// SIGTERM through the own child's PID, then `wait` on the handle; kills
    /// the child when it does not leave in time. Returns how it ended.
    fn shutdown(&mut self) -> String {
        if let Ok(Some(status)) = self.child.try_wait() {
            return format!("already exited: {status}");
        }
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.pid.to_string())
            .status();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return format!("graceful (SIGTERM): {status}");
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let status = self.child.wait().unwrap();
                return format!("killed after 10s: {status}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Edger {
    /// Runs on success AND on panic: no own process survives the test.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn content_type(&self) -> String {
        self.headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|err| panic!("body is not JSON ({err}): {}", self.text()))
    }
}

async fn send(
    client: &Client,
    edger: &Edger,
    method: Method,
    path: &str,
    host: Option<&str>,
    extra: &[(&str, &str)],
) -> Reply {
    let mut request = client.request(method.clone(), edger.url(path));
    if let Some(host) = host {
        request = request.header("host", host);
    }
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let response = request
        .send()
        .await
        .unwrap_or_else(|err| panic!("{method} {path}: {err}\n{}", edger.diagnostics()));
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.unwrap().to_vec();
    Reply {
        status,
        headers,
        body,
    }
}

async fn get(client: &Client, edger: &Edger, path: &str, host: Option<&str>) -> Reply {
    send(client, edger, Method::GET, path, host, &[]).await
}

/// Entries (names) under the worker root: refusals must publish nothing.
fn published_entries(edger: &Edger) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(edger.root.join("workers"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

async fn install_refused(client: &Client, edger: &Edger, zip: Vec<u8>, what: &str) {
    let before = published_entries(edger);
    let response = client
        .post(edger.url("/api/admin/workers/install"))
        .header(CONTENT_TYPE, "application/zip")
        .header(AUTHORIZATION, format!("Bearer {ROOT_KEY}"))
        .body(zip)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {text}");
    assert!(text.contains("DEPLOY_INVALID_PACKAGE"), "{what}: {text}");
    assert_eq!(
        published_entries(edger),
        before,
        "{what}: a refused package must leave nothing behind"
    );
}

async fn install(client: &Client, edger: &Edger, zip: Vec<u8>) -> serde_json::Value {
    let response = client
        .post(edger.url("/api/admin/workers/install"))
        .header(CONTENT_TYPE, "application/zip")
        .header(AUTHORIZATION, format!("Bearer {ROOT_KEY}"))
        .body(zip)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, StatusCode::CREATED, "install: {text}");
    serde_json::from_str(&text).unwrap()
}

fn assert_html(reply: &Reply, marker: &str, what: &str) {
    assert_eq!(reply.status, StatusCode::OK, "{what}: {}", reply.text());
    assert!(
        reply.content_type().starts_with("text/html"),
        "{what}: content-type {}",
        reply.content_type()
    );
    assert!(
        reply.text().contains(marker),
        "{what}: missing {marker}: {}",
        reply.text()
    );
}

/// The guest answers JSON of exactly this version/status, never the SPA.
fn assert_guest(reply: &Reply, version: &str, status: u16, what: &str) {
    assert_eq!(reply.status.as_u16(), status, "{what}: {}", reply.text());
    assert!(
        reply.content_type().starts_with("application/json"),
        "{what}: content-type {}",
        reply.content_type()
    );
    assert!(
        !reply.text().to_ascii_lowercase().contains("<html"),
        "{what}: html leaked into an API answer"
    );
    assert_eq!(
        reply.json(),
        serde_json::from_str::<serde_json::Value>(&api_body(version, status == 404)).unwrap(),
        "{what}"
    );
}

fn decode_brotli(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    brotli::Decompressor::new(bytes, 4096)
        .read_to_end(&mut out)
        .expect("valid brotli body");
    out
}

fn decode_gzip(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .expect("valid gzip body");
    out
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fullstack_wasm_runs_through_the_real_edger_binary_without_deno() {
    let root = tempfile::tempdir().unwrap();
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let mut edger = Edger::launch(root.path());
    eprintln!("edger child pid={} addr={}", edger.pid, edger.addr);
    assert!(edger.addr.ip().is_loopback());
    edger.wait_ready(&client).await;

    // --- control API: root key required; public data needs no Bearer ------
    let denied = get(&client, &edger, "/api/admin/workers", None).await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED, "{}", denied.text());
    let allowed = client
        .get(edger.url("/api/admin/workers"))
        .header(AUTHORIZATION, format!("Bearer {ROOT_KEY}"))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);

    // --- two versions of the same app through the real install API --------
    // 1.0.0: guest answers 200 JSON; 2.0.0: guest answers an intentional
    // 404 JSON (proves the API never falls back to the SPA shell).
    // Refusals first (each publishes nothing): the nested contract decides,
    // so a decoy top-level entrypoint + root index never rescues a package.
    install_refused(
        &client,
        &edger,
        package_shaped(
            "0.0.1",
            200,
            Shape {
                backend: false,
                client_index: true,
                decoy_top_level: true,
            },
        ),
        "missing nested backend, valid top-level entrypoint + root index",
    )
    .await;
    install_refused(
        &client,
        &edger,
        package_shaped(
            "0.0.1",
            200,
            Shape {
                backend: false,
                client_index: true,
                decoy_top_level: false,
            },
        ),
        "missing nested backend",
    )
    .await;
    install_refused(
        &client,
        &edger,
        package_shaped(
            "0.0.1",
            200,
            Shape {
                backend: true,
                client_index: false,
                decoy_top_level: true,
            },
        ),
        "missing clientDir/index.html, valid top-level entrypoint + root index",
    )
    .await;

    let first = install(&client, &edger, package("1.0.0", 200)).await;
    let second = install(&client, &edger, package("2.0.0", 404)).await;
    eprintln!("install#1={first} install#2={second}");
    let default = second["defaultVersion"]
        .as_str()
        .expect("defaultVersion in the install response")
        .to_string();
    assert!(default == "1.0.0" || default == "2.0.0", "{default}");
    let other = if default == "1.0.0" { "2.0.0" } else { "1.0.0" };
    let status_of = |version: &str| if version == "1.0.0" { 200 } else { 404 };

    // Installed layout: the WAT shipped as-is, variants next to the asset.
    let version_dir = |version: &str| {
        let workers = edger.root.join("workers");
        let pinned = workers.join(format!("{APP}@{version}"));
        if pinned.is_dir() {
            pinned
        } else {
            workers.join(APP)
        }
    };
    assert!(version_dir("1.0.0").join("backend/index.wat").is_file());

    let name = format!("/{APP}");

    // --- by-name route, default version ------------------------------------
    assert_html(
        &get(&client, &edger, &format!("{name}/"), None).await,
        &format!("SPA-SHELL-{default}"),
        "default root",
    );
    let asset = get(&client, &edger, &format!("{name}/{ASSET}"), None).await;
    assert_eq!(asset.status, StatusCode::OK);
    assert_eq!(asset.body, js_body(&default), "default asset bytes");
    assert_html(
        &get(&client, &edger, &format!("{name}/deep/spa/route"), None).await,
        &format!("SPA-SHELL-{default}"),
        "SPA fallback",
    );
    // `/apix` only shares a prefix with `/api`: it is a SPA route, not API.
    assert_html(
        &get(&client, &edger, &format!("{name}/apix"), None).await,
        &format!("SPA-SHELL-{default}"),
        "/apix is SPA",
    );
    // `/api` itself and below reach the WASM guest.
    assert_guest(
        &get(&client, &edger, &format!("{name}/api"), None).await,
        &default,
        status_of(&default),
        "default /api",
    );
    assert_guest(
        &get(&client, &edger, &format!("{name}/api/items?x=1"), None).await,
        &default,
        status_of(&default),
        "default /api/items",
    );

    // --- @version pins stay isolated from the default ---------------------
    for version in ["1.0.0", "2.0.0"] {
        let pin = format!("/{APP}@{version}");
        assert_html(
            &get(&client, &edger, &format!("{pin}/"), None).await,
            &format!("SPA-SHELL-{version}"),
            &format!("pin {version} root"),
        );
        assert_guest(
            &get(&client, &edger, &format!("{pin}/api/x"), None).await,
            version,
            status_of(version),
            &format!("pin {version} /api/x"),
        );
        let pinned_asset = get(&client, &edger, &format!("{pin}/{ASSET}"), None).await;
        assert_eq!(pinned_asset.body, js_body(version), "pin {version} asset");
    }
    assert_ne!(default, other);

    // --- vhost through the Host header (still connecting to 127.0.0.1) ----
    assert_html(
        &get(&client, &edger, "/", Some(VHOST)).await,
        &format!("SPA-SHELL-{default}"),
        "vhost root",
    );
    assert_html(
        &get(&client, &edger, "/some/client/route", Some(VHOST)).await,
        &format!("SPA-SHELL-{default}"),
        "vhost SPA fallback",
    );
    assert_guest(
        &get(&client, &edger, "/api/ping", Some(VHOST)).await,
        &default,
        status_of(&default),
        "vhost /api/ping",
    );

    // --- methods outside the API --------------------------------------------
    let post = send(
        &client,
        &edger,
        Method::POST,
        &format!("{name}/"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        post.status,
        StatusCode::METHOD_NOT_ALLOWED,
        "{}",
        post.text()
    );
    let head = send(
        &client,
        &edger,
        Method::HEAD,
        &format!("{name}/"),
        None,
        &[],
    )
    .await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty(), "HEAD must not carry a body");
    // Control: POST INSIDE the API prefix reaches the guest.
    let api_post = send(
        &client,
        &edger,
        Method::POST,
        &format!("/{APP}@1.0.0/api/items"),
        None,
        &[],
    )
    .await;
    assert_guest(&api_post, "1.0.0", 200, "POST /api");

    // --- precompress at install + negotiation (1.0.0 asset) ---------------
    let dir = version_dir("1.0.0");
    let br_file = dir.join(format!("dist/{ASSET}.br"));
    let gz_file = dir.join(format!("dist/{ASSET}.gz"));
    assert!(br_file.is_file(), "install generated {}", br_file.display());
    assert!(gz_file.is_file(), "install generated {}", gz_file.display());
    let pin_asset = format!("/{APP}@1.0.0/{ASSET}");
    let br = send(
        &client,
        &edger,
        Method::GET,
        &pin_asset,
        None,
        &[(ACCEPT_ENCODING.as_str(), "br")],
    )
    .await;
    assert_eq!(br.status, StatusCode::OK);
    assert_eq!(
        br.headers.get(CONTENT_ENCODING).map(|v| v.as_bytes()),
        Some(&b"br"[..])
    );
    assert_eq!(decode_brotli(&br.body), js_body("1.0.0"));
    let gz = send(
        &client,
        &edger,
        Method::GET,
        &pin_asset,
        None,
        &[(ACCEPT_ENCODING.as_str(), "gzip")],
    )
    .await;
    assert_eq!(gz.status, StatusCode::OK);
    assert_eq!(
        gz.headers.get(CONTENT_ENCODING).map(|v| v.as_bytes()),
        Some(&b"gzip"[..])
    );
    assert_eq!(decode_gzip(&gz.body), js_body("1.0.0"));

    // --- no Deno, no operator secret, bounded logs ----------------------------
    assert!(!edger.deno_missing.exists(), "the Deno path stays absent");
    let logs = format!(
        "{}{}",
        log_tail(&edger.stdout_log),
        log_tail(&edger.stderr_log)
    );
    assert!(
        !logs.contains(ROOT_KEY),
        "the root key canary must never reach the logs"
    );

    // --- cleanup: graceful stop, no child survives -----------------------------
    let ended = edger.shutdown();
    eprintln!("edger child pid={} ended: {ended}", edger.pid);
    assert!(
        matches!(edger.child.try_wait(), Ok(Some(_))),
        "child must be reaped"
    );
}

// ---------------------------------------------------------------------------
// EDG-53 12b: the real Vite + Go package through the real binary
// ---------------------------------------------------------------------------

const GO_ZIP_ENV: &str = "EDGER_GO_WASM_FULLSTACK_ZIP";
/// SHA-256 of the approved Go artifact shipped as `backend/index.wasm`.
const GO_WASM_SHA: &str = "8062d9cbed8f48dd8f167d7805b2f83e30e401c0d3a072c9d82dc5f810f535b0";
/// Pinned CONTENT of the four package files (name, exact size, SHA-256). The
/// authority is the content, not the ZIP container: a repack with other
/// timestamps, order or compression is accepted, a changed byte is not. The
/// 12a container SHA (4fb2997b...ff975) is evidence only, never required.
const GO_PINS: [(&str, usize, &str); 4] = [
    (
        "manifest.yaml",
        866,
        "f021d35ab9019952aee5938ac91145a10565d6b7108bdfdc5f1937322f323cf9",
    ),
    ("backend/index.wasm", 11_676_592, GO_WASM_SHA),
    (
        "dist/index.html",
        2196,
        "421c33ec15e9ae289224e164e866b2880abc3c8e6a76815761f861e4a8773010",
    ),
    (
        "dist/assets/index-BAhFX9bS.js",
        1600,
        "f0994a8da6efffbb2f61f3a4e5d286dd177df0834fb7f47b34f636233d858def",
    ),
];
/// Optional structural directory entries; nothing else is allowed.
const GO_DIRS: [&str; 3] = ["backend/", "dist/", "dist/assets/"];
/// Upper bound for the ZIP file read from the environment path.
const GO_ZIP_MAX_BYTES: u64 = 32 * 1024 * 1024;
const GO_APP: &str = "go-wasm-fullstack";
const GO_ASSET: &str = "assets/index-BAhFX9bS.js";
/// Reserved `.test` name added ONLY by the 2.0.0 test overlay; the original
/// 1.0.0 ZIP declares no host.
const GO_VHOST: &str = "fullstack-go.fixture.test";
/// Local canary for the request header/body: it must never reach the logs.
const GO_CANARY: &str = "go-canary-x-12b";
/// Local canary for the query string (the operational log records only the path).
const GO_QUERY_CANARY: &str = "go-canary-q-12b";

type ZipEntries = Vec<(String, Option<Vec<u8>>)>;

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// One pinned file: exact name, exact uncompressed size, SHA-256.
struct Pin {
    name: String,
    size: usize,
    sha: String,
}

/// Validates the WHOLE archive before anything is used: only pinned files
/// (each exactly once, exact size and SHA-256) plus optional pinned
/// directories; unknown, hostile, duplicate, missing or tampered entries and
/// truncated archives are refused. Every error is static: it never echoes
/// a path or bytes. Reads are bounded by the pinned size (+1 to detect growth).
fn parse_go_zip(bytes: &[u8], pins: &[Pin], dirs: &[&str]) -> Result<ZipEntries, &'static str> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| "not a valid ZIP")?;
    if archive.len() > pins.len() + dirs.len() {
        return Err("too many ZIP entries");
    }
    let mut seen_files = vec![false; pins.len()];
    let mut seen_dirs = vec![false; dirs.len()];
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|_| "unreadable ZIP entry")?;
        let name = file
            .name()
            .map_err(|_| "ZIP entry name is not valid UTF-8")?
            .into_owned();
        if file.is_dir() {
            let at = dirs
                .iter()
                .position(|dir| *dir == name)
                .ok_or("unexpected directory entry")?;
            if std::mem::replace(&mut seen_dirs[at], true) {
                return Err("duplicate directory entry");
            }
            if file.size() != 0 {
                return Err("directory entry carries data");
            }
            entries.push((name, None));
            continue;
        }
        let at = pins
            .iter()
            .position(|pin| pin.name == name)
            .ok_or("unexpected file entry")?;
        if std::mem::replace(&mut seen_files[at], true) {
            return Err("duplicate file entry");
        }
        let pin = &pins[at];
        if file.size() != pin.size as u64 {
            return Err("file size differs from the pin");
        }
        let mut data = Vec::with_capacity(pin.size);
        Read::take(&mut file, pin.size as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|_| "unreadable ZIP entry data")?;
        if data.len() != pin.size {
            return Err("file size differs from the pin");
        }
        if sha_hex(&data) != pin.sha {
            return Err("file content differs from the pin");
        }
        entries.push((name, Some(data)));
    }
    if seen_files.iter().any(|seen| !seen) {
        return Err("a pinned file is missing");
    }
    Ok(entries)
}

/// The original package entries from the ZIP at `EDGER_GO_WASM_FULLSTACK_ZIP`
/// (bounded read, content-pinned). A missing, unreadable, oversized or wrong
/// ZIP is a static panic, never a skip.
fn read_go_zip() -> ZipEntries {
    let path = std::env::var(GO_ZIP_ENV)
        .unwrap_or_else(|_| panic!("{GO_ZIP_ENV} is required (absolute path of the 12a ZIP)"));
    let file =
        fs::File::open(path).unwrap_or_else(|_| panic!("{GO_ZIP_ENV} is not a readable file"));
    let length = file
        .metadata()
        .unwrap_or_else(|_| panic!("{GO_ZIP_ENV} is not a readable file"))
        .len();
    assert!(length <= GO_ZIP_MAX_BYTES, "{GO_ZIP_ENV} is too large");
    let mut bytes = Vec::new();
    Read::take(file, GO_ZIP_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .unwrap_or_else(|_| panic!("{GO_ZIP_ENV} is not a readable file"));
    assert!(
        bytes.len() as u64 <= GO_ZIP_MAX_BYTES,
        "{GO_ZIP_ENV} is too large"
    );
    let pins: Vec<Pin> = GO_PINS
        .iter()
        .map(|(name, size, sha)| Pin {
            name: (*name).to_string(),
            size: *size,
            sha: (*sha).to_string(),
        })
        .collect();
    parse_go_zip(&bytes, &pins, &GO_DIRS)
        .unwrap_or_else(|reason| panic!("{GO_ZIP_ENV} is not the approved package: {reason}"))
}

fn zip_of(entries: &ZipEntries) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            match data {
                None => writer.add_directory(name.as_str(), options).unwrap(),
                Some(data) => {
                    writer.start_file(name.as_str(), options).unwrap();
                    writer.write_all(data).unwrap();
                }
            }
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

fn zip_file<'a>(entries: &'a ZipEntries, name: &str) -> &'a [u8] {
    entries
        .iter()
        .find(|(entry, _)| entry == name)
        .and_then(|(_, data)| data.as_deref())
        .unwrap_or_else(|| panic!("the ZIP has no file {name}"))
}

/// Request with a body; same shape as `send`.
async fn send_body(
    client: &Client,
    edger: &Edger,
    path: &str,
    host: Option<&str>,
    extra: &[(&str, &str)],
    body: Vec<u8>,
) -> Reply {
    let mut request = client.post(edger.url(path)).body(body);
    if let Some(host) = host {
        request = request.header("host", host);
    }
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let response = request
        .send()
        .await
        .unwrap_or_else(|err| panic!("POST {path}: {err}\n{}", edger.diagnostics()));
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.unwrap().to_vec();
    Reply {
        status,
        headers,
        body,
    }
}

/// Value of `attr="..."` inside the first tag that starts with `tag_start`.
fn tag_attr(html: &str, tag_start: &str, attr: &str) -> Option<String> {
    let from = html.find(tag_start)?;
    let tag = &html[from..from + html[from..].find('>')?];
    let marker = format!("{attr}=\"");
    let value_from = tag.find(&marker)? + marker.len();
    let len = tag[value_from..].find('"')?;
    Some(tag[value_from..value_from + len].to_string())
}

/// What a browser derives from the served HTML for a page at `page`.
struct Derived {
    base: Url,
    asset: Url,
    api: Url,
}

fn derive(page: &Url, html: &str, what: &str) -> Derived {
    let base_href = tag_attr(html, "<base", "href")
        .unwrap_or_else(|| panic!("{what}: no <base href> in the served HTML:\n{html}"));
    let base = page
        .join(&base_href)
        .unwrap_or_else(|_| panic!("{what}: bad base href"));
    let src = tag_attr(html, "<script", "src")
        .unwrap_or_else(|| panic!("{what}: no script src in the served HTML:\n{html}"));
    // `<base>` governs every relative URL of the document.
    let asset = base.join(&src).unwrap();
    let api = base.join("api/inspect").unwrap();
    Derived { base, asset, api }
}

struct Route {
    what: &'static str,
    prefix: String,
    host: Option<&'static str>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "opt-in: real Go/Vite package, needs EDGER_GO_WASM_FULLSTACK_ZIP"]
async fn real_go_vite_fullstack_package_through_real_edger_binary() {
    let entries = read_go_zip();
    let wasm = zip_file(&entries, "backend/index.wasm").to_vec();
    assert_eq!(
        sha_hex(&wasm),
        GO_WASM_SHA,
        "backend is not the approved Go"
    );
    let js = zip_file(&entries, &format!("dist/{GO_ASSET}")).to_vec();
    assert_eq!(js.len(), 1600, "the original Vite asset size");
    let manifest = String::from_utf8(zip_file(&entries, "manifest.yaml").to_vec()).unwrap();
    let v1_line = "\nversion: \"1.0.0\"\n";
    assert!(manifest.contains(v1_line));
    assert!(
        !manifest.contains(
            "
hosts:"
        ),
        "the original declares no host"
    );

    // Test overlay (NOT the original): only `version` and `hosts` differ.
    let overlay_manifest = format!(
        "{}hosts:\n  - {GO_VHOST}\n",
        manifest.replace(v1_line, "\nversion: \"2.0.0\"\n")
    );
    let overlay_entries: ZipEntries = entries
        .iter()
        .map(|(name, data)| {
            if name == "manifest.yaml" {
                (name.clone(), Some(overlay_manifest.clone().into_bytes()))
            } else {
                (name.clone(), data.clone())
            }
        })
        .collect();
    let zip_v1 = zip_of(&entries);
    let zip_v2 = zip_of(&overlay_entries);

    let root = tempfile::tempdir().unwrap();
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let mut edger = Edger::launch(root.path());
    eprintln!("edger child pid={} addr={}", edger.pid, edger.addr);
    assert!(edger.addr.ip().is_loopback());
    edger.wait_ready(&client).await;

    let first = install(&client, &edger, zip_v1).await;
    let second = install(&client, &edger, zip_v2).await;
    eprintln!("install#1={first} install#2={second}");
    let default = second["defaultVersion"]
        .as_str()
        .expect("defaultVersion in the install response")
        .to_string();
    assert_eq!(default, "2.0.0", "default version after both installs");

    // Installed layout: the Go module shipped as-is (not WAT).
    let version_dir = |version: &str| {
        let workers = edger.root.join("workers");
        let pinned = workers.join(format!("{GO_APP}@{version}"));
        if pinned.is_dir() {
            pinned
        } else {
            workers.join(GO_APP)
        }
    };
    for version in ["1.0.0", "2.0.0"] {
        let installed = fs::read(version_dir(version).join("backend/index.wasm")).unwrap();
        assert_eq!(sha_hex(&installed), GO_WASM_SHA, "installed Go {version}");
    }

    // --- routes: default, pins, vhost (same dist bytes in both versions) ---
    let routes = [
        Route {
            what: "default",
            prefix: format!("/{GO_APP}/"),
            host: None,
        },
        Route {
            what: "pin 1.0.0",
            prefix: format!("/{GO_APP}@1.0.0/"),
            host: None,
        },
        Route {
            what: "pin 2.0.0",
            prefix: format!("/{GO_APP}@2.0.0/"),
            host: None,
        },
        Route {
            what: "vhost",
            prefix: "/".to_string(),
            host: Some(GO_VHOST),
        },
    ];
    for route in &routes {
        let what = route.what;
        let authority = route.host.map_or(edger.addr.to_string(), str::to_string);
        let origin = format!("http://{authority}");
        // Root and a deep SPA route both serve the shell with the same base.
        let mut derived_pages = Vec::new();
        for page_path in [
            route.prefix.clone(),
            format!("{}deep/spa/route", route.prefix),
        ] {
            let reply = get(&client, &edger, &page_path, route.host).await;
            assert_html(&reply, "Testar API Go", &format!("{what} {page_path}"));
            let page = Url::parse(&format!("{origin}{page_path}")).unwrap();
            let derived = derive(&page, &reply.text(), &format!("{what} {page_path}"));
            assert_eq!(
                derived.base.path(),
                route.prefix,
                "{what} {page_path}: base href must not follow the deep route"
            );
            assert_eq!(
                derived.asset.path(),
                format!("{}{GO_ASSET}", route.prefix),
                "{what} {page_path}: module script URL"
            );
            assert_eq!(
                derived.api.path(),
                format!("{}api/inspect", route.prefix),
                "{what} {page_path}: API URL"
            );
            derived_pages.push(derived);
        }
        let derived = &derived_pages[0];
        // The module script resolved as a browser would, served over TCP.
        let asset = get(&client, &edger, derived.asset.path(), route.host).await;
        assert_eq!(asset.status, StatusCode::OK, "{what} asset");
        assert_eq!(asset.body, js, "{what}: asset bytes equal the ZIP's");
        let asset_text = asset.text();
        assert!(
            asset_text.contains("api/inspect") && asset_text.contains("document.baseURI"),
            "{what}: the shipped JS derives its API URL from the document base"
        );

        // POST through the URL the SPA derives (ethical body and header).
        let target = format!(
            "{}/a%2Fb?x=a%2Fb&x=two&empty=&k={GO_QUERY_CANARY}",
            derived.api.path()
        );
        let body = vec![0u8, 255, 10, b'g', b'o'];
        let reply = send_body(
            &client,
            &edger,
            &target,
            route.host,
            &[("x-example", GO_CANARY), ("content-type", "text/plain")],
            body.clone(),
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "{what} POST: {}",
            reply.text()
        );
        assert!(reply.content_type().starts_with("application/json"));
        assert!(!reply.text().to_ascii_lowercase().contains("<html"));
        let inspect = reply.json();
        eprintln!("{what} inspect={inspect}");
        assert_eq!(inspect["method"], "POST", "{what}");
        assert_eq!(inspect["xExample"], GO_CANARY, "{what}");
        assert!(inspect["rawQuery"]
            .as_str()
            .unwrap()
            .ends_with(&format!("x=a%2Fb&x=two&empty=&k={GO_QUERY_CANARY}")));
        assert!(inspect["requestUri"].as_str().unwrap().contains("a%2Fb"));
        assert!(inspect["rawPath"].as_str().unwrap().contains("a%2Fb"));
        let echoed = base64::engine::general_purpose::STANDARD
            .decode(inspect["body"].as_str().expect("body is base64"))
            .unwrap();
        assert_eq!(echoed, body, "{what}: body bytes");

        // /api/inspect itself (GET) and the 404 / SPA / db controls.
        let plain = get(&client, &edger, derived.api.path(), route.host).await;
        assert_eq!(plain.status, StatusCode::OK, "{what} GET inspect");
        assert_eq!(plain.json()["method"], "GET", "{what}");
        let missing = get(
            &client,
            &edger,
            &format!("{}api/nope", route.prefix),
            route.host,
        )
        .await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{what} api/nope");
        assert!(
            missing.content_type().starts_with("application/json"),
            "{what}: api 404 content-type {}",
            missing.content_type()
        );
        assert!(!missing.text().to_ascii_lowercase().contains("<html"));
        let _ = missing.json();
        assert_html(
            &get(
                &client,
                &edger,
                &format!("{}apix", route.prefix),
                route.host,
            )
            .await,
            "Testar API Go",
            &format!("{what} /apix is SPA"),
        );
        // No database configuration is declared: the Go module answers 503.
        // This is a missing-config answer, not a database proof.
        let db = get(
            &client,
            &edger,
            &format!("{}api/db", route.prefix),
            route.host,
        )
        .await;
        assert_eq!(db.status, StatusCode::SERVICE_UNAVAILABLE, "{what} api/db");
        assert!(!db.text().to_ascii_lowercase().contains("<html"));

        // The module and the manifest are never served as assets.
        for hidden in ["backend/index.wasm", "manifest.yaml", "dist/index.html"] {
            let reply = get(
                &client,
                &edger,
                &format!("{}{hidden}", route.prefix),
                route.host,
            )
            .await;
            assert!(
                !reply.body.starts_with(b"\0asm") && reply.body != wasm,
                "{what}: {hidden} leaked the Go module"
            );
            assert!(
                !reply.text().contains("wasmBackend"),
                "{what}: {hidden} leaked the manifest"
            );
        }
    }

    // --- precompressed variants of the original 1600 B asset ---------------
    for version in ["1.0.0", "2.0.0"] {
        let dir = version_dir(version);
        for ext in ["br", "gz"] {
            let file = dir.join(format!("dist/{GO_ASSET}.{ext}"));
            assert!(file.is_file(), "install generated {}", file.display());
        }
        let pin_asset = format!("/{GO_APP}@{version}/{GO_ASSET}");
        let br = send(
            &client,
            &edger,
            Method::GET,
            &pin_asset,
            None,
            &[(ACCEPT_ENCODING.as_str(), "br")],
        )
        .await;
        assert_eq!(br.status, StatusCode::OK, "{version} br");
        assert_eq!(
            br.headers.get(CONTENT_ENCODING).map(|v| v.as_bytes()),
            Some(&b"br"[..]),
            "{version} br encoding"
        );
        assert_eq!(decode_brotli(&br.body), js, "{version} br bytes");
        let gz = send(
            &client,
            &edger,
            Method::GET,
            &pin_asset,
            None,
            &[(ACCEPT_ENCODING.as_str(), "gzip")],
        )
        .await;
        assert_eq!(gz.status, StatusCode::OK, "{version} gz");
        assert_eq!(
            gz.headers.get(CONTENT_ENCODING).map(|v| v.as_bytes()),
            Some(&b"gzip"[..]),
            "{version} gzip encoding"
        );
        assert_eq!(decode_gzip(&gz.body), js, "{version} gzip bytes");
    }

    // --- stop and reap the child BEFORE reading logs ------------------------
    assert!(!edger.deno_missing.exists(), "the Deno path stays absent");
    let ended = edger.shutdown();
    eprintln!("edger child pid={} ended: {ended}", edger.pid);
    assert!(
        matches!(edger.child.try_wait(), Ok(Some(_))),
        "child must be reaped"
    );
    for (what, path) in [("stdout", &edger.stdout_log), ("stderr", &edger.stderr_log)] {
        let bytes = fs::read(path).unwrap_or_default();
        assert!(bytes.len() <= 1 << 20, "{what} log is over 1 MiB");
        let logs = String::from_utf8_lossy(&bytes);
        assert!(!logs.contains(ROOT_KEY), "{what}: root key canary in logs");
        assert!(!logs.contains(GO_CANARY), "{what}: request canary in logs");
        assert!(
            !logs.contains(GO_QUERY_CANARY),
            "{what}: query canary in logs"
        );
    }
}

// ---------------------------------------------------------------------------
// EDG-53 12c: the package admission is by content, not container bytes
// ---------------------------------------------------------------------------

#[cfg(test)]
mod go_zip_admission_tests {
    use super::*;

    const DIRS: [&str; 3] = ["a/", "b/", "b/c/"];
    const FILES: [(&str, &[u8]); 2] = [("a/one.bin", b"first payload"), ("b/c/two.bin", b"2nd")];

    fn pins() -> Vec<Pin> {
        FILES
            .iter()
            .map(|(name, data)| Pin {
                name: (*name).to_string(),
                size: data.len(),
                sha: sha_hex(data),
            })
            .collect()
    }

    fn build(
        dirs: &[&str],
        files: &[(&str, &[u8])],
        method: zip::CompressionMethod,
        year: u16,
    ) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let stamp = zip::DateTime::from_date_and_time(year, 7, 9, 12, 34, 56).unwrap();
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(method)
                .last_modified_time(stamp);
            for dir in dirs {
                writer.add_directory(*dir, options).unwrap();
            }
            for (name, data) in files {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn original() -> Vec<u8> {
        build(&DIRS, &FILES, zip::CompressionMethod::Deflated, 2026)
    }

    fn payload(entries: ZipEntries) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = entries
            .into_iter()
            .filter_map(|(name, data)| data.map(|data| (name, data)))
            .collect();
        files.sort();
        files
    }

    fn parse(bytes: &[u8]) -> Result<ZipEntries, &'static str> {
        parse_go_zip(bytes, &pins(), &DIRS)
    }

    #[test]
    fn repack_with_other_container_bytes_is_accepted_with_the_same_payload() {
        let first = original();
        let reversed: Vec<(&str, &[u8])> = FILES.iter().rev().copied().collect();
        // Other order, compression, timestamp and NO directory entries.
        let repack = build(&[], &reversed, zip::CompressionMethod::Stored, 2031);
        // Only some of the allowed directories.
        let partial = build(&["b/"], &FILES, zip::CompressionMethod::Stored, 2027);
        assert_ne!(sha_hex(&first), sha_hex(&repack), "different container");
        assert_ne!(sha_hex(&first), sha_hex(&partial));
        let expected = payload(parse(&first).unwrap());
        assert_eq!(expected.len(), 2);
        assert_eq!(payload(parse(&repack).unwrap()), expected);
        assert_eq!(payload(parse(&partial).unwrap()), expected);
    }

    #[test]
    fn a_single_changed_byte_in_any_file_is_refused() {
        for (index, (name, data)) in FILES.iter().enumerate() {
            let mut changed = data.to_vec();
            changed[0] ^= 1;
            let mut files = FILES.to_vec();
            files[index] = (name, &changed);
            let bytes = build(&DIRS, &files, zip::CompressionMethod::Deflated, 2026);
            assert_eq!(
                parse(&bytes).unwrap_err(),
                "file content differs from the pin"
            );
        }
        // Control: the untouched content is accepted.
        assert!(parse(&original()).is_ok());
        // A flipped byte inside a stored container fails too (CRC or pin).
        let stored = build(&DIRS, &FILES, zip::CompressionMethod::Stored, 2026);
        let at = stored
            .windows(FILES[0].1.len())
            .position(|window| window == FILES[0].1)
            .unwrap();
        let mut flipped = stored.clone();
        flipped[at] ^= 1;
        assert!(parse(&flipped).is_err());
    }

    #[test]
    fn size_growth_or_shrink_is_refused() {
        let longer = [FILES[0].1, b"!"].concat();
        let shorter = &FILES[0].1[..FILES[0].1.len() - 1];
        for data in [longer.as_slice(), shorter] {
            let files = [(FILES[0].0, data), FILES[1]];
            let bytes = build(&DIRS, &files, zip::CompressionMethod::Deflated, 2026);
            assert_eq!(parse(&bytes).unwrap_err(), "file size differs from the pin");
        }
    }

    #[test]
    fn missing_extra_hostile_and_unknown_entries_are_refused() {
        let method = zip::CompressionMethod::Deflated;
        assert_eq!(
            parse(&build(&DIRS, &FILES[..1], method, 2026)).unwrap_err(),
            "a pinned file is missing"
        );
        for hostile in [
            "../evil.bin",
            "/abs.bin",
            "a\\one.bin",
            "a/./one.bin",
            "extra.bin",
        ] {
            let files = [FILES[0], FILES[1], (hostile, b"x".as_slice())];
            assert_eq!(
                parse(&build(&[], &files, method, 2026)).unwrap_err(),
                "unexpected file entry",
                "{hostile}"
            );
        }
        let bytes = build(&["other/"], &FILES, method, 2026);
        assert_eq!(parse(&bytes).unwrap_err(), "unexpected directory entry");
        let extras = [FILES[0], FILES[1], ("x1", b""), ("x2", b"")];
        let many = build(&DIRS, &extras, method, 2026);
        assert_eq!(parse(&many).unwrap_err(), "too many ZIP entries");
    }

    #[test]
    fn duplicate_entries_are_refused() {
        // Same-length names so the container can be patched in place.
        let dup_pins = vec![
            Pin {
                name: "f1.bin".into(),
                size: 3,
                sha: sha_hex(b"one"),
            },
            Pin {
                name: "f2.bin".into(),
                size: 3,
                sha: sha_hex(b"two"),
            },
        ];
        let files: [(&str, &[u8]); 2] = [("f1.bin", b"one"), ("f2.bin", b"two")];
        let good = build(&[], &files, zip::CompressionMethod::Stored, 2026);
        assert!(parse_go_zip(&good, &dup_pins, &[]).is_ok());
        let mut patched = good.clone();
        while let Some(at) = patched.windows(6).position(|w| w == b"f2.bin") {
            patched[at..at + 6].copy_from_slice(b"f1.bin");
        }
        // The zip crate collapses duplicate central-directory names (the last
        // one wins), so the duplicate surfaces as a content/missing/size
        // refusal; our own duplicate guard stays as defense in depth.
        let err = parse_go_zip(&patched, &dup_pins, &[]).unwrap_err();
        assert!(
            matches!(
                err,
                "duplicate file entry"
                    | "file content differs from the pin"
                    | "a pinned file is missing"
                    | "not a valid ZIP"
            ),
            "{err}"
        );
    }

    #[test]
    fn truncated_empty_and_non_zip_inputs_are_refused_statically() {
        let bytes = original();
        assert_eq!(
            parse(&bytes[..bytes.len() - 10]).unwrap_err(),
            "not a valid ZIP"
        );
        assert_eq!(
            parse(&bytes[..bytes.len() / 2]).unwrap_err(),
            "not a valid ZIP"
        );
        assert_eq!(parse(b"").unwrap_err(), "not a valid ZIP");
        // A bare Go module (the wrong file) is not a ZIP: static, no leak.
        assert_eq!(
            parse(b"\0asm\x01\0\0\0 canary").unwrap_err(),
            "not a valid ZIP"
        );
    }

    #[test]
    fn the_real_pins_describe_the_four_package_files() {
        let names: Vec<&str> = GO_PINS.iter().map(|(name, _, _)| *name).collect();
        assert_eq!(
            names,
            [
                "manifest.yaml",
                "backend/index.wasm",
                "dist/index.html",
                "dist/assets/index-BAhFX9bS.js"
            ]
        );
        assert!(GO_PINS.iter().all(|(_, _, sha)| sha.len() == 64));
        let total: u64 = GO_PINS.iter().map(|(_, size, _)| *size as u64).sum();
        assert!(total < GO_ZIP_MAX_BYTES);
    }
}
