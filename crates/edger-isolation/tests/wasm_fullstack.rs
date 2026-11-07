//! EDG-53 11a: dispatch of the fullstack `wasm` adapter (SPA assets from
//! `clientDir` + WASM API behind `apiPrefix`).
//!
//! Tests that do not need a guest run against a recording spy `Isolate`
//! (no feature needed). The tests that prove API execution build a real WAT
//! guest and run it through `WasmIsolate` (`--features wasm`); no Deno.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::manifest::WasmBackendManifest;
use edger_core::{
    parse_worker_config, ExecutionKind, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, WorkerConfig, WorkerManifest, WorkerResponse,
};
use edger_isolation::{
    dispatch_fullstack_buffered, dispatch_fullstack_stream, try_serve_fullstack_asset,
};

const INDEX_HTML: &str =
    r#"<!doctype html><html><head><base href="/old/" /></head><body>SPA-SHELL</body></html>"#;
const ASSET_JS: &str = "console.log('app');";
const SECRET: &str = "PRIVATE-DATA";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Guest echoing the whole request frame as the response body, with a
/// configurable status; `trap` makes `edger_handle` trap instead.
fn guest_wat(status: u16, trap: bool) -> String {
    let headers = r#"[["x-guest","1"]]"#;
    let hlen = headers.len();
    let escaped = headers.replace('"', "\\\"");
    let body = if trap {
        "unreachable".to_string()
    } else {
        format!(
            r#"
        (local $i i32)
        i32.const 65536
        i32.const {status}
        i32.store16
        i32.const 65540
        i32.const {hlen}
        i32.store
        i32.const 65544
        local.get $l
        i32.store
        block $done
          loop $copy
            local.get $i
            local.get $l
            i32.lt_u
            if
              i32.const {out}
              local.get $i
              i32.add
              local.get $p
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
            br $done
          end
        end
        i32.const 65536
        i64.extend_i32_u
        i32.const {frame}
        local.get $l
        i32.add
        i64.extend_i32_u
        i64.const 32
        i64.shl
        i64.or"#,
            out = 65548 + hlen,
            frame = 12 + hlen,
        )
    };
    format!(
        r#"(module
  (memory (export "memory") 3)
  (data (i32.const 65548) "{escaped}")
  (func (export "edger_alloc") (param i32) (result i32) i32.const 1024)
  (func (export "edger_handle") (param $p i32) (param $l i32) (result i64){body}
  )
)"#
    )
}

#[cfg_attr(not(feature = "wasm"), allow(dead_code))]
struct Echo {
    method: String,
    uri: String,
    headers: String,
    body: Vec<u8>,
}

#[cfg_attr(not(feature = "wasm"), allow(dead_code))]
fn parse_echo(response: &SerializedResponse) -> Echo {
    let frame = response.body.as_ref().expect("echo body").to_vec();
    let word = |at: usize| u32::from_le_bytes(frame[at..at + 4].try_into().unwrap()) as usize;
    let (m, u, h, b) = (word(0), word(4), word(8), word(12));
    let text =
        |from: usize, len: usize| String::from_utf8(frame[from..from + len].to_vec()).unwrap();
    Echo {
        method: text(16, m),
        uri: text(16 + m, u),
        headers: text(16 + m + u, h),
        body: frame[16 + m + u + h..16 + m + u + h + b].to_vec(),
    }
}

/// worker dir: `dist/` (public), `backend/index.wasm`, plus private files.
fn build_app(guest: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("dist/assets")).unwrap();
    fs::create_dir_all(root.join("dist/sub")).unwrap();
    fs::create_dir_all(root.join("dist/.well-known")).unwrap();
    fs::create_dir_all(root.join("backend")).unwrap();
    fs::create_dir_all(root.join("private")).unwrap();
    fs::write(root.join("dist/index.html"), INDEX_HTML).unwrap();
    fs::write(root.join("dist/assets/app-AbCd1234.js"), ASSET_JS).unwrap();
    fs::write(root.join("dist/assets/app-AbCd1234.js.br"), b"BR-BYTES").unwrap();
    fs::write(root.join("dist/assets/app-AbCd1234.js.gz"), b"GZ-BYTES").unwrap();
    fs::write(root.join("dist/plain.txt"), "plain").unwrap();
    fs::write(root.join("dist/.hidden"), SECRET).unwrap();
    fs::write(root.join("dist/sub/.secret"), SECRET).unwrap();
    fs::write(root.join("dist/.well-known/ok.txt"), "well-known").unwrap();
    fs::write(root.join("private/secret.txt"), SECRET).unwrap();
    fs::write(root.join("private/index.html"), SECRET).unwrap();
    fs::write(root.join("edger.yaml"), SECRET).unwrap();
    fs::write(
        root.join("backend/index.wasm"),
        wat::parse_str(guest).unwrap(),
    )
    .unwrap();
    dir
}

fn manifest_with(prefix: &str, client_dir: &str) -> WorkerManifest {
    WorkerManifest {
        name: "spa".into(),
        kind: Some("fullstack".into()),
        adapter: Some("wasm".into()),
        client_dir: Some(client_dir.into()),
        wasm_backend: Some(WasmBackendManifest {
            entrypoint: Some("backend/index.wasm".into()),
            api_prefix: Some(prefix.into()),
        }),
        ..WorkerManifest::default()
    }
}

fn config_for(root: &Path, prefix: &str, client_dir: &str) -> WorkerConfig {
    let mut config = parse_worker_config(&manifest_with(prefix, client_dir));
    config.worker_dir = Some(root.to_path_buf());
    config
}

fn config(root: &Path) -> WorkerConfig {
    config_for(root, "/api", "dist")
}

fn request(method: &str, uri: &str) -> SerializedRequest {
    SerializedRequest {
        method: method.into(),
        uri: uri.into(),
        headers: vec![],
        body: None,
        request_id: "req".into(),
        base_href: Some("/app/".into()),
    }
}

fn body_text(response: &SerializedResponse) -> String {
    String::from_utf8_lossy(response.body.as_deref().unwrap_or_default()).into_owned()
}

fn header<'a>(response: &'a SerializedResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------------------
// Spy isolate
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Calls {
    wasm: Vec<(SerializedRequest, WorkerConfig)>,
    fetch: Vec<(SerializedRequest, WorkerConfig)>,
    other: usize,
}

#[derive(Clone, Default)]
struct Spy(Arc<Mutex<Calls>>);

impl Spy {
    fn wasm_calls(&self) -> usize {
        self.0.lock().unwrap().wasm.len()
    }
    fn total_calls(&self) -> usize {
        let calls = self.0.lock().unwrap();
        calls.wasm.len() + calls.fetch.len() + calls.other
    }
}

fn spy_answer(text: &str) -> SerializedResponse {
    SerializedResponse {
        status: 200,
        headers: vec![],
        body: Some(Bytes::from(text.to_string())),
    }
}

#[async_trait]
impl Isolate for Spy {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.0.lock().unwrap().fetch.push((req, config.clone()));
        Ok(spy_answer("spy-fetch"))
    }
    async fn execute_routes(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.0.lock().unwrap().other += 1;
        Ok(spy_answer("spy-routes"))
    }
    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.0.lock().unwrap().other += 1;
        Ok(spy_answer("spy-spa"))
    }
    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.0.lock().unwrap().wasm.push((req, config.clone()));
        Ok(spy_answer("spy-wasm"))
    }
}

// ---------------------------------------------------------------------------
// Spy tests: routing, clone and laziness (no guest involved)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spy_api_goes_to_execute_wasm_with_nested_entrypoint_and_preserved_config() {
    let app = build_app(&guest_wat(200, false));
    let mut original = config(app.path());
    original.env = HashMap::from([("DB_TOKEN".into(), "secret-value".into())]);
    original.secrets = vec!["DB_TOKEN".into()];
    original.allow_net = Some(vec!["api.example.test".into()]);
    original.timeout_ms = 1234;
    original.memory_mb = Some(64);
    original.max_body_size_bytes = Some(2048);
    original.public_env = vec!["PUBLIC_X".into()];
    let before = original.clone();
    let spy = Spy::default();
    let mut isolate = spy.clone();

    let mut req = request("POST", "/api/inspect?x=1");
    req.headers = vec![("x-test".into(), "kept".into())];
    req.body = Some(Bytes::from_static(b"payload"));
    let res = dispatch_fullstack_buffered(&mut isolate, req, &original)
        .await
        .unwrap();

    assert_eq!(body_text(&res), "spy-wasm");
    let calls = spy.0.lock().unwrap();
    assert_eq!(
        (calls.wasm.len(), calls.fetch.len(), calls.other),
        (1, 0, 0)
    );
    let (seen_req, seen_config) = &calls.wasm[0];
    assert_eq!(seen_req.uri, "/inspect?x=1");
    assert_eq!(seen_req.method, "POST");
    assert_eq!(seen_req.headers, vec![("x-test".into(), "kept".into())]);
    assert_eq!(seen_req.body.as_deref(), Some(b"payload".as_ref()));
    let mut expected = before.clone();
    expected.entrypoint = Some("backend/index.wasm".into());
    expected.kind = Some(ExecutionKind::WasmModule {
        entry: Some("backend/index.wasm".into()),
    });
    assert_eq!(seen_config, &expected, "only entrypoint/kind may differ");
    assert_eq!(original, before, "the received config is not mutated");
}

#[tokio::test]
async fn spy_assets_fallback_and_rejections_never_call_the_guest() {
    let app = build_app(&guest_wat(200, false));
    let config = config(app.path());
    let spy = Spy::default();
    let mut isolate = spy.clone();
    for (method, uri) in [
        ("GET", "/"),
        ("GET", "/assets/app-AbCd1234.js"),
        ("GET", "/deep/client/route"),
        ("HEAD", "/plain.txt"),
        ("GET", "/apix"),
        ("POST", "/form"),
        ("GET", "/%61pi/x"),
        ("GET", "/.hidden"),
    ] {
        dispatch_fullstack_buffered(&mut isolate, request(method, uri), &config)
            .await
            .unwrap();
    }
    assert_eq!(spy.total_calls(), 0);
}

#[tokio::test]
async fn spy_try_serve_returns_none_for_api_and_serves_the_rest() {
    let app = build_app(&guest_wat(200, false));
    let config = config(app.path());
    for method in ["GET", "POST", "HEAD", "DELETE"] {
        for uri in ["/api", "/api/", "/api/x?y=1", "https://h.test/api/x"] {
            assert!(
                try_serve_fullstack_asset(&request(method, uri), &config)
                    .unwrap()
                    .is_none(),
                "{method} {uri}"
            );
        }
    }
    let spa = try_serve_fullstack_asset(&request("GET", "/some/route"), &config)
        .unwrap()
        .unwrap();
    assert_eq!(spa.status, 200);
    assert!(body_text(&spa).contains("SPA-SHELL"));
    let post = try_serve_fullstack_asset(&request("POST", "/some/route"), &config)
        .unwrap()
        .unwrap();
    assert_eq!(post.status, 405);
}

#[tokio::test]
async fn spy_stream_wraps_api_and_static_as_buffered() {
    let app = build_app(&guest_wat(200, false));
    let config = config(app.path());
    let spy = Spy::default();
    let mut isolate = spy.clone();
    let api = dispatch_fullstack_stream(&mut isolate, request("GET", "/api/x"), &config)
        .await
        .unwrap();
    let stat = dispatch_fullstack_stream(&mut isolate, request("GET", "/"), &config)
        .await
        .unwrap();
    assert!(matches!(&api, WorkerResponse::Buffered(r) if body_text(r) == "spy-wasm"));
    assert!(matches!(&stat, WorkerResponse::Buffered(r) if r.status == 200));
    assert_eq!(spy.wasm_calls(), 1);
    assert_eq!(spy.0.lock().unwrap().fetch.len(), 0, "execute_fetch unused");
}

#[tokio::test]
async fn spy_wasm_without_backend_or_client_dir_is_a_static_error() {
    let app = build_app(&guest_wat(200, false));
    let mut manifest = manifest_with("/api", "dist");
    manifest.wasm_backend = None;
    let mut broken = parse_worker_config(&manifest);
    broken.worker_dir = Some(app.path().to_path_buf());
    let mut isolate = Spy::default();
    let err = dispatch_fullstack_buffered(&mut isolate, request("GET", "/api/x"), &broken)
        .await
        .unwrap_err();
    assert_eq!(err.code, "FULLSTACK_WASM_BACKEND_MISSING");

    let mut manifest = manifest_with("/api", "dist");
    manifest.client_dir = None;
    let mut no_client = parse_worker_config(&manifest);
    no_client.worker_dir = Some(app.path().to_path_buf());
    let err = dispatch_fullstack_buffered(&mut isolate, request("GET", "/"), &no_client)
        .await
        .unwrap_err();
    assert_eq!(err.code, "FULLSTACK_CLIENT_DIR_MISSING");
}

#[tokio::test]
async fn control_js_adapter_still_reaches_execute_fetch_with_ssr_entrypoint() {
    let app = build_app(&guest_wat(200, false));
    let manifest = WorkerManifest {
        name: "hono-app".into(),
        kind: Some("fullstack".into()),
        adapter: Some("hono".into()),
        ssr_entrypoint: Some("server/index.js".into()),
        ..WorkerManifest::default()
    };
    let mut config = parse_worker_config(&manifest);
    config.worker_dir = Some(app.path().to_path_buf());
    let spy = Spy::default();
    let mut isolate = spy.clone();
    let res = dispatch_fullstack_buffered(&mut isolate, request("GET", "/api/x"), &config)
        .await
        .unwrap();
    assert_eq!(body_text(&res), "spy-fetch");
    let calls = spy.0.lock().unwrap();
    assert_eq!((calls.wasm.len(), calls.fetch.len()), (0, 1));
    assert_eq!(
        calls.fetch[0].1.entrypoint.as_deref(),
        Some("server/index.js")
    );
}

// ---------------------------------------------------------------------------
// Static serving (no guest involved; Linux symlink cases are cfg(unix))
// ---------------------------------------------------------------------------

async fn get(config: &WorkerConfig, method: &str, uri: &str) -> SerializedResponse {
    let mut isolate = Spy::default();
    dispatch_fullstack_buffered(&mut isolate, request(method, uri), config)
        .await
        .unwrap()
}

#[tokio::test]
async fn assets_and_spa_fallback_from_client_dir_with_base_and_public_env() {
    let app = build_app(&guest_wat(200, false));
    let mut config = config(app.path());
    config
        .env
        .insert("PUBLIC_API_URL".into(), "https://api.example.test".into());
    config
        .env
        .insert("OPENAI_API_KEY".into(), "sk-secret".into());
    config.public_env = vec!["PUBLIC_API_URL".into(), "OPENAI_API_KEY".into()];

    let asset = get(&config, "GET", "/assets/app-AbCd1234.js").await;
    assert_eq!(asset.status, 200);
    assert_eq!(body_text(&asset), ASSET_JS);
    assert_eq!(
        header(&asset, "content-type"),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(
        header(&asset, "cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert!(header(&asset, "etag").is_some());

    let plain = get(&config, "GET", "/plain.txt").await;
    assert_eq!(body_text(&plain), "plain");

    for uri in [
        "/",
        "/index.html",
        "/deep/client/route",
        "/sub",
        "/assets",
        "/missing.js",
    ] {
        let page = get(&config, "GET", uri).await;
        assert_eq!(page.status, 200, "{uri}");
        let html = body_text(&page);
        assert!(html.contains("SPA-SHELL"), "{uri}");
        assert!(html.contains(r#"<base href="/app/" />"#), "{uri}: {html}");
        assert!(!html.contains(r#"<base href="/old/" />"#), "{uri}");
        assert!(html.contains(r#""PUBLIC_API_URL":"https://api.example.test""#));
        assert!(!html.contains("sk-secret") && !html.contains("OPENAI_API_KEY"));
        assert_eq!(header(&page, "cache-control"), Some("no-cache"));
    }

    let well_known = get(&config, "GET", "/.well-known/ok.txt").await;
    assert_eq!(well_known.status, 200);
    assert_eq!(body_text(&well_known), "well-known");
}

#[tokio::test]
async fn precompressed_variants_negotiate_br_then_gzip_else_identity() {
    let app = build_app(&guest_wat(200, false));
    let config = config(app.path());
    let uri = "/assets/app-AbCd1234.js";
    let with_encoding = |value: &str| {
        let mut req = request("GET", uri);
        req.headers = vec![("Accept-Encoding".into(), value.into())];
        req
    };
    let mut isolate = Spy::default();
    let br = dispatch_fullstack_buffered(&mut isolate, with_encoding("gzip, br"), &config)
        .await
        .unwrap();
    assert_eq!(header(&br, "content-encoding"), Some("br"));
    assert_eq!(br.body.as_deref(), Some(b"BR-BYTES".as_ref()));
    assert_eq!(header(&br, "vary"), Some("accept-encoding"));
    assert_eq!(
        header(&br, "content-type"),
        Some("application/javascript; charset=utf-8")
    );
    let gz = dispatch_fullstack_buffered(&mut isolate, with_encoding("gzip"), &config)
        .await
        .unwrap();
    assert_eq!(header(&gz, "content-encoding"), Some("gzip"));
    assert_eq!(gz.body.as_deref(), Some(b"GZ-BYTES".as_ref()));
    let identity = get(&config, "GET", uri).await;
    assert_eq!(header(&identity, "content-encoding"), None);
    assert_eq!(header(&identity, "vary"), Some("accept-encoding"));
    assert_eq!(body_text(&identity), ASSET_JS);
    assert_eq!(header(&identity, "etag"), header(&br, "etag"));
}

#[tokio::test]
async fn bad_paths_are_400_never_spa_and_never_the_guest() {
    let app = build_app(&guest_wat(200, false));
    let config = config(app.path());
    for uri in [
        "/%61pi/x", // encoded alias of /api/x
        "/%61pi",   // encoded alias of /api
        "/api%2Fx", // encoded slash after the prefix
        "/api%2fx",
        "/%zz",   // malformed escape
        "/a%2",   // truncated escape
        "/%ff",   // invalid UTF-8
        "/a%00b", // NUL
        "/a%5Cb", // backslash (encoded)
        "/a\\b",  // backslash (raw)
        "/%2e%2e/edger.yaml",
        "/..%2fedger.yaml",
        "/assets/%2e%2e/%2e%2e/edger.yaml",
        "/a/../edger.yaml",
        "/../edger.yaml",
        "https://h.test/%61pi/x",
        "rel/path", // not absolute
    ] {
        let res = get(&config, "GET", uri).await;
        assert_eq!(res.status, 400, "{uri}");
        let text = body_text(&res);
        assert!(
            !text.contains("SPA-SHELL") && !text.contains(SECRET),
            "{uri}"
        );
    }
    // Same refusal for non-GET: the 400 comes before the 405.
    assert_eq!(get(&config, "POST", "/%61pi/x").await.status, 400);
}

#[tokio::test]
async fn dotfiles_are_refused_but_well_known_first_segment_is_allowed() {
    let app = build_app(&guest_wat(200, false));
    let config = config(app.path());
    for uri in [
        "/.hidden",
        "/%2ehidden",
        "/sub/.secret",
        "/sub/%2Esecret",
        "/sub/.well-known/x",
    ] {
        let res = get(&config, "GET", uri).await;
        assert_eq!(res.status, 404, "{uri}");
        assert!(!body_text(&res).contains(SECRET), "{uri}");
    }
    // `..` is refused before the dotfile rule.
    assert_eq!(
        get(&config, "GET", "/.well-known/../.hidden").await.status,
        400
    );
    assert_eq!(get(&config, "GET", "/.well-known/ok.txt").await.status, 200);
}

#[tokio::test]
async fn non_api_methods_are_405_with_allow_and_head_has_no_body() {
    let app = build_app(&guest_wat(200, false));
    let config = config(app.path());
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        for uri in ["/", "/assets/app-AbCd1234.js", "/apix", "/apix/y"] {
            let res = get(&config, method, uri).await;
            assert_eq!(res.status, 405, "{method} {uri}");
            assert_eq!(header(&res, "allow"), Some("GET, HEAD"));
        }
    }
    for uri in ["/", "/assets/app-AbCd1234.js", "/deep/route"] {
        let get_res = get(&config, "GET", uri).await;
        let head = get(&config, "HEAD", uri).await;
        assert_eq!(head.status, 200, "{uri}");
        assert!(head.body.is_none(), "{uri}");
        assert_eq!(head.headers, get_res.headers, "{uri}");
    }
}

#[tokio::test]
async fn backend_module_and_manifest_are_not_reachable_through_client_dir() {
    let app = build_app(&guest_wat(200, false));
    let config = config(app.path());
    for uri in ["/backend/index.wasm", "/edger.yaml", "/private/secret.txt"] {
        let res = get(&config, "GET", uri).await;
        assert_eq!(res.status, 200, "{uri} falls back to the SPA shell");
        assert!(body_text(&res).contains("SPA-SHELL"), "{uri}");
    }
    // A backend module placed INSIDE clientDir is refused, not published.
    fs::copy(
        app.path().join("backend/index.wasm"),
        app.path().join("dist/api.wasm"),
    )
    .unwrap();
    let mut manifest = manifest_with("/api", "dist");
    manifest.wasm_backend.as_mut().unwrap().entrypoint = Some("dist/api.wasm".into());
    let mut inside = parse_worker_config(&manifest);
    inside.worker_dir = Some(app.path().to_path_buf());
    assert_eq!(get(&inside, "GET", "/api.wasm").await.status, 404);
    // Control: the same file is served when it is not the backend entrypoint.
    assert_eq!(get(&config, "GET", "/api.wasm").await.status, 200);
}

#[tokio::test]
async fn client_dir_equal_or_outside_worker_dir_is_an_error() {
    let app = build_app(&guest_wat(200, false));
    let mut isolate = Spy::default();
    for client_dir in [".", "../outside", "/abs", "dist/.."] {
        let config = config_for(app.path(), "/api", client_dir);
        let err = dispatch_fullstack_buffered(&mut isolate, request("GET", "/"), &config)
            .await
            .unwrap_err();
        assert!(
            err.code == "FULLSTACK_CLIENT_DIR_DENIED" || err.code == "FULLSTACK_CLIENT_DIR_INVALID",
            "{client_dir}: {}",
            err.code
        );
        assert!(!err.message.contains(SECRET));
    }
}

#[cfg(unix)]
mod symlinks {
    use super::*;
    use std::os::unix::fs::symlink;

    #[tokio::test]
    async fn asset_symlinks_outside_client_root_or_to_dotfiles_are_refused() {
        let app = build_app(&guest_wat(200, false));
        let root = app.path();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("o.txt"), SECRET).unwrap();
        symlink(
            root.join("private/secret.txt"),
            root.join("dist/in-app.txt"),
        )
        .unwrap();
        symlink(outside.path().join("o.txt"), root.join("dist/out.txt")).unwrap();
        symlink(root.join("private"), root.join("dist/linkdir")).unwrap();
        symlink(root.join("dist/.hidden"), root.join("dist/shown.txt")).unwrap();
        symlink(root.join("dist/plain.txt"), root.join("dist/alias.txt")).unwrap();
        let config = config(root);
        for uri in [
            "/in-app.txt",
            "/out.txt",
            "/linkdir/secret.txt",
            "/shown.txt",
        ] {
            let res = get(&config, "GET", uri).await;
            assert_eq!(res.status, 404, "{uri}");
            assert!(!body_text(&res).contains(SECRET), "{uri}");
        }
        // Control: a symlink that stays inside clientDir is served.
        let alias = get(&config, "GET", "/alias.txt").await;
        assert_eq!(alias.status, 200);
        assert_eq!(body_text(&alias), "plain");
    }

    #[tokio::test]
    async fn index_symlink_to_a_private_dir_in_the_app_cannot_swap_the_root() {
        let app = build_app(&guest_wat(200, false));
        let root = app.path();
        fs::remove_file(root.join("dist/index.html")).unwrap();
        symlink(
            root.join("private/index.html"),
            root.join("dist/index.html"),
        )
        .unwrap();
        let config = config(root);
        let mut isolate = Spy::default();
        for uri in ["/", "/secret.txt", "/anything"] {
            let err = dispatch_fullstack_buffered(&mut isolate, request("GET", uri), &config)
                .await
                .unwrap_err();
            assert_eq!(err.code, "FULLSTACK_INDEX_DENIED", "{uri}");
            assert!(!err.message.contains(SECRET));
        }
        assert_eq!(isolate.total_calls(), 0);
    }

    #[tokio::test]
    async fn index_symlink_to_a_hidden_file_in_client_dir_is_refused_for_every_route() {
        let app = build_app(&guest_wat(200, false));
        let root = app.path();
        fs::remove_file(root.join("dist/index.html")).unwrap();
        symlink(root.join("dist/.hidden"), root.join("dist/index.html")).unwrap();
        let config = config(root);
        let mut isolate = Spy::default();
        for uri in ["/", "/missing-route"] {
            let err = dispatch_fullstack_buffered(&mut isolate, request("GET", uri), &config)
                .await
                .unwrap_err();
            assert_eq!(err.code, "FULLSTACK_INDEX_DENIED", "{uri}");
            assert!(!err.message.contains(SECRET), "{uri}");
        }
        assert_eq!(isolate.total_calls(), 0);
    }

    #[tokio::test]
    async fn index_symlink_to_the_backend_module_in_client_dir_is_refused_for_every_route() {
        let app = build_app(&guest_wat(200, false));
        let root = app.path();
        // Backend entrypoint lives inside the public root in this layout.
        fs::write(
            root.join("dist/backend.wasm"),
            wat::parse_str(guest_wat(200, false)).unwrap(),
        )
        .unwrap();
        fs::remove_file(root.join("dist/index.html")).unwrap();
        symlink(root.join("dist/backend.wasm"), root.join("dist/index.html")).unwrap();
        let mut manifest = manifest_with("/api", "dist");
        manifest.wasm_backend.as_mut().unwrap().entrypoint = Some("dist/backend.wasm".into());
        let mut config = parse_worker_config(&manifest);
        config.worker_dir = Some(root.to_path_buf());
        let mut isolate = Spy::default();
        for uri in ["/", "/missing-route"] {
            let err = dispatch_fullstack_buffered(&mut isolate, request("GET", uri), &config)
                .await
                .unwrap_err();
            assert_eq!(err.code, "FULLSTACK_INDEX_DENIED", "{uri}");
            assert!(!err.message.contains("asm"), "{uri}");
        }
        // The API does not depend on a valid index.
        let api = dispatch_fullstack_buffered(&mut isolate, request("GET", "/api/x"), &config)
            .await
            .unwrap();
        assert_eq!(body_text(&api), "spy-wasm");
        assert_eq!(isolate.wasm_calls(), 1);
    }

    #[tokio::test]
    async fn index_symlink_to_a_normal_public_file_in_client_dir_still_serves() {
        let app = build_app(&guest_wat(200, false));
        let root = app.path();
        fs::write(root.join("dist/public.html"), "<html>PUBLIC-INDEX</html>").unwrap();
        fs::remove_file(root.join("dist/index.html")).unwrap();
        symlink(root.join("dist/public.html"), root.join("dist/index.html")).unwrap();
        let config = config(root);
        for uri in ["/", "/missing-route"] {
            let res = get(&config, "GET", uri).await;
            assert_eq!(res.status, 200, "{uri}");
            assert!(body_text(&res).contains("PUBLIC-INDEX"), "{uri}");
        }
    }

    #[tokio::test]
    async fn client_dir_symlink_leaving_worker_dir_is_refused() {
        let app = build_app(&guest_wat(200, false));
        let root = app.path();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("index.html"), SECRET).unwrap();
        symlink(outside.path(), root.join("linked-dist")).unwrap();
        let config = config_for(root, "/api", "linked-dist");
        let mut isolate = Spy::default();
        let err = dispatch_fullstack_buffered(&mut isolate, request("GET", "/"), &config)
            .await
            .unwrap_err();
        assert_eq!(err.code, "FULLSTACK_CLIENT_DIR_DENIED");
    }
}

// ---------------------------------------------------------------------------
// Real WASM guest through WasmIsolate
// ---------------------------------------------------------------------------

#[cfg(feature = "wasm")]
mod real_wasm {
    use super::*;
    use edger_isolation::{WasiConfig, WasmIsolate};

    fn isolate() -> WasmIsolate {
        WasmIsolate::new(WasiConfig::deny_all())
    }

    async fn call(
        config: &WorkerConfig,
        mut req: SerializedRequest,
    ) -> Result<SerializedResponse, IsolationError> {
        req.request_id = "real".into();
        dispatch_fullstack_buffered(&mut isolate(), req, config).await
    }

    #[tokio::test]
    async fn api_strips_prefix_and_preserves_request_fields() {
        let app = build_app(&guest_wat(200, false));
        let config = config(app.path());
        let mut req = request("POST", "/api/inspect?x=1&y=%20");
        req.headers = vec![("x-test".into(), "kept".into())];
        req.body = Some(Bytes::from_static(b"payload-bytes"));
        let res = call(&config, req).await.unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(header(&res, "x-guest"), Some("1"));
        let echo = parse_echo(&res);
        assert_eq!(echo.method, "POST");
        assert_eq!(echo.uri, "/inspect?x=1&y=%20");
        assert!(
            echo.headers.contains(r#"["x-test","kept"]"#),
            "{}",
            echo.headers
        );
        assert_eq!(echo.body, b"payload-bytes");
    }

    #[tokio::test]
    async fn api_root_prefix_becomes_slash_and_keeps_query() {
        let app = build_app(&guest_wat(200, false));
        let config = config(app.path());
        for (uri, want) in [
            ("/api", "/"),
            ("/api/", "/"),
            ("/api?z=1", "/?z=1"),
            ("/api/?z=1", "/?z=1"),
        ] {
            let echo = parse_echo(&call(&config, request("GET", uri)).await.unwrap());
            assert_eq!(echo.uri, want, "{uri}");
        }
    }

    #[tokio::test]
    async fn escaped_suffix_and_query_stay_raw() {
        let app = build_app(&guest_wat(200, false));
        let config = config(app.path());
        let echo = parse_echo(
            &call(&config, request("GET", "/api/inspect/a%2Fb?x=%20"))
                .await
                .unwrap(),
        );
        assert_eq!(echo.uri, "/inspect/a%2Fb?x=%20");
        // An encoded dot-dot after the prefix is forwarded raw to the guest
        // (it never touches the filesystem on the host).
        let echo = parse_echo(
            &call(&config, request("GET", "/api/%2e%2e/x"))
                .await
                .unwrap(),
        );
        assert_eq!(echo.uri, "/%2e%2e/x");
    }

    #[tokio::test]
    async fn absolute_uri_keeps_authority_and_query() {
        let app = build_app(&guest_wat(200, false));
        let config = config(app.path());
        for (uri, want) in [
            (
                "https://app.example.test/api/inspect?q=1",
                "https://app.example.test/inspect?q=1",
            ),
            ("https://app.example.test/api", "https://app.example.test/"),
            (
                "https://app.example.test:8443/api/a%2Fb",
                "https://app.example.test:8443/a%2Fb",
            ),
        ] {
            let echo = parse_echo(&call(&config, request("GET", uri)).await.unwrap());
            assert_eq!(echo.uri, want, "{uri}");
        }
        // Static path of an absolute uri is the SPA, not the guest.
        let page = call(&config, request("GET", "https://app.example.test/deep"))
            .await
            .unwrap();
        assert!(body_text(&page).contains("SPA-SHELL"));
    }

    #[tokio::test]
    async fn custom_prefix_and_per_segment_match() {
        let app = build_app(&guest_wat(200, false));
        let custom = config_for(app.path(), "/rpc/v1", "dist");
        let echo = parse_echo(
            &call(&custom, request("GET", "/rpc/v1/inspect?a=b"))
                .await
                .unwrap(),
        );
        assert_eq!(echo.uri, "/inspect?a=b");
        // `/api` is plain static content under this prefix; `/rpc/v1x` too.
        for uri in ["/api/inspect", "/rpc/v1x", "/rpc", "/rpc/v10"] {
            let page = call(&custom, request("GET", uri)).await.unwrap();
            assert!(body_text(&page).contains("SPA-SHELL"), "{uri}");
        }
        // The encoded alias of the CUSTOM prefix is refused.
        for uri in ["/rpc%2Fv1/x", "/%72pc/v1/x"] {
            assert_eq!(
                call(&custom, request("GET", uri)).await.unwrap().status,
                400,
                "{uri}"
            );
        }
        // Default prefix: `/apix` is the SPA.
        let default = config(app.path());
        for uri in ["/apix", "/apix/y", "/ap"] {
            let page = call(&default, request("GET", uri)).await.unwrap();
            assert!(body_text(&page).contains("SPA-SHELL"), "{uri}");
        }
    }

    #[tokio::test]
    async fn api_methods_other_than_get_head_reach_the_guest() {
        let app = build_app(&guest_wat(200, false));
        let config = config(app.path());
        for method in ["POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
            let echo = parse_echo(&call(&config, request(method, "/api/m")).await.unwrap());
            assert_eq!(echo.method, method);
        }
    }

    #[tokio::test]
    async fn api_404_and_guest_trap_never_fall_back_to_the_spa() {
        let not_found = build_app(&guest_wat(404, false));
        let config404 = config(not_found.path());
        let res = call(&config404, request("GET", "/api/missing"))
            .await
            .unwrap();
        assert_eq!(res.status, 404);
        assert!(!body_text(&res).contains("SPA-SHELL"));
        assert!(!body_text(&res).to_ascii_lowercase().contains("<html"));
        assert_eq!(parse_echo(&res).uri, "/missing");

        let trapping = build_app(&guest_wat(200, true));
        let config_trap = config(trapping.path());
        let err = call(&config_trap, request("GET", "/api/boom"))
            .await
            .unwrap_err();
        assert!(!err.message.contains("SPA-SHELL"));
        // Control: the same app still serves the SPA outside the prefix.
        let page = call(&config_trap, request("GET", "/boom")).await.unwrap();
        assert!(body_text(&page).contains("SPA-SHELL"));
        // The streaming entry returns the same buffered 404.
        let streamed =
            dispatch_fullstack_stream(&mut isolate(), request("GET", "/api/x"), &config404)
                .await
                .unwrap();
        assert!(matches!(streamed, WorkerResponse::Buffered(r) if r.status == 404));
    }

    #[tokio::test]
    async fn missing_backend_module_fails_the_api_only() {
        let app = build_app(&guest_wat(200, false));
        fs::remove_file(app.path().join("backend/index.wasm")).unwrap();
        let config = config(app.path());
        assert!(call(&config, request("GET", "/api/x")).await.is_err());
        let page = call(&config, request("GET", "/")).await.unwrap();
        assert_eq!(page.status, 200);
        assert!(body_text(&page).contains("SPA-SHELL"));
    }
}
