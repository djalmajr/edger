//! EDG-5 part 2: 304 Not Modified (`If-None-Match`) through the full pipeline.
//!
//! - when a client revalidates a static asset with an `If-None-Match` that
//!   matches the worker response's ETag, the pipeline answers 304 Not
//!   Modified instead of re-sending the body;
//! - a 304 has no body and keeps only the revalidation fields (`etag`,
//!   `cache-control`); no `content-type`, `content-length` or
//!   `content-encoding` travels;
//! - the short-circuit is wired on the BUFFERED branch only, with the
//!   RFC 9110 §13.1.2 method/status gates (GET/HEAD, status 200);
//! - the weak ETag (`W/"..."`) echoed from a compressed response revalidates
//!   too;
//! - `x-request-id` (added by the outer middleware) survives on the 304.
//!
//! Static SPA and fullstack workers are served by pure Rust (no Deno
//! process), so these tests run by default.

use std::fs;
use std::io::Read;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode};
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

/// Static SPA app: entry HTML (`no-cache`) plus a fingerprinted asset under
/// `assets/` (`immutable`). Returns the asset body so the tests can compare.
fn spa_app(root: &std::path::Path) -> String {
    let dir = root.join("spa-app");
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
    // A sub-1 KiB asset: below the compression floor, so neither its 200
    // (even with `br`) nor its 304 may carry `Vary: Accept-Encoding`.
    fs::write(dir.join("assets").join("small.js"), "// tiny\n").unwrap();
    js
}

/// Fullstack app (hono adapter): the client build lives in `client/` and the
/// default asset prefix `/assets/` is served statically — no Deno process is
/// spawned for the asset path. Returns the asset body.
fn fullstack_app(root: &std::path::Path) -> String {
    let dir = root.join("fs-app");
    fs::create_dir_all(dir.join("client").join("assets")).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: fs-app\nversion: \"1.0.0\"\nentrypoint: server.ts\nkind: fullstack\nadapter: hono\nclientDir: client\n",
    )
    .unwrap();
    fs::write(
        dir.join("server.ts"),
        "Deno.serve(() => new Response('ok'))",
    )
    .unwrap();
    let js = format!("// fs bundle\n{}", "f".repeat(2048));
    fs::write(
        dir.join("client").join("assets").join("app-AbCd1234.js"),
        js.clone(),
    )
    .unwrap();
    js
}

fn request(
    method: &str,
    uri: &str,
    accept_encoding: Option<&str>,
    if_none_match: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer test-root");
    if let Some(encoding) = accept_encoding {
        builder = builder.header("accept-encoding", encoding);
    }
    if let Some(value) = if_none_match {
        builder = builder.header("if-none-match", value);
    }
    builder.body(Body::empty()).unwrap()
}

async fn send(app: &Router, req: Request<Body>) -> axum::http::Response<Body> {
    app.clone().oneshot(req).await.unwrap()
}

fn header_str<'a>(res: &'a axum::http::Response<Body>, name: &str) -> Option<&'a str> {
    res.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn etag(res: &axum::http::Response<Body>) -> String {
    header_str(res, "etag")
        .expect("etag header present")
        .to_string()
}

/// True when some `Vary` field lists `accept-encoding` — the marker the
/// compression layer adds to a compressible 200, and that the 304 must mirror.
fn vary_has_accept_encoding(res: &axum::http::Response<Body>) -> bool {
    res.headers()
        .get_all("vary")
        .into_iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("accept-encoding"))
        })
}

/// A GET carrying one or more RAW `If-None-Match` header lines, to exercise
/// the repeated-field and malformed cases a single `&str` cannot express.
fn get_inm(uri: &str, inm_values: Vec<HeaderValue>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer test-root");
    for value in inm_values {
        builder = builder.header("if-none-match", value);
    }
    builder.body(Body::empty()).unwrap()
}

/// Consume a 304 body and prove it is empty (headers must be read from `res`
/// BEFORE this, since `into_body` moves the response).
async fn assert_body_empty(res: axum::http::Response<Body>) {
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body.is_empty(), "a 304 carries no body, got {body:?}");
}

/// Every e2e revalidation case over a static asset, shared by the SPA and
/// fullstack fixtures. `entry_uri` (SPA only: a fullstack entry falls through
/// to the SSR worker, which needs a Deno process) exercises the `no-cache`
/// entry HTML revalidation.
async fn revalidation_round_trip(
    app: &Router,
    asset_uri: &str,
    entry_uri: Option<(&str, &str)>,
    asset_body: &str,
) {
    // 1. First GET: 200 with the weak etag and the full body.
    let res = send(app, request("GET", asset_uri, None, None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let etag_value = etag(&res);
    assert!(
        etag_value.starts_with("W/\""),
        "static serving stamps a weak etag, got {etag_value}"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(String::from_utf8(body.to_vec()).unwrap(), asset_body);

    // 2. Revalidation with the SAME etag: 304, no body, revalidation fields
    //    kept, body-describing headers gone.
    let res = send(app, request("GET", asset_uri, None, Some(&etag_value))).await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        header_str(&res, "etag"),
        Some(etag_value.as_str()),
        "the 304 repeats the etag"
    );
    assert_eq!(
        header_str(&res, "cache-control"),
        Some("public, max-age=31536000, immutable"),
        "the 304 keeps the asset cache-control"
    );
    for absent in ["content-type", "content-length", "content-encoding"] {
        assert!(
            !res.headers().contains_key(absent),
            "the 304 must not carry {absent}"
        );
    }
    // 8. x-request-id (outer middleware) survives on the 304.
    assert!(
        res.headers().contains_key("x-request-id"),
        "the 304 must carry x-request-id"
    );
    assert_body_empty(res).await;

    // 3. A DIFFERENT If-None-Match: 200 with the full body.
    let res = send(
        app,
        request("GET", asset_uri, None, Some(r#"W/"deadbeef01234567""#)),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(String::from_utf8(body.to_vec()).unwrap(), asset_body);

    // 4. The entry HTML is `no-cache` but still revalidates to a 304.
    if let Some((entry_uri, entry_cache_control)) = entry_uri {
        let res = send(app, request("GET", entry_uri, None, None)).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(header_str(&res, "cache-control"), Some(entry_cache_control));
        let entry_etag = etag(&res);
        drop(
            axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        );

        let res = send(app, request("GET", entry_uri, None, Some(&entry_etag))).await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            header_str(&res, "etag"),
            Some(entry_etag.as_str()),
            "the 304 repeats the entry etag"
        );
        assert_eq!(
            header_str(&res, "cache-control"),
            Some(entry_cache_control),
            "the 304 keeps the entry cache-control"
        );
        assert_body_empty(res).await;
    }

    // 5. POST with the matching etag: the method gate excludes it — never a
    //    304, the static file comes back in full.
    let res = send(app, request("POST", asset_uri, None, Some(&etag_value))).await;
    assert_ne!(res.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(body.to_vec()).unwrap(),
        asset_body,
        "POST revalidation re-serves the body"
    );

    // 6. HEAD with the matching etag: 304 (HEAD is a revalidation method).
    let res = send(app, request("HEAD", asset_uri, None, Some(&etag_value))).await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert_body_empty(res).await;
}

#[tokio::test]
async fn spa_static_revalidation_returns_304() {
    let root = tempfile::tempdir().unwrap();
    let js = spa_app(root.path());
    let app = build_pipeline(state(root.path().to_path_buf()));
    revalidation_round_trip(
        &app,
        "/spa-app/assets/app-AbCd1234.js",
        Some(("/spa-app/", "no-cache")),
        &js,
    )
    .await;
}

#[tokio::test]
async fn fullstack_static_revalidation_returns_304() {
    let root = tempfile::tempdir().unwrap();
    let js = fullstack_app(root.path());
    let app = build_pipeline(state(root.path().to_path_buf()));
    revalidation_round_trip(
        &app,
        "/fs-app/assets/app-AbCd1234.js",
        // A fullstack entry is served by the SSR worker (Deno); the static
        // asset path is the no-Deno case this fixture exercises.
        None,
        &js,
    )
    .await;
}

// 4/5 from the brief: compressed delivery interacts with revalidation.

/// `Accept-Encoding: br` + matching `If-None-Match` → 304 WITHOUT
/// `content-encoding` (the 304 has no body, so no content-coding applies).
#[tokio::test]
async fn compressed_revalidation_returns_304_without_content_encoding() {
    let root = tempfile::tempdir().unwrap();
    let _js = spa_app(root.path());
    let app = build_pipeline(state(root.path().to_path_buf()));

    let res = send(
        &app,
        request("GET", "/spa-app/assets/app-AbCd1234.js", None, None),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let etag_value = etag(&res);
    drop(
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    );

    let res = send(
        &app,
        request(
            "GET",
            "/spa-app/assets/app-AbCd1234.js",
            Some("br"),
            Some(&etag_value),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert!(
        !res.headers().contains_key("content-encoding"),
        "a 304 must not announce a content-encoding"
    );
    assert!(
        !res.headers().contains_key("content-type"),
        "a 304 must not carry content-type"
    );
    assert_eq!(header_str(&res, "etag"), Some(etag_value.as_str()));
    assert_body_empty(res).await;
}

/// The etag echoed by a COMPRESSED response revalidates: GET with `br` →
/// take the (weak) etag → GET with that exact value → 304.
#[tokio::test]
async fn weak_etag_from_compressed_response_round_trips_to_304() {
    let root = tempfile::tempdir().unwrap();
    let js = spa_app(root.path());
    let app = build_pipeline(state(root.path().to_path_buf()));

    let res = send(
        &app,
        request("GET", "/spa-app/assets/app-AbCd1234.js", Some("br"), None),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        header_str(&res, "content-encoding"),
        Some("br"),
        "the fixture must actually negotiate brotli"
    );
    let compressed_etag = etag(&res);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    // Decode back to the original body (brotli) to prove the 200 carried the
    // real asset, so the round-trip is not vacuous.
    assert_eq!(brotli_decode(&body.to_vec()), js.as_bytes());

    let res = send(
        &app,
        request(
            "GET",
            "/spa-app/assets/app-AbCd1234.js",
            None,
            Some(&compressed_etag),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        header_str(&res, "etag"),
        Some(compressed_etag.as_str()),
        "the weak etag from the compressed response revalidates"
    );
    assert_body_empty(res).await;
}

fn brotli_decode(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = brotli::Decompressor::new(bytes, 0);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .expect("full brotli stream decodes");
    out
}

// The 304 must carry the `Vary` the 200 of the SAME request would carry.

/// A compressible 200 (with `br`) gets `Vary: Accept-Encoding` from the
/// compression layer; the matching 304 must carry the same Vary.
#[tokio::test]
async fn compressed_200_and_304_carry_vary_accept_encoding() {
    let root = tempfile::tempdir().unwrap();
    spa_app(root.path());
    let app = build_pipeline(state(root.path().to_path_buf()));

    // 200 with br: compressible → the layer adds Vary: Accept-Encoding.
    let res = send(
        &app,
        request("GET", "/spa-app/assets/app-AbCd1234.js", Some("br"), None),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let etag_value = etag(&res);
    assert!(
        vary_has_accept_encoding(&res),
        "a compressible 200 must list accept-encoding in Vary"
    );
    drop(
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    );

    // The 304 for the SAME request must mirror that Vary.
    let res = send(
        &app,
        request(
            "GET",
            "/spa-app/assets/app-AbCd1234.js",
            Some("br"),
            Some(&etag_value),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert!(
        vary_has_accept_encoding(&res),
        "the 304 must carry the Vary the 200 would carry"
    );
    assert_body_empty(res).await;
}

/// An asset below the 1 KiB compression floor is not compressible, so NEITHER
/// the 200 (even with `br`) nor the 304 gets `Vary: Accept-Encoding`.
#[tokio::test]
async fn small_asset_below_floor_gets_no_vary() {
    let root = tempfile::tempdir().unwrap();
    spa_app(root.path());
    let app = build_pipeline(state(root.path().to_path_buf()));

    let res = send(
        &app,
        request("GET", "/spa-app/assets/small.js", Some("br"), None),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let etag_value = etag(&res);
    assert!(
        !vary_has_accept_encoding(&res),
        "a sub-1 KiB 200 must not list accept-encoding in Vary"
    );
    drop(
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    );

    let res = send(
        &app,
        request(
            "GET",
            "/spa-app/assets/small.js",
            Some("br"),
            Some(&etag_value),
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert!(
        !vary_has_accept_encoding(&res),
        "the 304 of a sub-1 KiB asset must not gain Vary"
    );
    assert_body_empty(res).await;
}

// Repeated If-None-Match: RFC 9110 §5.2 joins every line before matching.

/// Two `If-None-Match` lines, the match only on the SECOND: the joined value
/// must still revalidate to 304 (a single-value read would miss it and 200).
#[tokio::test]
async fn if_none_match_match_only_on_second_line_is_304() {
    let root = tempfile::tempdir().unwrap();
    spa_app(root.path());
    let app = build_pipeline(state(root.path().to_path_buf()));

    let res = send(
        &app,
        request("GET", "/spa-app/assets/app-AbCd1234.js", None, None),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let etag_value = etag(&res);
    drop(
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    );

    // First line: a non-matching weak etag. Second line: the real etag.
    let res = send(
        &app,
        get_inm(
            "/spa-app/assets/app-AbCd1234.js",
            vec![
                HeaderValue::from_static(r#"W/"0000000000000000""#),
                HeaderValue::from_str(&etag_value).unwrap(),
            ],
        ),
    )
    .await;
    assert_eq!(
        res.status(),
        StatusCode::NOT_MODIFIED,
        "the match on the second line must revalidate to 304"
    );
    assert_body_empty(res).await;
}

/// Two `If-None-Match` lines, the SECOND a malformed entity-tag (unquoted,
/// so `parse_entity_tag` rejects it): the joined value is malformed as a
/// whole → NO 304. A capture that ignored the second line and matched only
/// the first would wrongly 304, so this is the discriminating case. The
/// value is valid UTF-8 so it reaches the parser (it does not trip the
/// dispatch's non-UTF-8 request-header rejection).
#[tokio::test]
async fn if_none_match_malformed_second_line_is_not_304() {
    let root = tempfile::tempdir().unwrap();
    spa_app(root.path());
    let app = build_pipeline(state(root.path().to_path_buf()));

    let res = send(
        &app,
        request("GET", "/spa-app/assets/app-AbCd1234.js", None, None),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let etag_value = etag(&res);
    drop(
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    );

    // First line: the matching etag. Second line: an unquoted (malformed)
    // entity-tag. Joined, the list is malformed → the whole header is a
    // "no match", so the full body is re-sent.
    let res = send(
        &app,
        get_inm(
            "/spa-app/assets/app-AbCd1234.js",
            vec![
                HeaderValue::from_str(&etag_value).unwrap(),
                HeaderValue::from_static("deadbeef00"),
            ],
        ),
    )
    .await;
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "a malformed second line must not revalidate to 304"
    );
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!body.is_empty(), "the full body is re-sent");
}

// The 304's Vary mirrors the 200's eligibility with the SAME guards the
// compression layer applies (P3): `content-encoding` and `content-range` are
// rejected BEFORE the predicate runs, so a worker-provided `content-encoding`
// 200 is never compressible — neither the 200 nor its 304 gains `Vary:
// accept-encoding`.

/// A BUFFERED 200 whose worker already carries `content-encoding: gzip`
/// (pre-compressed) must gain `Vary: accept-encoding` NEITHER on the 200 (the
/// layer skips already-encoded bodies) NOR on the 304 (the eligibility check
/// mirrors the layer's content-encoding/content-range guards, not just the
/// predicate). A Deno worker supplies the worker's own content-encoding (a
/// static asset is never pre-encoded), and `ttl: 0ms` keeps the streamable
/// kind on the BUFFERED path — the only branch that can short-circuit to a
/// 304. Body is > 1 KiB and text so the predicate alone would say compressible;
/// the content-encoding guard is the only thing that keeps the Vary away.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn self_encoded_200_and_304_never_gain_vary_accept_encoding() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("encoded-app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: encoded-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nttl: 0ms\n",
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"Deno.serve(() => {
  const body = "z".repeat(4096);
  return new Response(body, {
    headers: {
      "content-type": "text/plain",
      "content-encoding": "gzip",
      "etag": "\"v1\"",
    },
  });
});
"#,
    )
    .unwrap();
    let app = build_pipeline(state(root.path().to_path_buf()));

    // The 200 carries the worker's own content-encoding and must NOT gain
    // Vary: accept-encoding (the layer skips already-encoded bodies).
    let res = send(&app, request("GET", "/encoded-app", Some("br, gzip"), None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        header_str(&res, "content-encoding"),
        Some("gzip"),
        "the worker's own content-encoding passes through untouched"
    );
    assert!(
        !vary_has_accept_encoding(&res),
        "a self-encoded 200 must not gain Vary: accept-encoding"
    );
    let etag_value = etag(&res);
    drop(
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    );

    // Revalidate with the worker's etag: 304. The 304 must ALSO not gain
    // Vary: accept-encoding — the eligibility check mirrors the layer's guard
    // (no content-encoding), so `compressible` is false.
    let res = send(
        &app,
        request("GET", "/encoded-app", Some("br, gzip"), Some(&etag_value)),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    assert!(
        !vary_has_accept_encoding(&res),
        "a self-encoded 304 must not gain Vary: accept-encoding"
    );
    assert_body_empty(res).await;
}
