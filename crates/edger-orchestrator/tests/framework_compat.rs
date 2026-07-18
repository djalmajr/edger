//! Story 15.C: real JS frameworks (Express via node:http, Hono via Deno.serve)
//! run on the persistent Deno process backend — full Deno compat, no reimpl.
//!
//! Requires `deno` on PATH and network access to resolve `npm:` on a cold
//! cache. Ignored by default to keep the core suite hermetic; run explicitly.

use std::fs;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct ProcessFactory;

impl IsolateFactory for ProcessFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(DenoProcessIsolate::new()),
        }
    }
}

fn state(root: std::path::PathBuf) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(ProcessFactory));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

async fn get(app: Router, uri: &str) -> (StatusCode, String) {
    let (status, _, body) = request(app, Method::GET, uri, &[], "").await;
    (status, body)
}

async fn request(
    app: Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if !body.is_empty() {
        request = request.header("content-length", body.len().to_string());
    }
    let res = app
        .oneshot(
            request
                .header("authorization", "Bearer test-root")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn framework_test_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workers/framework-tests")
}

fn worker(root: &std::path::Path, name: &str, index: &str) {
    worker_with_entry(root, name, "index.ts", index, None);
}

fn worker_with_entry(
    root: &std::path::Path,
    name: &str,
    entry: &str,
    index: &str,
    deno_json: Option<&str>,
) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!("name: {name}\nversion: \"1.0.0\"\nentrypoint: {entry}\nkind: fetch\n"),
    )
    .unwrap();
    fs::write(dir.join(entry), index).unwrap();
    if let Some(config) = deno_json {
        fs::write(dir.join("deno.json"), config).unwrap();
    }
}

// Mutation captured: dropping the node:http listener capture in the harness
// leaves Express with no handler — spawn fails / route 500s and this goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno + npm network (cold cache); run explicitly"]
async fn express_and_hono_execute_on_the_process_backend() {
    let root = tempfile::tempdir().unwrap();
    worker(
        root.path(),
        "express-app",
        r#"import express from "npm:express@5";
const app = express();
app.get("/", (_req, res) => res.json({ framework: "express" }));
app.get("/users/:id", (req, res) => res.json({ user: req.params.id }));
app.listen(3000);
"#,
    );
    worker(
        root.path(),
        "hono-app",
        r#"import { Hono } from "npm:hono@4";
const app = new Hono();
app.get("/", (c) => c.json({ framework: "hono" }));
app.get("/users/:id", (c) => c.json({ user: c.req.param("id") }));
Deno.serve(app.fetch);
"#,
    );

    let app = build_pipeline(state(root.path().to_path_buf()));

    // Express via node:http listener capture
    let (status, body) = get(app.clone(), "/express-app").await;
    assert_eq!(status, StatusCode::OK, "express root: {body}");
    assert!(
        body.contains("\"framework\":\"express\""),
        "express: {body}"
    );
    let (status, body) = get(app.clone(), "/express-app/users/7").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"user\":\"7\""), "express param: {body}");

    // Hono via Deno.serve capture (warm process reused)
    let (status, body) = get(app.clone(), "/hono-app").await;
    assert_eq!(status, StatusCode::OK, "hono root: {body}");
    assert!(body.contains("\"framework\":\"hono\""), "hono: {body}");
    let (status, body) = get(app, "/hono-app/users/9").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"user\":\"9\""), "hono param: {body}");
}

// Story 16.A: the fullstack blessed path — Hono SSR + JSX deployed as SOURCE
// (.tsx, no build step; Deno transpiles via deno.json jsxImportSource).
// Mutation captured: dropping the `--config deno.json` pass-through in the
// process spawn breaks the JSX transform (import fails) and this goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno + npm network (cold cache); run explicitly"]
async fn hono_ssr_jsx_renders_html_on_the_server() {
    let root = tempfile::tempdir().unwrap();
    worker_with_entry(
        root.path(),
        "ssr-app",
        "index.tsx",
        r#"import { Hono } from "npm:hono@4";
import { jsxRenderer } from "npm:hono@4/jsx-renderer";

const app = new Hono();
app.use("*", jsxRenderer(({ children }) => (
  <html><body><header>ssr-layout</header>{children}</body></html>
)));
app.get("/", (c) => c.render(<main>rendered-on-server:{String(2 + 3)}</main>));
app.get("/api/info", (c) => c.json({ ssr: "hono/jsx" }));
Deno.serve(app.fetch);
"#,
        Some(
            r#"{
  "compilerOptions": { "jsx": "precompile", "jsxImportSource": "npm:hono@4/jsx" }
}
"#,
        ),
    );

    let app = build_pipeline(state(root.path().to_path_buf()));

    // SSR page: HTML rendered server-side from JSX, dynamic expression evaluated.
    let (status, body) = get(app.clone(), "/ssr-app").await;
    assert_eq!(status, StatusCode::OK, "ssr root: {body}");
    assert!(
        body.contains("<header>ssr-layout</header>"),
        "layout: {body}"
    );
    assert!(body.contains("rendered-on-server:5"), "dynamic jsx: {body}");

    // JSON API served by the SAME worker — the fullstack pair.
    let (status, body) = get(app, "/ssr-app/api/info").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"ssr\":\"hono/jsx\""), "api: {body}");
}

// Story 16.B: the SvelteKit adapter-node pattern — `createServer()` with NO
// argument, the real handler registered later via `server.on("request", h)`,
// PLUS a second tracking listener (adapter-node does both), and a handler that
// requires the Host header to build its origin (SvelteKit's getRequest).
// Mutations captured: keeping only the last "request" listener dispatches to
// the tracker and the process exits cleanly mid-request (event loop drains);
// dropping the Host default makes the origin reconstruction fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno + npm network (cold cache); run explicitly"]
async fn polka_style_on_request_capture_and_host_header() {
    let root = tempfile::tempdir().unwrap();
    worker(
        root.path(),
        "polka-style",
        r#"import http from "node:http";
const server = http.createServer();
let tracked = 0;
server.on("request", (req, _res) => { tracked++; req.on("close", () => {}); });
server.on("request", (req, res) => {
  const host = req.headers.host;
  if (!host) { res.writeHead(400); res.end("no host"); return; }
  const origin = `http://${host}`;
  res.writeHead(200, { "content-type": "application/json" });
  res.end(JSON.stringify({ origin, tracked, url: req.url }));
});
server.listen(3000);
"#,
    );

    let app = build_pipeline(state(root.path().to_path_buf()));

    let (status, body) = get(app.clone(), "/polka-style").await;
    assert_eq!(status, StatusCode::OK, "polka-style: {body}");
    assert!(
        body.contains("\"origin\":\"http://"),
        "host default: {body}"
    );
    assert!(
        body.contains("\"tracked\":1"),
        "all listeners invoked: {body}"
    );

    // Second request proves the process did not exit after the first dispatch.
    let (status, body) = get(app, "/polka-style").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"tracked\":2"), "process survived: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno + npm network (cold cache); run explicitly"]
async fn nestjs_express_and_fastify_cover_enterprise_http_features() {
    let app = build_pipeline(state(framework_test_root()));

    for (name, adapter) in [
        ("nest-express-demo", "express"),
        ("nest-fastify-demo", "fastify"),
    ] {
        let uri = format!("/{name}");
        let (status, headers, body) = request(app.clone(), Method::GET, &uri, &[], "").await;
        assert_eq!(status, StatusCode::OK, "{name} root: {body}");
        assert!(body.contains("\"framework\":\"nestjs\""), "{body}");
        assert!(
            body.contains(&format!("\"adapter\":\"{adapter}\"")),
            "{body}"
        );
        assert_eq!(headers["x-nest-interceptor"], "active");

        let (status, _, body) = request(app.clone(), Method::GET, &uri, &[], "").await;
        assert_eq!(status, StatusCode::OK, "{name} warm root: {body}");
        assert!(
            body.contains("\"count\":2"),
            "provider state was not reused: {body}"
        );

        let guarded = format!("/{name}/guarded");
        let (status, _, _) = request(app.clone(), Method::GET, &guarded, &[], "").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, body) = request(
            app.clone(),
            Method::GET,
            &guarded,
            &[("x-test-auth", "allowed")],
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "guarded {name}: {body}");

        let validate = format!("/{name}/validate");
        let (status, _, body) = request(
            app.clone(),
            Method::POST,
            &validate,
            &[("content-type", "application/json")],
            r#"{"message":"works"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "valid DTO {name}: {body}");
        let (status, _, _) = request(
            app.clone(),
            Method::POST,
            &validate,
            &[("content-type", "application/json")],
            r#"{"message":"x"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let stream = format!("/{name}/stream");
        let (status, _, body) = request(app.clone(), Method::GET, &stream, &[], "").await;
        assert_eq!(status, StatusCode::OK, "stream {name}: {body}");
        assert_eq!(body, format!("nest-{adapter}-stream"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno + npm network (cold cache); run explicitly"]
async fn fastify_and_koa_cover_server_lifecycle_and_middleware() {
    let app = build_pipeline(state(framework_test_root()));

    for (name, framework, marker, stream_body) in [
        (
            "fastify-demo",
            "fastify",
            "x-fastify-hook",
            "fastify-stream",
        ),
        ("koa-demo", "koa", "x-koa-middleware", "koa-stream"),
    ] {
        let uri = format!("/{name}");
        let (status, headers, body) = request(app.clone(), Method::GET, &uri, &[], "").await;
        assert_eq!(status, StatusCode::OK, "{name} root: {body}");
        assert!(
            body.contains(&format!("\"framework\":\"{framework}\"")),
            "{body}"
        );
        assert_eq!(headers[marker], "active");
        if framework == "koa" {
            assert_eq!(headers["x-koa-upstream"], "resumed");
        }

        let (status, _, body) = request(app.clone(), Method::GET, &uri, &[], "").await;
        assert_eq!(status, StatusCode::OK, "{name} warm root: {body}");
        assert!(body.contains("\"requests\":2"), "warm state {name}: {body}");

        let user = format!("/{name}/users/42");
        let (status, _, body) = request(app.clone(), Method::GET, &user, &[], "").await;
        assert_eq!(status, StatusCode::OK, "route param {name}: {body}");
        assert!(body.contains("\"user\":\"42\""), "{body}");

        let validate = format!("/{name}/validate");
        let (status, _, body) = request(
            app.clone(),
            Method::POST,
            &validate,
            &[("content-type", "application/json")],
            r#"{"message":"works"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "valid body {name}: {body}");
        let (status, _, _) = request(
            app.clone(),
            Method::POST,
            &validate,
            &[("content-type", "application/json")],
            r#"{"message":"x"}"#,
        )
        .await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "invalid body {name}: {status}"
        );

        let stream = format!("/{name}/stream");
        let (status, _, body) = request(app.clone(), Method::GET, &stream, &[], "").await;
        assert_eq!(status, StatusCode::OK, "stream {name}: {body}");
        assert_eq!(body, stream_body);
    }
}
