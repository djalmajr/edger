//! Wasm worker pool integration (story 07.05).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use edger_core::{
    parse_worker_config, ExecutionKind, SerializedRequest, WorkerConfig, WorkerManifest,
};
use edger_isolation::{WasiConfig, WasmIsolate};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};

const HELLO_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (data (i32.const 0) "wasm-hello")
      (func (export "http_status") (result i32) i32.const 200)
      (func (export "http_body_len") (result i32) i32.const 10)
    )
"#;

struct WasmHelloFactory;

impl IsolateFactory for WasmHelloFactory {
    fn create_isolate(&self) -> Box<dyn edger_core::Isolate> {
        Box::new(WasmIsolate::new(WasiConfig::deny_all()))
    }
}

fn write_wasm_fixture(dir: &PathBuf) {
    fs::create_dir_all(dir).expect("mkdir");
    let wasm_bytes = wat::parse_str(HELLO_WAT).expect("wat");
    fs::write(dir.join("index.wasm"), wasm_bytes).expect("write wasm");
    fs::write(
        dir.join("manifest.yaml"),
        r#"name: wasm-hello
version: "1.0.0"
entrypoint: index.wasm
kind: wasm
"#,
    )
    .expect("write manifest");
}

#[tokio::test]
async fn pool_fetch_wasm_worker_returns_hello_body() {
    let dir = tempfile::tempdir().unwrap();
    let worker_dir = dir.path().to_path_buf();
    write_wasm_fixture(&worker_dir);

    let manifest: WorkerManifest =
        serde_yaml::from_str(&fs::read_to_string(worker_dir.join("manifest.yaml")).unwrap())
            .unwrap();
    let mut config = parse_worker_config(&manifest);
    config.worker_dir = Some(worker_dir.clone());

    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(WasmHelloFactory));
    let req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "pool-wasm".into(),
        base_href: None,
    };

    let res = pool
        .fetch(
            &worker_dir,
            &config,
            req,
            Some(ExecutionKind::WasmModule {
                entry: Some("index.wasm".into()),
            }),
        )
        .await
        .unwrap();

    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"wasm-hello".as_ref())
    );
}
