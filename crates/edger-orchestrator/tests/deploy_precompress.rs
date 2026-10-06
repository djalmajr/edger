//! EDG-4: deploy-time pre-compression of immutable assets.
//!
//! - installing a Vite-shaped SPA zip generates `.br`/`.gz` ONLY for the
//!   immutable assets (fingerprinted, compressible type, >= 1 KiB);
//! - a variant shipped by the package is kept as-is;
//! - a failing release rolls the install back and removes the variants with
//!   the target directory (the generation happens inside the staging
//!   transaction).

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use flate2::read::GzDecoder;
use tower::ServiceExt;

/// Static SPA workers never dispatch to an isolate; a probe that errors if
/// it ever does keeps this test free of Deno processes.
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

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, serde_json::Value, String) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", content_type);
    request = request.header("authorization", "Bearer test-root");
    let res = app
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json, text)
}

fn zip_package(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
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

/// Deterministic pseudo-random bytes: incompressible raster payload.
fn raster_body(size: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E3779B97F4A7C15;
    (0..size)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

const APP_JS: &str = "assets/app-a1b2c3d4.js";
const STYLE_CSS: &str = "assets/style-Bx2K9zM1.css";
const CONTROLLER_JS: &str = "assets/controller.js";
const LOGO_PNG: &str = "assets/logo-a1b2c3d4.png";
const MINI_JS: &str = "assets/mini-d4c3b2a1.js";
const VENDOR_JS: &str = "assets/vendor-c3b2a1d4.js";
const PREBUILT_BR: &[u8] = b"PREBUILT-BY-FRAMEWORK-BR";

fn spa_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "manifest.yaml",
            format!("name: spa4\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: static\n")
                .into_bytes(),
        ),
        (
            "index.html",
            r#"<!doctype html><html><head></head><body><div id="root"></div></body></html>"#
                .as_bytes()
                .to_vec(),
        ),
        (APP_JS, js_body(8192)),
        (STYLE_CSS, vec![b'.'; 4096]),
        (CONTROLLER_JS, js_body(4096)),
        (LOGO_PNG, raster_body(4096)),
        (MINI_JS, vec![b'n'; 100]),
        (VENDOR_JS, js_body(2048)),
        ("assets/vendor-c3b2a1d4.js.br", PREBUILT_BR.to_vec()),
    ]
}

fn spa_zip(name: &str, release: Option<&str>) -> Vec<u8> {
    let mut manifest =
        format!("name: {name}\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: static\n");
    if let Some(release) = release {
        manifest.push_str(&format!("release: \"{release}\"\n"));
    }
    let mut files = spa_files();
    files[0] = ("manifest.yaml", manifest.into_bytes());
    zip_package(&files)
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

#[tokio::test]
async fn vite_spa_install_generates_variants_only_for_immutable_assets() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state(root.path().to_path_buf()));

    let (status, json, text) = send(
        app,
        "POST",
        "/api/admin/workers/install",
        "application/zip",
        spa_zip("spa4", None),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");
    let source = Path::new(json["source"].as_str().expect("source dir"));

    let app_original = js_body(8192);
    let style_original = vec![b'.'; 4096];

    // Immutable, compressible, >= 1 KiB: both variants generated.
    let app_br = source.join(format!("{APP_JS}.br"));
    let app_gz = source.join(format!("{APP_JS}.gz"));
    assert!(app_br.is_file(), "missing {}", app_br.display());
    assert!(app_gz.is_file(), "missing {}", app_gz.display());
    assert_eq!(brotli_decode(&fs::read(&app_br).unwrap()), app_original);
    assert_eq!(gzip_decode(&fs::read(&app_gz).unwrap()), app_original);
    assert!(fs::read(&app_br).unwrap().len() < app_original.len());
    assert!(fs::read(&app_gz).unwrap().len() < app_original.len());
    let style_br = source.join(format!("{STYLE_CSS}.br"));
    let style_gz = source.join(format!("{STYLE_CSS}.gz"));
    assert!(style_br.is_file());
    assert!(style_gz.is_file());
    assert_eq!(brotli_decode(&fs::read(&style_br).unwrap()), style_original);

    // Package-provided variant: kept byte-for-byte, not regenerated.
    assert_eq!(
        fs::read(source.join(format!("{VENDOR_JS}.br"))).unwrap(),
        PREBUILT_BR
    );

    // Ineligible files: no variant at all.
    for absent in [
        format!("{CONTROLLER_JS}.br"), // not fingerprinted: never immutable
        format!("{CONTROLLER_JS}.gz"),
        format!("{LOGO_PNG}.br"), // raster image: never compressible
        format!("{LOGO_PNG}.gz"),
        format!("{MINI_JS}.br"), // below the 1 KiB floor
        format!("{MINI_JS}.gz"),
        "index.html.br".to_string(), // HTML entries are transformed at runtime
        "index.html.gz".to_string(),
        "manifest.yaml.br".to_string(),
    ] {
        assert!(!source.join(&absent).exists(), "{absent} must not exist");
    }
}

#[tokio::test]
async fn failing_release_rolls_back_the_precompressed_variants() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state(root.path().to_path_buf()));

    // `release: "false"` fails inside the transaction: the rollback must
    // remove the whole target — the generated variants included.
    let (status, _json, text) = send(
        app,
        "POST",
        "/api/admin/workers/install",
        "application/zip",
        spa_zip("spa4fail", Some("false")),
    )
    .await;
    assert_ne!(status, StatusCode::CREATED, "release must fail: {text}");

    let leftovers = fs::read_dir(root.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("spa4fail"))
        .count();
    assert_eq!(
        leftovers, 0,
        "rollback must remove the worker dir (variants included)"
    );
}
