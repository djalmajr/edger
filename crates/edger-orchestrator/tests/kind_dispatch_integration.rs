//! ExecutionKind dispatch integration (story 07.01 / 07.05).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::{
    ExecutionKind, Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig,
};
use edger_isolation::{DenoFacade, DenoIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct RuntimeFactory;

impl IsolateFactory for RuntimeFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(DenoIsolate::new(DenoFacade::new())),
        }
    }
}

struct FullstackProbeFactory;

impl IsolateFactory for FullstackProbeFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(FullstackProbeIsolate)
    }
}

struct FullstackProbeIsolate;

#[async_trait]
impl Isolate for FullstackProbeIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let x_base = req
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-base"))
            .map(|(_, value)| value.as_str())
            .unwrap_or("");
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: Some(bytes::Bytes::from(format!(
                "ssr uri={} entrypoint={} x-base={}",
                req.uri,
                config.entrypoint.as_deref().unwrap_or(""),
                x_base
            ))),
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new(
            "UNEXPECTED_SPA",
            "unexpected SPA dispatch",
        ))
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new(
            "UNEXPECTED_WASM",
            "unexpected Wasm dispatch",
        ))
    }
}

fn state_with_workers(root: std::path::PathBuf) -> OrchestratorState {
    state_with_factory(root, Arc::new(RuntimeFactory))
}

fn state_with_factory(
    root: std::path::PathBuf,
    factory: Arc<dyn IsolateFactory>,
) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), factory);
    server.mark_ready(pool.clone());

    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

async fn dispatch(
    app: Router,
    method: &str,
    uri: &str,
    body: impl Into<Body>,
) -> (StatusCode, bytes::Bytes) {
    let res = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .header("content-type", "application/json")
                .body(body.into())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body)
}

fn static_wasm_response_wat(body: &str) -> String {
    let body_len = body.len();
    let frame_len = 12 + 2 + body_len;

    format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 64) "[]")
  (data (i32.const 96) "{body}")

  (func (export "edger_alloc") (param $len i32) (result i32)
    i32.const 1024
  )

  (func $copy (param $dst i32) (param $src i32) (param $len i32)
    (local $i i32)
    loop $copy_loop
      local.get $i
      local.get $len
      i32.lt_u
      if
        local.get $dst
        local.get $i
        i32.add
        local.get $src
        local.get $i
        i32.add
        i32.load8_u
        i32.store8
        local.get $i
        i32.const 1
        i32.add
        local.set $i
        br $copy_loop
      end
    end
  )

  (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
    i32.const 512
    i32.const 200
    i32.store16
    i32.const 516
    i32.const 2
    i32.store
    i32.const 520
    i32.const {body_len}
    i32.store
    i32.const 524
    i32.const 64
    i32.const 2
    call $copy
    i32.const 526
    i32.const 96
    i32.const {body_len}
    call $copy

    i32.const 512
    i64.extend_i32_u
    i64.const {frame_len}
    i64.const 32
    i64.shl
    i64.or
  )
)"#
    )
}

#[tokio::test]
async fn js_worker_dispatches_through_deno_backend() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("hello-world");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: hello-world
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r#"Deno.serve(async (req: Request) => {
  const payload = await req.json();
  return new Response(JSON.stringify({ message: `Hello ${payload.name}` }), {
    headers: { "content-type": "application/json" },
  });
});
"#,
    )
    .unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/hello-world")
                .header("authorization", "Bearer test-root")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"Alice"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), br#"{"message":"Hello Alice"}"#);
}

#[tokio::test]
async fn deno_backend_loads_worker_deno_config_import_map() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("config-worker");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: config-worker
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("deno.json"),
        r##"{
  "imports": {
    "#message": "./message.ts"
  }
}
"##,
    )
    .unwrap();
    fs::write(
        worker_dir.join("message.ts"),
        r#"export const message = "from-import-map";"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r##"import { message } from "#message";

Deno.serve(() => new Response(message));
"##,
    )
    .unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (status, body) = dispatch(app, "GET", "/config-worker", Body::empty()).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"from-import-map");
}

#[tokio::test]
async fn deno_backend_injects_all_manifest_env() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("env-worker");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: env-worker
version: "1.0.0"
entrypoint: index.ts
kind: fetch
env:
  PUBLIC_FLAG: visible
  DATABASE_URL: postgres://secret
  OPENAI_API_KEY: sk-secret
  GITHUB_TOKEN: gh-secret
  SERVICE_KEY: service-secret
  ADMIN_PASSWORD: password-secret
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r#"Deno.serve(() => {
  const body = {
    publicFlag: Deno.env.get("PUBLIC_FLAG") ?? null,
    databaseUrl: Deno.env.get("DATABASE_URL") ?? null,
    openaiApiKey: Deno.env.get("OPENAI_API_KEY") ?? null,
    githubToken: Deno.env.get("GITHUB_TOKEN") ?? null,
    serviceKey: Deno.env.get("SERVICE_KEY") ?? null,
    adminPassword: Deno.env.get("ADMIN_PASSWORD") ?? null,
  };
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
  });
});
"#,
    )
    .unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (status, body) = dispatch(app, "GET", "/env-worker", Body::empty()).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Server workers receive ALL declared env, including secrets (B1). Browser
    // exposure stays gated separately by the publicEnv allowlist (static_spa).
    assert_eq!(json["publicFlag"], "visible");
    assert_eq!(json["databaseUrl"], "postgres://secret");
    assert_eq!(json["openaiApiKey"], "sk-secret");
    assert_eq!(json["githubToken"], "gh-secret");
    assert_eq!(json["serviceKey"], "service-secret");
    assert_eq!(json["adminPassword"], "password-secret");
}

#[tokio::test]
async fn commonjs_server_listen_worker_dispatches_through_node_adapter() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("commonjs-node");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: commonjs-node
version: "1.0.0"
entrypoint: index.js
kind: fetch
"#,
    )
    .unwrap();
    fs::write(worker_dir.join("package.json"), r#"{"type":"commonjs"}"#).unwrap();
    fs::write(
        worker_dir.join("index.js"),
        r#"const http = require("http");

const server = http.createServer((req, res) => {
  res.writeHead(200, { "content-type": "text/plain" });
  res.write(`CommonJS ${req.method} ${req.url} base=${req.headers["x-base"]}`);
  res.end();
});

server.listen(8080);
"#,
    )
    .unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (status, body) = dispatch(app, "GET", "/commonjs-node/hello", Body::empty()).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"CommonJS GET /hello base=/commonjs-node");
}

#[tokio::test]
async fn namespaced_worker_receives_relative_path_and_base_header() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("team-checkout");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: "@team/checkout"
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r#"Deno.serve((req: Request) => {
  const url = new URL(req.url);
  return new Response(`${url.pathname} base=${req.headers.get("x-base")}`);
});
"#,
    )
    .unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (status, body) = dispatch(app, "GET", "/@team/checkout/api/ping", Body::empty()).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"/api/ping base=/@team/checkout");
}

#[tokio::test]
async fn static_spa_serves_index_assets_and_fallback_through_rust_pipeline() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("todos");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: todos
version: "1.0.0"
entrypoint: index.html
injectBase: true
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.html"),
        r#"<!doctype html><html><head></head><body><div id="root"></div></body></html>"#,
    )
    .unwrap();
    fs::write(worker_dir.join("index.css"), "body{color:red}").unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (status, body) = dispatch(app.clone(), "GET", "/todos", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(String::from_utf8_lossy(&body).contains(r#"<base href="/todos/" />"#));

    let (status, body) = dispatch(app.clone(), "GET", "/todos/index.css", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"body{color:red}");

    let (status, body) = dispatch(app, "GET", "/todos/filter/active", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(String::from_utf8_lossy(&body).contains(r#"<div id="root"></div>"#));
}

#[tokio::test]
async fn repository_js_examples_dispatch_through_deno_backend() {
    let workers_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("workers/examples");
    let app = build_pipeline(state_with_workers(workers_root));

    let (status, body) = dispatch(
        app.clone(),
        "POST",
        "/hello-world",
        Body::from(r#"{"name":"Alice"}"#),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), br#"{"message":"Hello Alice from foo!"}"#);

    let (status, body) = dispatch(app.clone(), "POST", "/read-body", Body::from("12345")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), br#"{"totalSize":5}"#);

    let (status, body) = dispatch(app.clone(), "GET", "/empty-response", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(body.is_empty());

    let (status, body) = dispatch(
        app.clone(),
        "GET",
        "/serve-declarative-style",
        Body::empty(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"Hello, world");

    let (status, body) = dispatch(app.clone(), "GET", "/chunked-text", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"meow");

    let (status, body) = dispatch(app.clone(), "GET", "/stream", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"Hello, World!\n");

    let (status, body) = dispatch(app.clone(), "GET", "/sse", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"data: hella\r\n\r\n");

    let (status, body) = dispatch(app, "GET", "/serve-html/foo", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(String::from_utf8_lossy(&body).contains("<h1>Foo</h1>"));
}

#[tokio::test]
async fn deno_backend_times_out_hanging_streams() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("never-ending");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: never-ending
version: "1.0.0"
entrypoint: index.ts
kind: fetch
timeout: 100ms
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r#"Deno.serve(() => {
  const encoder = new TextEncoder();
  const stream = new ReadableStream({
    start(controller) {
      setInterval(() => controller.enqueue(encoder.encode("tick")), 1000);
    },
  });
  return new Response(stream);
});
"#,
    )
    .unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (status, body) = dispatch(app, "GET", "/never-ending", Body::empty()).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        String::from_utf8_lossy(&body).contains("DENO_TIMEOUT"),
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
}

#[tokio::test]
async fn wasm_worker_dispatches_through_real_backend() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("wasm-hello");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: wasm-hello
version: "1.0.0"
entrypoint: index.wat
kind: wasm
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.wat"),
        static_wasm_response_wat("wasm-hello"),
    )
    .unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/wasm-hello")
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), b"wasm-hello");
}

#[tokio::test]
async fn same_process_serves_deno_and_wasm_workers_from_one_pool() {
    let root = tempfile::tempdir().unwrap();
    let js_dir = root.path().join("js-hello");
    let wasm_dir = root.path().join("wasm-hello");
    fs::create_dir_all(&js_dir).unwrap();
    fs::create_dir_all(&wasm_dir).unwrap();
    fs::write(
        js_dir.join("manifest.yaml"),
        r#"name: js-hello
version: "1.0.0"
entrypoint: index.ts
kind: fetch
"#,
    )
    .unwrap();
    fs::write(
        js_dir.join("index.ts"),
        r#"Deno.serve(() => new Response("js-ok", {
  headers: { "content-type": "text/plain" },
}));
"#,
    )
    .unwrap();
    fs::write(
        wasm_dir.join("manifest.yaml"),
        r#"name: wasm-hello
version: "1.0.0"
entrypoint: index.wat
kind: wasm
"#,
    )
    .unwrap();
    fs::write(
        wasm_dir.join("index.wat"),
        static_wasm_response_wat("wasm-ok"),
    )
    .unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));
    let (js_status, js_body) = dispatch(app.clone(), "GET", "/js-hello", Body::empty()).await;
    let (wasm_status, wasm_body) = dispatch(app, "GET", "/wasm-hello", Body::empty()).await;

    assert_eq!(js_status, StatusCode::OK);
    assert_eq!(js_body.as_ref(), b"js-ok");
    assert_eq!(wasm_status, StatusCode::OK);
    assert_eq!(wasm_body.as_ref(), b"wasm-ok");
}

// Mutation captured: reverting the bridge routes dispatch to plain
// `execute_fetch` (ignoring the `routes` export) sends every request to the
// fallback handler and the exact/param/method assertions below go red.
#[tokio::test]
async fn routes_table_worker_dispatches_by_path_method_and_params() {
    let workers_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("workers/examples");
    let app = build_pipeline(state_with_workers(workers_root));

    let (status, body) =
        dispatch(app.clone(), "GET", "/routes-demo/api/status", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), br#"{"ok":true}"#);

    let (status, body) = dispatch(app.clone(), "GET", "/routes-demo/users/42", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), br#"{"user":"42"}"#);

    let (status, body) = dispatch(app.clone(), "GET", "/routes-demo/admin", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), b"admin-get");

    let (status, _body) = dispatch(app.clone(), "POST", "/routes-demo/admin", Body::empty()).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);

    let (status, body) = dispatch(
        app.clone(),
        "GET",
        "/routes-demo/files/deep/nested.txt",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), b"wildcard");

    let (status, body) = dispatch(app, "GET", "/routes-demo/unmatched", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_ref(), b"fallback");
}

// Mutation captured: dropping the 404 branch for routes-only modules (no
// `fetch` fallback) turns unmatched paths into a handler-missing crash and
// this test goes red.
#[tokio::test]
async fn routes_table_without_fallback_returns_404_for_unmatched() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("routes-only");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: routes-only
version: "1.0.0"
entrypoint: index.ts
kind: routes
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r#"export default {
  routes: {
    "/ping": () => new Response("pong"),
  },
};
"#,
    )
    .unwrap();

    let app = build_pipeline(state_with_workers(root.path().to_path_buf()));

    let (status, body) = dispatch(app.clone(), "GET", "/routes-only/ping", Body::empty()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(body.as_ref(), b"pong");

    let (status, _body) = dispatch(app, "GET", "/routes-only/missing", Body::empty()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// Mutation captured: accepting a fullstack manifest without an adapter lets the
// runtime reach a generic dispatch error instead of a clear config failure.
#[tokio::test]
async fn fullstack_worker_without_adapter_fails_manifest_validation() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("ssr-app");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: ssr-app
version: "1.0.0"
kind: fullstack
ssrEntrypoint: index.ts
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r#"Deno.serve(() => new Response("ssr"));"#,
    )
    .unwrap();

    let err = load_manifests_from_dirs(&[root.path().to_path_buf()]).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
    assert!(err.message.contains("manifest.adapter"));
}

#[tokio::test]
async fn fullstack_worker_with_invalid_adapter_fails_manifest_validation() {
    let root = tempfile::tempdir().unwrap();
    let worker_dir = root.path().join("ssr-app");
    fs::create_dir_all(&worker_dir).unwrap();
    fs::write(
        worker_dir.join("manifest.yaml"),
        r#"name: ssr-app
version: "1.0.0"
kind: fullstack
adapter: next
ssrEntrypoint: index.ts
"#,
    )
    .unwrap();
    fs::write(
        worker_dir.join("index.ts"),
        r#"Deno.serve(() => new Response("ssr"));"#,
    )
    .unwrap();

    let err = load_manifests_from_dirs(&[root.path().to_path_buf()]).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
    assert!(err.message.contains("unsupported adapter"));
}

fn workers_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("workers/examples")
}

// Mutation captured: dropping fullstack base restoration sends `/` to the
// TanStack server bundle and this request body loses `/tanstack-demo/`.
#[tokio::test]
async fn tanstack_fullstack_worker_dispatches_ssr_with_base_restored() {
    let app = build_pipeline(state_with_factory(
        workers_root(),
        Arc::new(FullstackProbeFactory),
    ));

    let (status, body) = dispatch(app, "GET", "/tanstack-demo", Body::empty()).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected body: {}",
        String::from_utf8_lossy(&body)
    );
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("uri=/tanstack-demo/"), "body: {text}");
    assert!(text.contains("entrypoint=server/server.js"), "body: {text}");
    assert!(text.contains("x-base=/tanstack-demo"), "body: {text}");
}

// Mutation captured: routing TanStack assets through SSR instead of the Rust
// clientDir preflight returns the probe body instead of the built CSS.
#[tokio::test]
async fn tanstack_fullstack_worker_serves_client_asset() {
    let app = build_pipeline(state_with_factory(
        workers_root(),
        Arc::new(FullstackProbeFactory),
    ));

    let (status, body) = dispatch(
        app,
        "GET",
        "/tanstack-demo/assets/styles-DLLFwaUX.css",
        Body::empty(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("@layer"), "body: {text}");
    assert!(!text.contains("ssr uri="), "asset hit SSR: {text}");
}

// Mutation captured: removing decoded path validation lets encoded traversal
// reach files outside clientDir or treats malformed escapes as normal SSR.
#[tokio::test]
async fn tanstack_fullstack_worker_rejects_bad_asset_paths() {
    let app = build_pipeline(state_with_factory(
        workers_root(),
        Arc::new(FullstackProbeFactory),
    ));

    let (status, _body) = dispatch(
        app.clone(),
        "GET",
        "/tanstack-demo/assets/%2e%2e/server/server.js",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _body) =
        dispatch(app, "GET", "/tanstack-demo/assets/%E0%A4%A", Body::empty()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
