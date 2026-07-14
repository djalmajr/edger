//! Timeout guard tests (story 03.03).

use edger_core::{parse_worker_config, ExecutionKind, SerializedRequest, WorkerManifest};
use edger_isolation::{execute_with_limits, MockIsolate, ResourceLimits};

#[tokio::test]
async fn execute_with_limits_returns_timeout_on_slow_mock() {
    let mut isolate = MockIsolate::new().with_slow_fetch_ms(200);
    let config = parse_worker_config(&WorkerManifest {
        name: "w".into(),
        ..Default::default()
    });
    let limits = ResourceLimits {
        wall_timeout_ms: 50,
        ..ResourceLimits::from_config(&config)
    };
    let req = SerializedRequest {
        method: "GET".into(),
        uri: "/slow".into(),
        headers: vec![],
        body: None,
        request_id: "t".into(),
        base_href: None,
    };
    let err = execute_with_limits(
        &mut isolate,
        ExecutionKind::FetchHandler,
        req,
        &config,
        &limits,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "TIMEOUT");
}
