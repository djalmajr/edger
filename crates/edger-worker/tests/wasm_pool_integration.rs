//! Wasm worker pool integration (story 07.05).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use edger_core::{parse_worker_config, ExecutionKind, SerializedRequest, WorkerManifest};
use edger_isolation::{WasiConfig, WasmIsolate};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};

const HELLO_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (data (i32.const 64) "[]")
      (data (i32.const 96) "wasm-hello")

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
        i32.const 10
        i32.store
        i32.const 524
        i32.const 64
        i32.const 2
        call $copy
        i32.const 526
        i32.const 96
        i32.const 10
        call $copy

        i32.const 512
        i64.extend_i32_u
        i64.const 24
        i64.const 32
        i64.shl
        i64.or
      )
    )
"#;

struct WasmHelloFactory;

impl IsolateFactory for WasmHelloFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
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

#[tokio::test]
async fn pool_fetch_uses_wasm_kind_from_worker_config_without_hint() {
    let dir = tempfile::tempdir().unwrap();
    let worker_dir = dir.path().to_path_buf();
    write_wasm_fixture(&worker_dir);

    let manifest: WorkerManifest =
        serde_yaml::from_str(&fs::read_to_string(worker_dir.join("manifest.yaml")).unwrap())
            .unwrap();
    let config = parse_worker_config(&manifest);

    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(WasmHelloFactory));
    let req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "pool-wasm-no-hint".into(),
        base_href: None,
    };

    let res = pool.fetch(&worker_dir, &config, req, None).await.unwrap();

    assert_eq!(res.status, 200);
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"wasm-hello".as_ref())
    );
}
