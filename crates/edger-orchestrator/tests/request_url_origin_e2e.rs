//! EDG-33: the worker's `request.url` carries the real origin when the
//! request arrived on the worker's own host (vhost routing).
//!
//! A request routed by vhost — the `Host`/authority matched one of the
//! version's `hosts` — crosses the isolate boundary with an ABSOLUTE URI:
//! `<scheme>://<routed host><path and query>`. Only the process-backend
//! kinds (fetch, routes, fullstack) receive it: the harness passes a
//! scheme-bearing uri through to the worker's `new Request`, and the
//! fullstack glue inserts the base after the authority. Static SPA and
//! WASM keep the worker-relative URI. The routed host is exactly the
//! authority that matched, with the port when the client sent one, and
//! the scheme comes from `X-Forwarded-Proto`: `https` when the first value
//! of the list is `https` (case-insensitive), `http` in every other case
//! (absent, empty or invalid — the worker never sees another scheme).
//! Every other path keeps the relative URI and the harness/bridge
//! `http://edger.local` placeholder: the name route, the `@version` pin,
//! the homepage fallback, the plugin base on an owner-less host, the
//! authenticated control-plane invoke, cron, health check and warmup.
//!
//! End-to-end proof with a real Deno process: the worker echoes
//! `request.url` and `new URL(request.url).origin` — the worker's own view
//! of its address, through the persistent-process harness.
//!
//! Requires `deno` on PATH. Ignored by default; run explicitly:
//! `cargo test -p edger-orchestrator --test request_url_origin_e2e -- --ignored`

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

/// Real Deno processes for process-kind workers (the persistent-process
/// harness is the path under test).
#[derive(Clone)]
struct ProcessFactory {
    /// EDG-32: per-worker Deno cache root inside the test's own tempdir, so
    /// nothing lands in the shared `${TMPDIR}/edger-deno-cache`.
    deno_cache_root: std::path::PathBuf,
}

impl IsolateFactory for ProcessFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(
                DenoProcessIsolate::new()
                    .with_per_worker_deno_cache_root(self.deno_cache_root.clone()),
            ),
        }
    }
}

fn state(root: std::path::PathBuf) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(
        PoolConfig::default(),
        Arc::new(ProcessFactory {
            deno_cache_root: root.join(".edger-test-deno-cache"),
        }),
    );
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

/// Probe worker that echoes `request.url` and its origin: the worker's view
/// of its own address, end to end.
fn write_probe_worker(root: &std::path::Path, name: &str, hosts: &[&str]) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let hosts_yaml = hosts
        .iter()
        .map(|host| format!("  - {host}"))
        .collect::<Vec<_>>()
        .join("\n");
    let hosts_block = if hosts.is_empty() {
        String::new()
    } else {
        format!("hosts:\n{hosts_yaml}\n")
    };
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nvisibility: public\nentrypoint: index.ts\nkind: fetch\n{hosts_block}"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"Deno.serve((req) => {
  const url = new URL(req.url);
  return Response.json({ requestUrl: req.url, origin: url.origin });
});
"#,
    )
    .unwrap();
}

/// Worker with a common CSRF protection: it compares the `Origin` header
/// with the origin of `request.url` and refuses the POST when they differ.
fn write_csrf_worker(root: &std::path::Path, name: &str, hosts: &[&str]) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let hosts_yaml = hosts
        .iter()
        .map(|host| format!("  - {host}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nvisibility: public\nentrypoint: index.ts\nkind: fetch\nhosts:\n{hosts_yaml}\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"Deno.serve((req) => {
  const origin = new URL(req.url).origin;
  const sent = req.headers.get("origin");
  if (req.method === "POST" && sent !== origin) {
    return new Response("origin mismatch", {
      status: 403,
      headers: { "x-origin-sent": sent ?? "none" },
    });
  }
  return Response.json({ accepted: true, origin });
});
"#,
    )
    .unwrap();
}

/// Fullstack worker (tanstack-style uri rewriting) with a FIXED non-root
/// base: the SSR entry echoes `request.url` and `x-base` — the absolute
/// (vhost) uri with the base inserted AFTER the authority, and the
/// worker-relative base.
fn write_fullstack_worker(root: &std::path::Path, name: &str, hosts: &[&str]) {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("client")).unwrap();
    let hosts_yaml = hosts
        .iter()
        .map(|host| format!("  - {host}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nvisibility: public\nkind: fullstack\nadapter: tanstack\nbasePath: /fs\nssrEntrypoint: server.ts\nclientDir: client\nhosts:\n{hosts_yaml}\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("server.ts"),
        r#"export default {
  fetch(req) {
    return Response.json({
      requestUrl: req.url,
      xBase: req.headers.get("x-base"),
    });
  },
};
"#,
    )
    .unwrap();
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = body.map(|b| Body::from(b.to_owned())).unwrap_or_default();
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.clone().oneshot(builder.body(body).unwrap()),
    )
    .await
    .expect("request must finish in time")
    .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn as_json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or(serde_json::Value::Null)
}

// Own host: the worker's `request.url` carries the real origin. The scheme
// comes from the FIRST value of X-Forwarded-Proto (https, case-insensitive);
// any other value — absent, empty, `javascript`, a list starting with
// something else — yields http. A port in the authority is kept.
#[ignore = "needs deno on PATH; run explicitly"]
#[tokio::test]
async fn vhost_request_url_carries_the_real_origin() {
    let root = tempfile::tempdir().unwrap();
    write_probe_worker(root.path(), "origin-app", &["app.example"]);
    let app = build_pipeline(state(root.path().to_path_buf()));

    let (status, text) = send(
        &app,
        "GET",
        "/probe?q=1",
        &[("host", "app.example"), ("x-forwarded-proto", "https")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "https://app.example/probe?q=1");
    assert_eq!(json["origin"], "https://app.example");

    // No X-Forwarded-Proto: http.
    let (status, text) = send(&app, "GET", "/probe?q=1", &[("host", "app.example")], None).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "http://app.example/probe?q=1");
    assert_eq!(json["origin"], "http://app.example");

    // Invalid X-Forwarded-Proto value: http (the worker never sees
    // `javascript:`).
    let (status, text) = send(
        &app,
        "GET",
        "/probe?q=1",
        &[("host", "app.example"), ("x-forwarded-proto", "javascript")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "http://app.example/probe?q=1");
    assert_eq!(json["origin"], "http://app.example");

    // List: the FIRST value decides.
    let (status, text) = send(
        &app,
        "GET",
        "/probe?q=1",
        &[
            ("host", "app.example"),
            ("x-forwarded-proto", "https, http"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "https://app.example/probe?q=1");
    assert_eq!(json["origin"], "https://app.example");

    // Authority with a port: the port is kept in the origin.
    let (status, text) = send(
        &app,
        "GET",
        "/probe?q=1",
        &[("host", "app.example:8443"), ("x-forwarded-proto", "https")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "https://app.example:8443/probe?q=1");
    assert_eq!(json["origin"], "https://app.example:8443");
}

// The reported failure (stg.pinar.dev): a worker that compares `Origin`
// with the origin of `request.url` (a common CSRF protection) accepts the
// browser POST when both arrive on the worker's own host behind an
// HTTPS-terminating proxy.
#[ignore = "needs deno on PATH; run explicitly"]
#[tokio::test]
async fn vhost_origin_allows_a_matching_browser_post() {
    let root = tempfile::tempdir().unwrap();
    write_csrf_worker(root.path(), "shop-app", &["shop.example"]);
    let app = build_pipeline(state(root.path().to_path_buf()));

    let (status, text) = send(
        &app,
        "POST",
        "/submit",
        &[
            ("host", "shop.example"),
            ("x-forwarded-proto", "https"),
            ("origin", "https://shop.example"),
            ("content-type", "application/json"),
        ],
        Some("{}"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the CSRF check must accept the post: {text}"
    );
    let json = as_json(&text);
    assert_eq!(json["accepted"], true);
    assert_eq!(json["origin"], "https://shop.example");

    // Control: a foreign Origin is still refused by the worker's own check
    // (the comparison is real, not a bypass).
    let (status, text) = send(
        &app,
        "POST",
        "/submit",
        &[
            ("host", "shop.example"),
            ("x-forwarded-proto", "https"),
            ("origin", "https://evil.example"),
            ("content-type", "application/json"),
        ],
        Some("{}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(text, "origin mismatch");
}

// (EDG-33 emenda 1) Fullstack via its own host with a FIXED non-root base:
// the SSR `request.url` carries the real origin with the base inserted
// AFTER the authority (`https://fs.example` + `/fs` + `/about?q=1`), and
// `x-base` keeps the worker-relative value.
#[ignore = "needs deno on PATH; run explicitly"]
#[tokio::test]
async fn fullstack_on_its_own_host_keeps_the_origin_behind_the_fixed_base() {
    let root = tempfile::tempdir().unwrap();
    write_fullstack_worker(root.path(), "fs-app", &["fs.example"]);
    let app = build_pipeline(state(root.path().to_path_buf()));

    let (status, text) = send(
        &app,
        "GET",
        "/about?q=1",
        &[("host", "fs.example"), ("x-forwarded-proto", "https")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "https://fs.example/fs/about?q=1");
    assert_eq!(json["xBase"], "/fs");

    // Root request: the base lands right after the authority (the
    // sveltekit/tanstack collapse keeps its trailing slash and the origin).
    let (status, text) = send(
        &app,
        "GET",
        "/",
        &[("host", "fs.example"), ("x-forwarded-proto", "https")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "https://fs.example/fs/");
}

// Controls that do NOT change: the name route on an owner-less host keeps
// the harness placeholder `http://edger.local` (a worker without hosts, and
// a request without any Host header), and the authenticated control-plane
// invoke keeps the placeholder even on a hosted worker.
#[ignore = "needs deno on PATH; run explicitly"]
#[tokio::test]
async fn non_vhost_paths_keep_the_placeholder_origin() {
    let root = tempfile::tempdir().unwrap();
    write_probe_worker(root.path(), "plain-app", &[]);
    write_probe_worker(root.path(), "origin-app", &["app.example"]);
    let app = build_pipeline(state(root.path().to_path_buf()));

    // Name route on an owner-less host: the harness placeholder.
    let (status, text) = send(
        &app,
        "GET",
        "/plain-app/probe?q=1",
        &[("host", "other.example")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "http://edger.local/probe?q=1");
    assert_eq!(json["origin"], "http://edger.local");

    // No Host header at all: the same placeholder.
    let (status, text) = send(&app, "GET", "/plain-app/probe", &[], None).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "http://edger.local/probe");

    // Authenticated control-plane invoke on the HOSTED worker: as today.
    let (status, text) = send(
        &app,
        "POST",
        "/api/admin/workers/origin-app/invoke/probe?q=1",
        &[("x-edger-control-authorization", "Bearer test-root")],
        Some("{}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = as_json(&text);
    assert_eq!(json["requestUrl"], "http://edger.local/probe?q=1");
    assert_eq!(json["origin"], "http://edger.local");
}
