//! EDG-2/EDG-3/EDG-6: gzip + brotli compression of app responses, negotiated
//! via `Accept-Encoding`, through the full pipeline.
//!
//! - only responses marked by `pipeline_handler` (apps) are compressed;
//!   the control plane (`/health`, admin API) passes through untouched;
//! - already-compressed responses (worker `content-encoding`) and
//!   `Cache-Control: no-transform` stay intact;
//! - SSE is never compressed and stays incremental;
//! - compressed delivery is progressive: the first chunk is decodable
//!   before the second one even exists (gzip and brotli);
//! - a worker's strong ETag becomes weak once compression changed the
//!   content-coding; weak ETags, worker-provided encodings and uncompressed
//!   responses stay intact;
//! - cancelling a compressed stream recycles the worker, like an identity
//!   stream;
//! - `Accept-Encoding` that accepts neither `br`, `gzip` nor `identity`
//!   (the tower-http 406 case, RFC 9110 §12.5.3) is answered 406 ONLY for
//!   app responses (same body/headers the layer would pass through); the
//!   control plane returns its normal response, uncompressed, and never
//!   406s (EDG-6);
//! - the compression byte counters (`edger_http_compression_bytes_{in,
//!   out}_total{encoding}`) record the pre/post-compression sizes when the
//!   compressed body ends (EDG-6);
//! - `EDGER_COMPRESSION=off` mounts no layer at all: no `content-encoding`,
//!   no 406, no `Vary` from the layer (EDG-6).
//!
//! Deno-backed tests are ignored by default; run explicitly.

use std::fs;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::IntoResponse;
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, WasmIsolate};
use edger_orchestrator::compression::{
    compression_layer, mark_app_response, mark_worker_without_encoding, weaken_worker_etag,
    CompressionConfig,
};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
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

/// Like [`state`], but with an explicit `CompressionConfig` mounted on the
/// shared server state before `build_pipeline` (EDG-6).
fn state_with_compression(
    root: std::path::PathBuf,
    compression: CompressionConfig,
) -> (
    OrchestratorState,
    edger_orchestrator::metrics::CompressionMetrics,
) {
    let server = ServerState::new_unready();
    server.set_compression_config(compression);
    let metrics = server.compression_metrics();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(ProcessFactory));
    server.mark_ready(pool.clone());
    (
        OrchestratorState {
            server,
            pool,
            index: load_manifests_from_dirs(&[root]).unwrap(),
            auth: ControlAuth::with_static_key("test-root"),
        },
        metrics,
    )
}

/// Write a static SPA app (kind `spa`) with a 2048 B `index.html` and a
/// 2048 B hashed JS asset — the deno-free compressible app fixture.
fn spa_app(root: &std::path::Path) -> (String, String) {
    let dir = root.join("spa-app");
    fs::create_dir_all(dir.join("assets")).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: spa-app\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: spa\n",
    )
    .unwrap();
    let html = format!("<!doctype html><html><body>{}", "h".repeat(2048));
    fs::write(dir.join("index.html"), html.clone()).unwrap();
    let js = format!("// spa bundle\n{}", "s".repeat(2048));
    fs::write(dir.join("assets").join("app-AbCd1234.js"), js.clone()).unwrap();
    (html, js)
}

fn worker(root: &std::path::Path, name: &str, index: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!("name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n"),
    )
    .unwrap();
    fs::write(dir.join("index.ts"), index).unwrap();
}

fn vary_has_accept_encoding(headers: &axum::http::HeaderMap) -> bool {
    headers.get_all(header::VARY).iter().any(|value| {
        value
            .to_str()
            .ok()
            .is_some_and(|value| value.to_ascii_lowercase().contains("accept-encoding"))
    })
}

fn gzip_decode(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = GzDecoder::new(bytes);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .expect("full gzip stream decodes");
    out
}

/// Incrementally decode whatever a (possibly truncated) gzip stream already
/// carries; stops at the first 0/Err, keeping what decoded so far.
fn partial_gzip_prefix(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = GzDecoder::new(std::io::Cursor::new(bytes));
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match decoder.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
    out
}

/// Incrementally decode whatever a (possibly truncated) brotli stream already
/// carries; stops at the first 0/Err, keeping what decoded so far. Works
/// because the encoder flushes a self-contained meta-block after each worker
/// chunk, so the bytes of the first chunk decode on their own.
fn partial_brotli_prefix(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = brotli::Decompressor::new(bytes, 0);
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match decoder.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
    out
}

fn brotli_decode(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = brotli::Decompressor::new(bytes, 0);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .expect("full brotli stream decodes");
    out
}

const BIG_WORKER: &str = r#"Deno.serve(() => {
  const body = "/* edger bundle */\n" + "x".repeat(4096);
  return new Response(body, { headers: { "content-type": "text/javascript" } });
});
"#;

const SMALL_WORKER: &str = r#"Deno.serve(() => new Response("tiny", { headers: { "content-type": "text/plain" } }));
"#;

const NO_TRANSFORM_WORKER: &str = r#"Deno.serve(() => {
  return new Response("y".repeat(4096), { headers: { "content-type": "text/html", "cache-control": "no-transform, max-age=300" } });
});
"#;

const SELF_ENCODED_WORKER: &str = r#"Deno.serve(() => {
  return new Response("z".repeat(4096), { headers: { "content-type": "text/plain", "content-encoding": "gzip" } });
});
"#;

const ETAG_WORKER: &str = r#"Deno.serve(() => {
  const body = "/* edger bundle */\n" + "x".repeat(4096);
  return new Response(body, {
    headers: { "content-type": "text/javascript", "etag": "\"v1\"" },
  });
});
"#;

const INFINITE_HTML_WORKER: &str = r#"Deno.serve(() => {
  let n = 0;
  let id;
  const stream = new ReadableStream({
    start(c) {
      id = setInterval(() => c.enqueue(new TextEncoder().encode(`<p>tick-${n++}</p>`)), 200);
    },
    cancel() { clearInterval(id); },
  });
  return new Response(stream, { headers: { "content-type": "text/html" } });
});
"#;

const SSE_WORKER: &str = r#"Deno.serve(() => {
  let n = 0;
  let id;
  const stream = new ReadableStream({
    start(c) {
      id = setInterval(() => c.enqueue(new TextEncoder().encode(`data: tick-${n++}\n\n`)), 200);
    },
    cancel() { clearInterval(id); },
  });
  return new Response(stream, { headers: { "content-type": "text/event-stream" } });
});
"#;

const PROGRESSIVE_WORKER: &str = r#"Deno.serve(() => {
  const enc = new TextEncoder();
  const part1 = "PART1:" + "a".repeat(2048);
  const part2 = "PART2:" + "b".repeat(2048);
  const stream = new ReadableStream({
    start(c) {
      c.enqueue(enc.encode(part1));
      setTimeout(() => {
        c.enqueue(enc.encode(part2));
        c.close();
      }, 500);
    },
  });
  return new Response(stream, { headers: { "content-type": "text/html" } });
});
"#;

async fn send(app: Router, uri: &str, accept_encoding: Option<&str>) -> axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer test-root");
    if let Some(encoding) = accept_encoding {
        builder = builder.header("accept-encoding", encoding);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// HEAD variant of [`send`], with an optional `If-None-Match`.
async fn head_request(
    app: Router,
    uri: &str,
    accept_encoding: Option<&str>,
    if_none_match: Option<&str>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method("HEAD")
        .uri(uri)
        .header("authorization", "Bearer test-root");
    if let Some(encoding) = accept_encoding {
        builder = builder.header("accept-encoding", encoding);
    }
    if let Some(value) = if_none_match {
        builder = builder.header("if-none-match", value);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// GET variant of the request above, with an optional `If-None-Match`
/// (the 304 case).
async fn get_request(
    app: Router,
    uri: &str,
    accept_encoding: Option<&str>,
    if_none_match: Option<&str>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer test-root");
    if let Some(encoding) = accept_encoding {
        builder = builder.header("accept-encoding", encoding);
    }
    if let Some(value) = if_none_match {
        builder = builder.header("if-none-match", value);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

mod compression {
    use super::*;

    // ---- layer-level (no deno) -------------------------------------------------

    fn marked_app() -> Router {
        Router::new()
            .route(
                "/big",
                axum::routing::get(|| async move {
                    let mut res =
                        ([("content-type", "text/plain")], "l".repeat(4096)).into_response();
                    mark_app_response(&mut res);
                    res
                }),
            )
            .layer(compression_layer())
    }

    #[tokio::test]
    async fn layer_compresses_marked_response() {
        let app = marked_app();
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/big")
                    .header("accept-encoding", "br")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("br")
        );
        assert!(vary_has_accept_encoding(res.headers()));
        assert!(!res.headers().contains_key(header::CONTENT_LENGTH));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(brotli_decode(&bytes), vec![b'l'; 4096]);
    }

    #[tokio::test]
    async fn layer_skips_unmarked_response() {
        let app = Router::new()
            .route(
                "/big",
                axum::routing::get(|| async move {
                    ([("content-type", "text/plain")], "l".repeat(4096))
                }),
            )
            .layer(compression_layer());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/big")
                    .header("accept-encoding", "br")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.to_vec(), vec![b'l'; 4096]);
    }

    #[tokio::test]
    async fn layer_skips_marked_response_with_content_range() {
        let app = Router::new()
            .route(
                "/part",
                axum::routing::get(|| async move {
                    let mut res = (
                        [
                            ("content-type", "text/plain"),
                            ("content-range", "bytes 0-1023/8192"),
                        ],
                        "p".repeat(4096),
                    )
                        .into_response();
                    mark_app_response(&mut res);
                    res
                }),
            )
            .layer(compression_layer());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/part")
                    .header("accept-encoding", "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.to_vec(), vec![b'p'; 4096]);
    }

    #[tokio::test]
    async fn layer_skips_marked_response_with_existing_content_encoding() {
        let app = Router::new()
            .route(
                "/encoded",
                axum::routing::get(|| async move {
                    let mut res = (
                        [("content-type", "text/plain"), ("content-encoding", "gzip")],
                        "q".repeat(4096),
                    )
                        .into_response();
                    mark_app_response(&mut res);
                    res
                }),
            )
            .layer(compression_layer());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/encoded")
                    .header("accept-encoding", "br, gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.headers()
                .get_all(header::CONTENT_ENCODING)
                .iter()
                .collect::<Vec<_>>(),
            vec![&"gzip".parse::<axum::http::HeaderValue>().unwrap()]
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            bytes.to_vec(),
            vec![b'q'; 4096],
            "body must pass through untouched"
        );
    }

    #[tokio::test]
    async fn layer_returns_406_when_no_encoding_is_accepted() {
        let app = marked_app();
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/big")
                    .header("accept-encoding", "identity;q=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_ACCEPTABLE);
        assert!(vary_has_accept_encoding(res.headers()));
    }

    // ---- ETag weakening (no deno) ---------------------------------------------
    //
    // The handler mirrors `pipeline_handler`: it marks the response as app
    // and — only when the worker did NOT provide its own content-encoding —
    // as a plain worker body, exactly as the production code does. The ETag
    // middleware sits immediately OUTSIDE the compression layer, so the fact
    // that it can see the markers also proves the response extensions survive
    // the tower-http compression layer (which rebuilds the response from
    // `parts`).

    fn etag_app(etag: Option<&'static str>, worker_has_encoding: bool, size: usize) -> Router {
        Router::new()
            .route(
                "/big",
                axum::routing::get(move || async move {
                    let mut res =
                        ([("content-type", "text/plain")], "l".repeat(size)).into_response();
                    mark_app_response(&mut res);
                    if !worker_has_encoding {
                        mark_worker_without_encoding(&mut res);
                    }
                    if worker_has_encoding {
                        res.headers_mut()
                            .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
                    }
                    if let Some(value) = etag {
                        res.headers_mut().insert(
                            header::ETAG,
                            value.parse::<axum::http::HeaderValue>().unwrap(),
                        );
                    }
                    res
                }),
            )
            .layer(compression_layer())
            .layer(axum::middleware::from_fn(weaken_worker_etag))
    }

    async fn etag_request(
        app: Router,
        accept_encoding: Option<&str>,
    ) -> axum::http::Response<Body> {
        let mut builder = Request::builder().uri("/big");
        if let Some(value) = accept_encoding {
            builder = builder.header("accept-encoding", value);
        }
        app.oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn strong_etag_becomes_weak_when_compressed_with_brotli() {
        let app = etag_app(Some("\"v1\""), false, 4096);
        let res = etag_request(app, Some("br")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("br")
        );
        assert_eq!(
            res.headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some("W/\"v1\"")
        );
    }

    #[tokio::test]
    async fn strong_etag_becomes_weak_when_compressed_with_gzip() {
        let app = etag_app(Some("\"v1\""), false, 4096);
        let res = etag_request(app, Some("gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        assert_eq!(
            res.headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some("W/\"v1\"")
        );
    }

    #[tokio::test]
    async fn strong_etag_stays_strong_without_accept_encoding() {
        let app = etag_app(Some("\"v1\""), false, 4096);
        let res = etag_request(app, None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(
            res.headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some("\"v1\"")
        );
    }

    #[tokio::test]
    async fn weak_etag_stays_weak_in_all_variants() {
        let app = etag_app(Some("W/\"v1\""), false, 4096);
        for accept_encoding in [Some("br"), Some("gzip"), None] {
            let res = etag_request(app.clone(), accept_encoding).await;
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(
                res.headers()
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok()),
                Some("W/\"v1\""),
                "variant {accept_encoding:?}: the weak etag must stay unchanged"
            );
        }
    }

    #[tokio::test]
    async fn strong_etag_with_worker_content_encoding_stays_intact() {
        let app = etag_app(Some("\"v1\""), true, 4096);
        let res = etag_request(app, Some("br, gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        // The worker's own content-encoding passes through, uncompressed.
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        assert_eq!(
            res.headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some("\"v1\""),
            "worker-provided encoding: the strong etag must stay intact"
        );
    }

    #[tokio::test]
    async fn strong_etag_of_a_body_below_the_floor_stays_intact() {
        let app = etag_app(Some("\"v1\""), false, 512);
        let res = etag_request(app, Some("br, gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(
            res.headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some("\"v1\"")
        );
    }

    // The ETag field is a SINGLE opaque entity-tag: a comma inside the quotes
    // is part of the tag and must never be split. Compressed variants weaken
    // the WHOLE tag; identity keeps it intact.
    #[tokio::test]
    async fn etag_with_comma_inside_the_quotes_is_never_split() {
        let app = etag_app(Some("\"a,b\""), false, 4096);
        for accept_encoding in [Some("gzip"), Some("br")] {
            let res = etag_request(app.clone(), accept_encoding).await;
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(
                res.headers().get(header::ETAG).map(|v| v.as_bytes()),
                Some(b"W/\"a,b\"".as_slice()),
                "variant {accept_encoding:?}: the whole tag is weakened, comma preserved"
            );
        }
        let res = etag_request(app, None).await;
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(
            res.headers().get(header::ETAG).map(|v| v.as_bytes()),
            Some(b"\"a,b\"".as_slice()),
            "identity: the strong tag with an inner comma stays intact"
        );
    }

    // Repeated `ETag` fields: every field is weakened by the same rule.
    #[tokio::test]
    async fn repeated_strong_etag_fields_are_all_weakened() {
        let app = Router::new()
            .route(
                "/big",
                axum::routing::get(|| async move {
                    let mut res =
                        ([("content-type", "text/plain")], "l".repeat(4096)).into_response();
                    mark_app_response(&mut res);
                    mark_worker_without_encoding(&mut res);
                    res.headers_mut()
                        .append(header::ETAG, "\"v1\"".parse().unwrap());
                    res.headers_mut()
                        .append(header::ETAG, "\"v2\"".parse().unwrap());
                    res
                }),
            )
            .layer(compression_layer())
            .layer(axum::middleware::from_fn(weaken_worker_etag));
        let res = etag_request(app, Some("br")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("br")
        );
        let etags: Vec<Vec<u8>> = res
            .headers()
            .get_all(header::ETAG)
            .iter()
            .map(|v| v.as_bytes().to_vec())
            .collect();
        assert_eq!(
            etags,
            vec![b"W/\"v1\"".to_vec(), b"W/\"v2\"".to_vec()],
            "both repeated strong fields are weakened"
        );
    }

    // ---- known-size floor (no deno): body < 1 KiB is not compressed -----------

    #[tokio::test]
    async fn marked_body_below_the_1kib_floor_is_not_compressed() {
        let app = Router::new()
            .route(
                "/small",
                axum::routing::get(|| async move {
                    let mut res =
                        ([("content-type", "text/plain")], "s".repeat(512)).into_response();
                    mark_app_response(&mut res);
                    res
                }),
            )
            .layer(compression_layer());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/small")
                    .header("accept-encoding", "br, gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.to_vec(), vec![b's'; 512]);
    }

    // ---- full pipeline: control plane stays plain (no deno) ---------------------

    // The review (P3) requires the control-plane exemption to be proven with a
    // LARGE body (>= 1024 B): a small body stays plain because of the size
    // floor, not because of the app-only predicate. `GET /api/admin/workers`
    // with several registered apps is a real control-plane route whose JSON
    // body exceeds the floor deterministically; its size is asserted below.
    #[tokio::test]
    async fn control_plane_is_never_compressed() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..10 {
            let name = format!("control-plane-app-{i:02}");
            worker(root.path(), &name, "Deno.serve(() => new Response('ok'))");
        }
        let app = build_pipeline(state(root.path().to_path_buf()));

        // Small body: still plain.
        let res = send(app.clone(), "/health", Some("br")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&bytes[..], br#"{"status":"ok"}"#);

        // Large body (>= 1024 B) under BOTH negotiated codings: the absence of
        // content-encoding must come from the app-only predicate, not the
        // size floor.
        for accept_encoding in ["br", "gzip"] {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/admin/workers")
                        .header("x-api-key", "test-root")
                        .header("accept-encoding", accept_encoding)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert!(
                !res.headers().contains_key(header::CONTENT_ENCODING),
                "control plane must not be compressed with {accept_encoding}"
            );
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            assert!(
                bytes.len() >= 1024,
                "the fixture must push the listing past the 1024 B floor; got {} bytes",
                bytes.len()
            );
        }
    }

    // EDG-6: the tower-http 406 (RFC 9110 §12.5.3) is scoped to the data
    // plane. `Accept-Encoding` accepting neither `br`, `gzip` nor
    // `identity` no longer makes the control plane answer 406: the request
    // is renegotiated to identity and every control-plane route returns its
    // normal response, uncompressed.
    #[tokio::test]
    async fn control_plane_never_406s_and_stays_plain_when_no_encoding_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..10 {
            let name = format!("control-plane-app-{i:02}");
            worker(root.path(), &name, "Deno.serve(() => new Response('ok'))");
        }
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app.clone(), "/health", Some("identity;q=0")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert!(!vary_has_accept_encoding(res.headers()));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&bytes[..], br#"{"status":"ok"}"#);

        // Same for the admin API (large body, >= 1024 B: the plain response
        // must come from the renegotiation, not the size floor).
        let res = send(app.clone(), "/api/admin/workers", Some("*;q=0")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert!(!vary_has_accept_encoding(res.headers()));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            bytes.len() >= 1024,
            "the fixture must push the listing past the 1024 B floor; got {} bytes",
            bytes.len()
        );

        // The 406 used to mask an EXECUTED admin mutation (the layer 406'd
        // after the handler ran). Now the operation must execute with a
        // NORMAL response: disable a worker and prove the effect persists.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/workers/control-plane-app-01/disable")
                    .header("authorization", "Bearer test-root")
                    .header("accept-encoding", "identity;q=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "an unsatisfiable Accept-Encoding must not mask an executed admin operation"
        );
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let value: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["status"], "disabled", "the operation itself ran");

        // The operation was executed exactly once: the worker stays disabled
        // in the listing (read back without the unsatisfiable header).
        let res = send(app, "/api/admin/workers", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let value: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let workers = value["workers"].as_array().expect("worker listing");
        let disabled = workers
            .iter()
            .find(|w| w["name"] == "control-plane-app-01")
            .expect("worker listed");
        assert_eq!(
            disabled["status"].as_str(),
            Some("disabled"),
            "the disable mutation must have taken effect: {value:?}"
        );
    }

    // EDG-6: an APP response to the same unsatisfiable `Accept-Encoding`
    // still answers 406 Not Acceptable — with the same body/headers the
    // tower-http layer passes through today (the plain body, the status
    // overwritten, `Vary: Accept-Encoding` appended when missing).
    #[tokio::test]
    async fn app_response_still_gets_406_when_no_encoding_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "hello", "Deno.serve(() => new Response('ok'))");
        let app = build_pipeline(state(root.path().to_path_buf()));

        // Any app route: with deno the worker answers; without it the
        // pipeline returns a JSON error — both are app responses
        // (`AppResponse` marker), so the 406 scoping applies and the PLAIN
        // body must pass through unchanged.
        let res = send(app.clone(), "/hello/", Some("identity;q=0")).await;
        assert_eq!(res.status(), StatusCode::NOT_ACCEPTABLE);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert!(vary_has_accept_encoding(res.headers()));
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        if &body[..] == b"ok" {
            // deno is on PATH: the worker's plain body passes through.
        } else {
            let value: serde_json::Value = serde_json::from_slice(&body).expect("JSON error body");
            assert!(
                value.get("code").is_some(),
                "the plain error body passes through: {value:?}"
            );
        }

        for reject in ["*;q=0", "br;q=0,gzip;q=0,identity;q=0"] {
            let res = send(app.clone(), "/hello/", Some(reject)).await;
            assert_eq!(
                res.status(),
                StatusCode::NOT_ACCEPTABLE,
                "{reject} must be 406 for app responses"
            );
        }

        // The control plane on the SAME pipeline stays 200.
        let res = send(app, "/health", Some("identity;q=0")).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    // ---- full pipeline: fullstack/SPA static assets (no deno) -------------------

    #[tokio::test]
    async fn spa_asset_is_compressed_with_immutable_cache_control_preserved() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("spa-app");
        fs::create_dir_all(dir.join("assets")).unwrap();
        fs::write(
            dir.join("manifest.yaml"),
            "name: spa-app\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: spa\n",
        )
        .unwrap();
        let html = format!("<!doctype html><html><body>{}", "h".repeat(2048));
        fs::write(dir.join("index.html"), html).unwrap();
        let js = format!("// spa bundle\n{}", "s".repeat(2048));
        fs::write(dir.join("assets").join("app-AbCd1234.js"), js.clone()).unwrap();

        let app = build_pipeline(state(root.path().to_path_buf()));
        let res = send(app, "/spa-app/assets/app-AbCd1234.js", Some("br")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("br")
        );
        assert_eq!(
            res.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(
            res.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/javascript; charset=utf-8")
        );
        assert!(vary_has_accept_encoding(res.headers()));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(brotli_decode(&bytes)).unwrap(), js);
    }

    // ---- EDG-6: configuration + byte counters (no deno) ---------------------

    #[tokio::test]
    async fn compression_off_mounts_no_layer() {
        let root = tempfile::tempdir().unwrap();
        let (_html, js) = spa_app(root.path());
        let (st, _) = state_with_compression(
            root.path().to_path_buf(),
            CompressionConfig {
                enabled: false,
                ..Default::default()
            },
        );
        let app = build_pipeline(st);

        // No content-encoding even when br is accepted: there is no layer
        // at all.
        let res = send(app.clone(), "/spa-app/assets/app-AbCd1234.js", Some("br")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(bytes.to_vec()).unwrap(), js);

        // And no 406: the unsatisfiable Accept-Encoding is renegotiated to
        // identity and the asset is served plain.
        let res = send(app, "/spa-app/assets/app-AbCd1234.js", Some("identity;q=0")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
    }

    #[tokio::test]
    async fn compression_min_bytes_config_is_applied() {
        let root = tempfile::tempdir().unwrap();
        spa_app(root.path());
        // Second, 8 KiB asset: with a 4096 B floor the 2 KiB asset must NOT
        // be compressed while the 8 KiB one must.
        fs::write(
            root.path()
                .join("spa-app")
                .join("assets")
                .join("big-AbCd1234.js"),
            format!("// big bundle\n{}", "t".repeat(8192)),
        )
        .unwrap();
        let (st, _) = state_with_compression(
            root.path().to_path_buf(),
            CompressionConfig {
                min_bytes: 4096,
                ..Default::default()
            },
        );
        let app = build_pipeline(st);

        // ~2 KiB body, below the 4096 B floor: plain.
        let res = send(app.clone(), "/spa-app/assets/app-AbCd1234.js", Some("br")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));

        // ~8 KiB body, above the floor: compressed.
        let res = send(app, "/spa-app/assets/big-AbCd1234.js", Some("br")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("br")
        );
    }

    #[tokio::test]
    async fn compression_byte_counters_record_br_and_gzip() {
        let root = tempfile::tempdir().unwrap();
        let (_html, _js) = spa_app(root.path());
        let (st, metrics) =
            state_with_compression(root.path().to_path_buf(), CompressionConfig::default());
        let app = build_pipeline(st);

        // brotli: when the compressed body ends, the wrapper drops and the
        // counters are updated.
        let res = send(app.clone(), "/spa-app/assets/app-AbCd1234.js", Some("br")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .unwrap()
                .to_str()
                .unwrap(),
            "br"
        );
        let _compressed = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(
            metrics.br_bytes_in() >= 1024,
            "pre-compression bytes counted"
        );
        assert!(metrics.br_bytes_out() > 0, "compressed bytes counted");
        assert!(
            metrics.br_bytes_out() < metrics.br_bytes_in(),
            "br must actually shrink the body: in={} out={}",
            metrics.br_bytes_in(),
            metrics.br_bytes_out()
        );
        // The gzip counter is untouched by the br response.
        assert_eq!(metrics.gzip_bytes_in(), 0);
        assert_eq!(metrics.gzip_bytes_out(), 0);

        // gzip: counted under its own label set.
        let res = send(app.clone(), "/spa-app/assets/app-AbCd1234.js", Some("gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .unwrap()
                .to_str()
                .unwrap(),
            "gzip"
        );
        let _compressed = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(metrics.gzip_bytes_in() >= 1024);
        assert!(metrics.gzip_bytes_out() > 0);
        assert!(metrics.gzip_bytes_out() < metrics.gzip_bytes_in());

        // Uncompressed traffic is not counted at all.
        let in_before = metrics.br_bytes_in();
        let out_before = metrics.br_bytes_out();
        let res = send(app.clone(), "/spa-app/assets/app-AbCd1234.js", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let _plain = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            metrics.br_bytes_in(),
            in_before,
            "identity traffic is not counted"
        );
        assert_eq!(metrics.br_bytes_out(), out_before);

        // The control plane /metrics renders the counters with the non-zero
        // values.
        let res = send(app, "/metrics", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        for line in [
            "edger_http_compression_bytes_in_total{encoding=\"br\"} ",
            "edger_http_compression_bytes_out_total{encoding=\"br\"} ",
            "edger_http_compression_bytes_in_total{encoding=\"gzip\"} ",
            "edger_http_compression_bytes_out_total{encoding=\"gzip\"} ",
        ] {
            let sample = text
                .lines()
                .find(|l| l.starts_with(line))
                .unwrap_or_else(|| panic!("missing {line:?} in /metrics"));
            let value = sample
                .split_whitespace()
                .next_back()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or_else(|| panic!("bad sample {sample:?}"));
            assert!(value > 0, "{line:?} must be non-zero: {sample:?}");
        }
        assert!(text.contains("# TYPE edger_http_compression_bytes_in_total counter"));
        assert!(text.contains("# TYPE edger_http_compression_bytes_out_total counter"));
    }

    // EDG-6 correction 1 + 2: HEAD is never compressed and its metadata
    // passes through untouched; and a 304 carries NO content-length
    // (artificial or not), for GET and HEAD alike — the main 0154d3a
    // criterion (static fixture, no deno).
    #[tokio::test]
    async fn head_response_preserves_metadata_and_is_never_compressed() {
        let root = tempfile::tempdir().unwrap();
        spa_app(root.path());
        let app = build_pipeline(state(root.path().to_path_buf()));
        let uri = "/spa-app/assets/app-AbCd1234.js";

        // Reference: the identity GET (no Accept-Encoding) — the HEAD
        // metadata must match its content-length (present or absent alike).
        let res = send(app.clone(), uri, None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let get_content_length = res
            .headers()
            .get(header::CONTENT_LENGTH)
            .map(|value| value.to_str().ok().map(|s| s.to_string()));
        let etag_value = res
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .expect("static asset carries a weak etag")
            .to_string();
        drop(
            axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        );

        // HEAD without Accept-Encoding: the same content-length as the
        // identity GET (or absent, exactly like the GET) and no
        // content-encoding.
        let res = head_request(app.clone(), uri, None, None).await;
        assert_eq!(res.status(), StatusCode::OK);
        let head_content_length = res
            .headers()
            .get(header::CONTENT_LENGTH)
            .map(|value| value.to_str().ok().map(|s| s.to_string()));
        assert_eq!(
            head_content_length, get_content_length,
            "HEAD must preserve the GET's content-length (or its absence)"
        );
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        drop(
            axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        );

        // HEAD with br accepted: never compressed — no content-encoding.
        let res = head_request(app.clone(), uri, Some("br"), None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        drop(
            axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        );

        // GET with a matching If-None-Match: 304 with NO content-length —
        // not even an artificial 0 — no content-encoding, empty body.
        // main 0154d3a returned no content-length on a 304 (verified on a
        // base copy); the pipeline gives the 304 an unknown-size empty
        // stream so axum's `set_content_length` stamp is skipped
        // (pipeline.rs, RFC 9110 §15.4.5).
        let res = get_request(app.clone(), uri, None, Some(&etag_value)).await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert!(
            !res.headers().contains_key(header::CONTENT_LENGTH),
            "a 304 carries no content-length, artificial or not (got {:?})",
            res.headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
        );
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(bytes.is_empty(), "a 304 carries no body, got {bytes:?}");

        // HEAD with a matching If-None-Match: 304 with no content-encoding
        // and — correction 2 — NO content-length at all, not even the
        // artificial `content-length: 0`. Since the EDG-6 rework a HEAD 304
        // carried the framework's exact-0 stamp (main 0154d3a returned no
        // content-length on a 304); the outermost pipeline middleware
        // (`strip_304_content_length`) removes the header and leaves the
        // body unknown-sized, so axum's `RouteFuture`
        // `set_content_length` — which records `size_hint().exact()` when
        // the header is absent and empties the HEAD body to the exact-0
        // `Body::empty()` around the layers — has nothing to record.
        let res = head_request(app, uri, None, Some(&etag_value)).await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert!(
            !res.headers().contains_key(header::CONTENT_LENGTH),
            "a 304 carries no content-length, artificial or not (got {:?})",
            res.headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
        );
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(bytes.is_empty(), "a 304 carries no body, got {bytes:?}");
    }

    // EDG-6 correction 3: a COMPRESSED app error response (the pipeline Err
    // branch, which `pipeline_handler` does not mark with
    // `WorkerResponseWithoutEncoding`) still advances the byte counters
    // exactly once — the transformation marker is inserted by the inner
    // middleware, which covers both pipeline branches (no deno).
    #[tokio::test]
    async fn compressed_app_error_advances_byte_counters_once() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "hello", "Deno.serve(() => new Response('ok'))");
        let (st, metrics) = state_with_compression(
            root.path().to_path_buf(),
            CompressionConfig {
                min_bytes: 32,
                ..Default::default()
            },
        );
        let app = build_pipeline(st);

        // The route does not resolve to any worker: the pipeline answers its
        // JSON error (404) — deterministically, with or without deno.
        let res = send(app, "/missing-app", Some("gzip")).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip"),
            "the pipeline error JSON must be compressed (32 B floor)"
        );
        let compressed = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let plain = gzip_decode(&compressed);
        let value: serde_json::Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(value["code"], "NOT_FOUND");
        assert!(
            plain.len() >= 32,
            "the error body is above the 32 B floor: {plain:?}"
        );

        // Both counters advanced ONCE: `in` equals the plain body length (a
        // double add would be 2x) and `out` equals exactly the compressed
        // bytes collected above. (No shrink assertion: a small body can
        // legitimately grow under gzip — the codec's fixed header overhead
        // exceeds the savings; that is a property of the codec, not of the
        // counting. The pipeline's real min_bytes floor prevents it in
        // production traffic.)
        assert_eq!(metrics.gzip_bytes_in(), plain.len() as u64);
        assert_eq!(metrics.gzip_bytes_out(), compressed.len() as u64);
        // The br label set is untouched.
        assert_eq!(metrics.br_bytes_in(), 0);
        assert_eq!(metrics.br_bytes_out(), 0);
    }

    // ---- deno-backed (run explicitly) -------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn large_text_asset_negotiates_brotli() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "big-app", BIG_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/big-app", Some("br, gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("br")
        );
        assert!(vary_has_accept_encoding(res.headers()));
        assert!(
            !res.headers().contains_key(header::CONTENT_LENGTH),
            "compressed body carries no content-length"
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let original = format!("/* edger bundle */\n{}", "x".repeat(4096));
        assert!(
            bytes.len() < original.len(),
            "brotli output {} must be smaller than {} bytes",
            bytes.len(),
            original.len()
        );
        assert_eq!(String::from_utf8(brotli_decode(&bytes)).unwrap(), original);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn large_text_asset_negotiates_gzip() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "big-app", BIG_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/big-app", Some("gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let original = format!("/* edger bundle */\n{}", "x".repeat(4096));
        assert_eq!(String::from_utf8(gzip_decode(&bytes)).unwrap(), original);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn no_accept_encoding_serves_original_body() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "big-app", BIG_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/big-app", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        // tower-http 0.7 still announces `Vary: Accept-Encoding` on the
        // identity path when the predicate says the body WOULD be compressed
        // (the client simply did not accept it). The body itself is untouched.
        assert!(vary_has_accept_encoding(res.headers()));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let original = format!("/* edger bundle */\n{}", "x".repeat(4096));
        assert_eq!(String::from_utf8(bytes.to_vec()).unwrap(), original);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn br_with_q0_falls_back_to_gzip() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "big-app", BIG_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/big-app", Some("br;q=0, gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let original = format!("/* edger bundle */\n{}", "x".repeat(4096));
        assert_eq!(String::from_utf8(gzip_decode(&bytes)).unwrap(), original);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn worker_provided_content_encoding_stays_intact() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "encoded-app", SELF_ENCODED_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/encoded-app", Some("br, gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get_all(header::CONTENT_ENCODING)
                .iter()
                .collect::<Vec<_>>(),
            vec![&"gzip".parse::<axum::http::HeaderValue>().unwrap()],
            "the worker's own content-encoding passes through, uncompressed once"
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.to_vec(), vec![b'z'; 4096], "body untouched");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn no_transform_response_is_not_compressed() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "no-transform-app", NO_TRANSFORM_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/no-transform-app", Some("br, gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(bytes.to_vec(), vec![b'y'; 4096]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn sse_is_not_compressed_and_stays_incremental() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "sse-app", SSE_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/sse-app", Some("br, gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));

        let mut body = res.into_body().into_data_stream();
        let started = Instant::now();
        let first = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("first SSE event within 5s")
            .expect("stream open")
            .expect("chunk ok");
        let first_at = started.elapsed();
        assert!(String::from_utf8_lossy(&first).contains("tick-"));

        let second = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("second SSE event within 5s")
            .expect("stream open")
            .expect("chunk ok");
        let second_at = started.elapsed();
        assert!(String::from_utf8_lossy(&second).contains("tick-"));

        assert!(
        second_at >= first_at + Duration::from_millis(100),
        "SSE must stay incremental through the compression layer: first {first_at:?}, second {second_at:?}"
    );
    }

    // Progressive delivery: the worker enqueues part1, waits 500 ms, then
    // part2. The client must receive the FIRST compressed chunk, be able to
    // decompress its bytes into part1's prefix — all before part2 exists on the
    // wire. Times are measured like streaming_e2e.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn compressed_delivery_is_progressive() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "progressive-app", PROGRESSIVE_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/progressive-app", Some("gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );

        let mut body = res.into_body().into_data_stream();
        let started = Instant::now();
        let first = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("first compressed chunk within 5s")
            .expect("stream open")
            .expect("chunk ok");
        let first_at = started.elapsed();
        assert!(!first.is_empty(), "first compressed chunk must carry data");

        // Decode what the first chunk already carries — BEFORE part2 exists.
        let prefix = partial_gzip_prefix(&first);
        assert!(
            prefix.starts_with(b"PART1:"),
            "first chunk must decode to part1's prefix, got {prefix:?}"
        );

        let rest = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("second compressed chunk within 5s")
            .expect("stream open")
            .expect("chunk ok");
        let second_at = started.elapsed();
        assert!(
        second_at >= first_at + Duration::from_millis(300),
        "part2 is enqueued 500 ms after part1: the first chunk must reach the client well before it — first {first_at:?}, second {second_at:?}"
    );

        let mut all: Vec<u8> = first.to_vec();
        all.extend_from_slice(&rest);
        let mut collected = body.collect::<Vec<_>>().await;
        for chunk in collected.drain(..) {
            all.extend_from_slice(chunk.expect("chunk ok").as_ref());
        }
        let decoded = String::from_utf8(gzip_decode(&all)).expect("full gzip stream decodes");
        let expected = format!("PART1:{}PART2:{}", "a".repeat(2048), "b".repeat(2048));
        assert_eq!(decoded, expected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn small_streamed_worker_body_is_compressed_because_size_is_unknown() {
        // Deno responses always flow as a stream (size unknown), so the 1 KiB
        // floor cannot apply — decision 4: unknown-size bodies are compressed.
        // The floor itself is proven by `marked_body_below_the_1kib_floor_is_not_compressed`.
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "small-app", SMALL_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/small-app", Some("gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(gzip_decode(&bytes), b"tiny");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn head_response_is_not_compressed() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "big-app", BIG_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = app
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/big-app")
                    .header("accept-encoding", "br, gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(bytes.is_empty(), "HEAD carries no body, got {bytes:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn identity_q0_app_route_returns_406() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "big-app", BIG_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/big-app", Some("identity;q=0")).await;
        assert_eq!(res.status(), StatusCode::NOT_ACCEPTABLE);
        assert!(vary_has_accept_encoding(res.headers()));
    }

    // EDG-6: streaming compressed response — when the compressed body ends,
    // the counters record the pre/post-compression totals (the encoder
    // flushes per chunk; the body is counted as it passes).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn compression_byte_counters_record_streamed_response() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "big-app", BIG_WORKER);
        let (st, metrics) =
            state_with_compression(root.path().to_path_buf(), CompressionConfig::default());
        let app = build_pipeline(st);

        let res = send(app, "/big-app", Some("gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let plain = gzip_decode(&bytes);
        let expected_plain_len = "/* edger bundle */\n".len() + 4096;
        assert_eq!(plain.len(), expected_plain_len);
        assert_eq!(metrics.gzip_bytes_in(), expected_plain_len as u64);
        assert_eq!(metrics.gzip_bytes_out(), bytes.len() as u64);
        assert!(metrics.gzip_bytes_out() < metrics.gzip_bytes_in());
        assert_eq!(metrics.br_bytes_in(), 0);
        assert_eq!(metrics.br_bytes_out(), 0);
    }

    // ETag through the full pipeline: a worker's strong ETag must be weakened
    // exactly when compression changed the content-coding, and stay intact on
    // every uncompressed variant.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn worker_strong_etag_becomes_weak_when_compressed() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "etag-app", ETAG_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        for (accept_encoding, expected_encoding) in [("br, gzip", "br"), ("gzip", "gzip")] {
            let res = send(app.clone(), "/etag-app", Some(accept_encoding)).await;
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(
                res.headers()
                    .get(header::CONTENT_ENCODING)
                    .and_then(|v| v.to_str().ok()),
                Some(expected_encoding),
                "accept-encoding: {accept_encoding}"
            );
            assert_eq!(
                res.headers()
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok()),
                Some("W/\"v1\""),
                "the strong worker etag must be weakened when compressed ({accept_encoding})"
            );
        }

        let res = send(app, "/etag-app", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(
            res.headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some("\"v1\""),
            "without accept-encoding the strong etag stays intact"
        );
    }

    // Cancellation of a COMPRESSED stream: the client takes one chunk of the
    // gzip-compressed infinite HTML stream, drops the body (client
    // disconnect), and a second request to the same app must answer
    // normally — the recycled instance must not wedge. Pattern of
    // streaming_e2e::client_disconnect_mid_stream_recycles_the_worker, but
    // the body travels through the compression layer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn compressed_stream_cancellation_recycles_the_worker() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "infinite-app", INFINITE_HTML_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        // First request: take one compressed chunk, then DROP the body
        // (client disconnect).
        let res = send(app.clone(), "/infinite-app", Some("gzip")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let mut body = res.into_body().into_data_stream();
        let _ = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("first compressed chunk")
            .expect("stream open")
            .expect("chunk ok");
        drop(body);
        // Give the recycle task a beat.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Second request must get a FRESH worker, not a wedged/desynced one.
        let res = tokio::time::timeout(
            Duration::from_secs(10),
            send(app.clone(), "/infinite-app", Some("gzip")),
        )
        .await
        .expect("second request must not hang after the compressed stream was cancelled");
        assert_eq!(res.status(), StatusCode::OK, "recycled worker serves again");
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let mut body = res.into_body().into_data_stream();
        let chunk = tokio::time::timeout(Duration::from_secs(10), body.next())
            .await
            .expect("chunk from fresh worker")
            .expect("stream open")
            .expect("chunk ok");
        let prefix = partial_gzip_prefix(&chunk);
        assert!(
            String::from_utf8_lossy(&prefix).contains("tick-"),
            "the fresh worker's compressed stream must decode to ticks, got {prefix:?}"
        );
    }

    // Progressive delivery with BROTLI: mirror of `compressed_delivery_is_progressive`
    // with `Accept-Encoding: br`. The worker enqueues part1, waits 500 ms,
    // then part2. The client must receive the FIRST compressed chunk and be
    // able to decode its bytes into part1's prefix — all before part2 exists
    // on the wire.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs deno on PATH; run explicitly"]
    async fn brotli_delivery_is_progressive() {
        let root = tempfile::tempdir().unwrap();
        worker(root.path(), "progressive-app", PROGRESSIVE_WORKER);
        let app = build_pipeline(state(root.path().to_path_buf()));

        let res = send(app, "/progressive-app", Some("br")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("br")
        );

        let mut body = res.into_body().into_data_stream();
        let started = Instant::now();
        let first = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("first compressed chunk within 5s")
            .expect("stream open")
            .expect("chunk ok");
        let first_at = started.elapsed();
        assert!(!first.is_empty(), "first compressed chunk must carry data");

        // Decode what the first chunk already carries — BEFORE part2 exists.
        let prefix = partial_brotli_prefix(&first);
        assert!(
            prefix.starts_with(b"PART1:"),
            "first chunk must decode to part1's prefix, got {prefix:?}"
        );

        let rest = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("second compressed chunk within 5s")
            .expect("stream open")
            .expect("chunk ok");
        let second_at = started.elapsed();
        assert!(
        second_at >= first_at + Duration::from_millis(300),
        "part2 is enqueued 500 ms after part1: the first chunk must reach the client well before it — first {first_at:?}, second {second_at:?}"
    );

        let mut all: Vec<u8> = first.to_vec();
        all.extend_from_slice(&rest);
        let collected = body.collect::<Vec<_>>().await;
        for chunk in collected {
            all.extend_from_slice(chunk.expect("chunk ok").as_ref());
        }
        let decoded = String::from_utf8(brotli_decode(&all)).expect("full brotli stream decodes");
        let expected = format!("PART1:{}PART2:{}", "a".repeat(2048), "b".repeat(2048));
        assert_eq!(decoded, expected);
    }
}
