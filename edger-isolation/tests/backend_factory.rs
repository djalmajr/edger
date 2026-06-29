//! Factory tests for IsolationBackend (story 03.04) — written first (TDD red).

use edger_core::{Isolate, SerializedRequest, WorkerConfig};
use edger_isolation::{create_isolate, IsolationBackend};

fn sample_req(uri: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: uri.into(),
        headers: vec![],
        body: None,
        request_id: "factory-req".into(),
        base_href: None,
    }
}

fn default_config() -> WorkerConfig {
    edger_core::parse_worker_config(&edger_core::WorkerManifest {
        name: "factory-worker".into(),
        ..Default::default()
    })
}

#[tokio::test]
async fn factory_mock_backend_executes_fetch() {
    let config = default_config();
    let mut isolate = create_isolate(IsolationBackend::Mock, &config);
    let req = sample_req("/hello");
    let res = isolate.execute_fetch(req, &config).await.unwrap();
    assert_eq!(res.status, 200);
    assert!(res.body.unwrap().starts_with(b"fetch:"));
}

#[test]
fn factory_mock_returns_box_dyn_isolate() {
    let config = default_config();
    let isolate = create_isolate(IsolationBackend::Mock, &config);
    let _: &dyn Isolate = isolate.as_ref();
}

#[cfg(feature = "deno")]
#[test]
fn deno_isolate_stub_compiles() {
    use edger_isolation::DenoIsolate;
    assert!(std::mem::size_of::<DenoIsolate>() > 0);
}

#[cfg(feature = "wasm")]
#[test]
fn wasm_isolate_stub_compiles() {
    use edger_isolation::WasmIsolate;
    assert!(std::mem::size_of::<WasmIsolate>() > 0);
}
