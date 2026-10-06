//! EDG-4: e2e HTTP — pre-compressed immutable assets through the full
//! pipeline (pattern of `compression_e2e.rs`).
//!
//! - a Vite-shaped SPA installed via the admin deploy API serves
//!   `Accept-Encoding: br` with `content-encoding: br`; the body decodes to
//!   the original byte-for-byte and is no larger than real-time (q4)
//!   compression of the same file;
//! - `If-None-Match` with the shared weak ETag returns 304 in both codings;
//! - a worker installed without variants (pre-EDG-4) stays real-time
//!   compressed by the `CompressionLayer`.
//!
//! The static SPA path is pure Rust (no Deno process is spawned), so these
//! tests run in the regular suite without `#[ignore]`.

use std::fs;
use std::io::{Read, Write};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use flate2::read::GzDecoder;
use tower::ServiceExt;

/// Static SPA workers never dispatch to an isolate.
struct ProbeIsolate;

#[async_trait]
impl Isolate for ProbeIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new(
            "UNEXPECTED_DISPATCH",
            "static SPA must not dispatch to an isolate",
        ))
    }

    async fn execute_routes(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new(
            "UNEXPECTED_ROUTES",
            "static SPA must not dispatch to an isolate",
        ))
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new(
            "UNEXPECTED_SPA",
            "static SPA is served by the Rust pipeline",
        ))
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new(
            "UNEXPECTED_WASM",
            "no wasm in this test",
        ))
    }
}

struct ProbeFactory;

impl IsolateFactory for ProbeFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(ProbeIsolate)
    }
}

fn state(root: std::path::PathBuf) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(ProbeFactory));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

fn js_body(size: usize) -> Vec<u8> {
    let mut body = String::from("/* edger bundle */\nconst seed = 'edger-4';\n");
    let mut index = 0;
    while body.len() < size {
        body.push_str(&format!(
            "export const chunk{index} = 'padding padding padding padding';\n"
        ));
        index += 1;
    }
    body.into_bytes()
}

fn brotli_encode(data: &[u8], quality: i32) -> Vec<u8> {
    let mut encoder = brotli::CompressorWriter::new(Vec::new(), 0, quality as u32, 22);
    encoder.write_all(data).unwrap();
    encoder.into_inner()
}

fn brotli_decode(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = brotli::Decompressor::new(bytes, 0);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).unwrap();
    out
}

fn gzip_decode(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = GzDecoder::new(bytes);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).unwrap();
    out
}

async fn get(
    app: Router,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer test-root");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let res = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (parts, body) = res.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    (parts.status, parts.headers, bytes.to_vec())
}

fn vary_has_accept_encoding(headers: &axum::http::HeaderMap) -> bool {
    headers.get_all(header::VARY).iter().any(|value| {
        value
            .to_str()
            .ok()
            .is_some_and(|value| value.to_ascii_lowercase().contains("accept-encoding"))
    })
}

#[tokio::test]
async fn deployed_spa_negotiates_precompressed_variants() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state(root.path().to_path_buf()));

    // Deploy a Vite-shaped SPA: fingerprinted assets + ineligible files.
    let app_original = js_body(8192);
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in [
            (
                "manifest.yaml",
                "name: spa4\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: static\n".as_bytes(),
            ),
            (
                "index.html",
                r#"<!doctype html><html><head></head><body><div id="root"></div></body></html>"#
                    .as_bytes(),
            ),
            ("assets/app-a1b2c3d4.js", app_original.as_slice()),
            ("assets/controller.js", js_body(900).as_slice()),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
    }
    let package = cursor.into_inner();
    let request = Request::builder()
        .method("POST")
        .uri("/api/admin/workers/install")
        .header("content-type", "application/zip")
        .header("authorization", "Bearer test-root");
    let install = app
        .clone()
        .oneshot(request.body(Body::from(package)).unwrap())
        .await
        .unwrap();
    let (install_parts, install_body) = install.into_parts();
    let install_bytes = axum::body::to_bytes(install_body, usize::MAX)
        .await
        .unwrap();
    let install_json: serde_json::Value =
        serde_json::from_slice(&install_bytes).expect("install response");
    assert_eq!(
        install_parts.status,
        StatusCode::CREATED,
        "install failed: {}",
        String::from_utf8_lossy(&install_bytes)
    );
    let source = std::path::Path::new(install_json["source"].as_str().unwrap());
    assert!(
        source.join("assets/app-a1b2c3d4.js.br").is_file(),
        "deploy must generate the .br variant"
    );
    assert!(
        source.join("assets/app-a1b2c3d4.js.gz").is_file(),
        "deploy must generate the .gz variant"
    );

    let uri = "/spa4/assets/app-a1b2c3d4.js";

    // 1. `Accept-Encoding: br` -> the brotli variant, decodable to the
    //    original byte-for-byte, no larger than real-time q4.
    let (status, headers, body) = get(app.clone(), uri, &[("accept-encoding", "br")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("br")
    );
    assert!(headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|content_type| content_type.starts_with("application/javascript")));
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable")
    );
    assert!(vary_has_accept_encoding(&headers));
    let etag = headers
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .expect("etag")
        .to_string();
    assert_eq!(brotli_decode(&body), app_original);
    assert!(
        body.len() <= brotli_encode(&app_original, 4).len(),
        "pre-compressed q11 ({} bytes) must be no larger than real-time q4 ({} bytes)",
        body.len(),
        brotli_encode(&app_original, 4).len()
    );
    // The served body IS the on-disk variant.
    assert_eq!(
        body,
        fs::read(source.join("assets/app-a1b2c3d4.js.br")).unwrap()
    );

    // 2. `br;q=0, gzip` -> the gzip variant.
    let (status, headers, body) =
        get(app.clone(), uri, &[("accept-encoding", "br;q=0, gzip")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
    assert_eq!(gzip_decode(&body), app_original);

    // 3. `identity` (and no header) -> the original, still varying.
    for accept_encoding in [
        ("accept-encoding", "identity"),
        ("accept-encoding", "br;q=0, gzip;q=0"),
    ] {
        let (status, headers, body) = get(app.clone(), uri, &[accept_encoding]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.get(header::CONTENT_ENCODING).is_none());
        assert!(vary_has_accept_encoding(&headers));
        assert_eq!(body, app_original);
    }
    let (status, headers, body) = get(app.clone(), uri, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::CONTENT_ENCODING).is_none());
    assert!(vary_has_accept_encoding(&headers));
    assert_eq!(body, app_original);

    // 4. The SAME weak ETag revalidates in every coding: 304 with br and
    //    with identity (and the 304 keeps the Vary of the 200).
    let (status, headers, body) = get(
        app.clone(),
        uri,
        &[("accept-encoding", "br"), ("if-none-match", etag.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED, "304 expected (br)");
    assert!(body.is_empty());
    assert_eq!(
        headers.get(header::ETAG).and_then(|v| v.to_str().ok()),
        Some(etag.as_str())
    );
    assert!(vary_has_accept_encoding(&headers));
    let (status, headers, body) = get(app.clone(), uri, &[("if-none-match", etag.as_str())]).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED, "304 expected (identity)");
    assert!(body.is_empty());
    assert_eq!(
        headers.get(header::ETAG).and_then(|v| v.to_str().ok()),
        Some(etag.as_str())
    );
    assert!(vary_has_accept_encoding(&headers));

    // 5. A direct request for the variant file serves the file as today.
    let (status, headers, body) = get(app.clone(), "/spa4/assets/app-a1b2c3d4.js.br", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::CONTENT_ENCODING).is_none());
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/octet-stream")
    );
    assert_eq!(
        body,
        fs::read(source.join("assets/app-a1b2c3d4.js.br")).unwrap()
    );

    // 6. Ineligible files keep the current behavior (no variant files).
    assert!(!source.join("assets/controller.js.br").exists());
    assert!(!source.join("assets/controller.js.gz").exists());
    assert!(!source.join("index.html.br").exists());
}

/// A worker installed WITHOUT variants (pre-EDG-4 layout) still gets the
/// real-time compression of the `CompressionLayer`.
#[tokio::test]
async fn worker_without_variants_stays_real_time_compressed() {
    let root = tempfile::tempdir().unwrap();
    let original = js_body(8192);
    let worker_dir = root.path().join("spa4rt");
    std::fs::create_dir_all(worker_dir.join("assets")).unwrap();
    std::fs::write(
        worker_dir.join("manifest.yaml"),
        "name: spa4rt\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: static\n",
    )
    .unwrap();
    std::fs::write(
        worker_dir.join("index.html"),
        "<!doctype html><html><head></head><body></body></html>",
    )
    .unwrap();
    std::fs::write(worker_dir.join("assets/app-a1b2c3d4.js"), &original).unwrap();

    let app = build_pipeline(state(root.path().to_path_buf()));
    let uri = "/spa4rt/assets/app-a1b2c3d4.js";

    let (status, headers, body) = get(app.clone(), uri, &[("accept-encoding", "gzip")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
    assert_eq!(gzip_decode(&body), original);

    let (status, headers, body) = get(app.clone(), uri, &[("accept-encoding", "br")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("br")
    );
    assert_eq!(brotli_decode(&body), original);

    let (status, _headers, body) = get(app, uri, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, original);
}
