//! Wasm execution integration tests (story 07.05 v1).

#![cfg(feature = "wasm")]

use edger_core::{Isolate, SerializedRequest, WorkerConfig, WorkerManifest};
use edger_isolation::{WasiConfig, WasmIsolate};

fn default_config() -> WorkerConfig {
    edger_core::parse_worker_config(&WorkerManifest {
        name: "wasm-hello".into(),
        ..Default::default()
    })
}

const HELLO_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (data (i32.const 0) "wasm-hello")
      (func (export "http_status") (result i32) i32.const 200)
      (func (export "http_body_len") (result i32) i32.const 10)
    )
"#;

fn hello_wasm_bytes() -> Vec<u8> {
    wat::parse_str(HELLO_WAT).expect("valid WAT")
}

#[tokio::test]
async fn wasm_isolate_returns_body_from_module() {
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(hello_wasm_bytes());
    let req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "wasm-it".into(),
        base_href: None,
    };
    let res = isolate.execute_wasm(req, &default_config()).await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"wasm-hello".as_ref())
    );
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
        .execute_wasm(req, &default_config())
        .await
        .unwrap_err();
    assert_eq!(err.code, "WASM_NOT_LOADED");
}
