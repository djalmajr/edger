//! Wasm execution integration tests.

#![cfg(feature = "wasm")]

use std::path::PathBuf;

use edger_core::{Isolate, SerializedRequest, WorkerConfig, WorkerManifest};
use edger_isolation::{WasiConfig, WasmIsolate};

fn fixture_config() -> WorkerConfig {
    let mut config = edger_core::parse_worker_config(&WorkerManifest {
        name: "wasm-hello".into(),
        entrypoint: Some("index.wat".into()),
        kind: Some("wasm".into()),
        ..Default::default()
    });
    config.worker_dir = Some(workspace_root().join("workers/wasm-hello"));
    config
}

fn missing_module_config() -> WorkerConfig {
    edger_core::parse_worker_config(&WorkerManifest {
        name: "wasm-missing".into(),
        ..Default::default()
    })
}

#[tokio::test]
async fn wasm_isolate_passes_request_uri_to_guest_module() {
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all());
    let req = SerializedRequest {
        method: "POST".into(),
        uri: "/proof-path?from=integration".into(),
        headers: vec![("x-proof".into(), "request-arrived".into())],
        body: Some(bytes::Bytes::from_static(b"body reaches memory frame")),
        request_id: "wasm-it".into(),
        base_href: None,
    };
    let res = isolate.execute_wasm(req, &fixture_config()).await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"wasm path: /proof-path?from=integration".as_ref())
    );
    assert!(res
        .headers
        .iter()
        .any(|(name, value)| name == "x-wasm-abi" && value == "v2"));
}

#[tokio::test]
async fn wasm_isolate_errors_without_module_bytes() {
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all());
    let req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "wasm-it".into(),
        base_href: None,
    };
    let err = isolate
        .execute_wasm(req, &missing_module_config())
        .await
        .unwrap_err();
    assert_eq!(err.code, "WASM_NOT_LOADED");
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("edger-isolation has a workspace parent")
        .to_path_buf()
}
