use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use edger_core::WorkerManifest;
use edger_isolation::MockIsolate;
use edger_orchestrator::observability::{
    OperationalEventInput, OperationalEventLevel, OperationalEventSource, OperationalStore,
    OperationalStoreConfig,
};
use edger_orchestrator::{
    build_pipeline, ControlAuth, ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::Value;
use tower::ServiceExt;

const ROOT_KEY: &str = "test-root";

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn input(worker: &str, version: &str, request_id: &str) -> OperationalEventInput {
    OperationalEventInput {
        source: OperationalEventSource::Runtime,
        kind: "dispatch".into(),
        level: OperationalEventLevel::Info,
        namespace: Some("default".into()),
        worker: Some(worker.into()),
        version: Some(version.into()),
        process_id: None,
        request_id: Some(request_id.into()),
        trace_id: None,
        outcome: Some("ok".into()),
        status: Some(200),
        duration_ms: Some(3),
        code: None,
        message: None,
        truncated: None,
        dropped_count: None,
    }
}

#[test]
fn store_enforces_global_and_per_identity_limits_with_stable_cursor() {
    let store = OperationalStore::new(OperationalStoreConfig {
        global_capacity: 3,
        per_identity_capacity: 2,
    });

    store.record(input("alpha", "1.0.0", "a-1"));
    store.record(input("alpha", "1.0.0", "a-2"));
    store.record(input("alpha", "1.0.0", "a-3"));
    store.record(input("beta", "2.0.0", "b-1"));

    let first = store.query(Default::default());
    assert_eq!(first.events.len(), 3);
    assert_eq!(first.events[0].request_id.as_deref(), Some("b-1"));
    assert_eq!(first.events[1].request_id.as_deref(), Some("a-3"));
    assert_eq!(first.events[2].request_id.as_deref(), Some("a-2"));
    assert_eq!(first.stats.evicted, 1);

    let cursor = first.events[1].id;
    let page = store.query(edger_orchestrator::observability::OperationalEventQuery {
        before: Some(cursor),
        ..Default::default()
    });
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].request_id.as_deref(), Some("a-2"));
}

#[test]
fn store_recording_cost_remains_bounded_at_capacity() {
    let store = OperationalStore::default();
    let started = std::time::Instant::now();
    for index in 0..10_000 {
        store.record(input(
            &format!("worker-{}", index % 20),
            "1.0.0",
            &format!("request-{index}"),
        ));
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "10k bounded event inserts took {elapsed:?}"
    );
    let page = store.query(Default::default());
    assert_eq!(page.stats.size, 2_000);
    assert_eq!(page.stats.evicted, 8_000);
    eprintln!("10k bounded event inserts: {elapsed:?}");
}

#[test]
fn store_accounts_for_console_drops_and_truncations() {
    let store = OperationalStore::default();
    let mut event = input("alpha", "1.0.0", "console-1");
    event.source = OperationalEventSource::Console;
    event.truncated = Some(true);
    event.dropped_count = Some(7);
    store.record(event);

    let page = store.query(Default::default());
    assert_eq!(page.stats.dropped, 7);
    assert_eq!(page.stats.truncated, 1);
}

#[test]
fn tail_is_ordered_deduplicated_and_declares_expired_cursor_gap() {
    let store = OperationalStore::new(OperationalStoreConfig {
        global_capacity: 3,
        per_identity_capacity: 3,
    });
    for index in 1..=5 {
        store.record(input("alpha", "1.0.0", &format!("tail-{index}")));
    }

    let gap = store.tail(Default::default(), 1);
    assert!(gap.gap);
    assert_eq!(gap.oldest_available, Some(3));
    assert_eq!(
        gap.events.iter().map(|event| event.id).collect::<Vec<_>>(),
        vec![3, 4, 5]
    );

    let resumed = store.tail(Default::default(), 4);
    assert!(!resumed.gap);
    assert_eq!(resumed.events.len(), 1);
    assert_eq!(resumed.events[0].request_id.as_deref(), Some("tail-5"));
}

#[test]
fn series_is_bounded_identity_scoped_and_marks_partial_runtime_window() {
    let store = OperationalStore::new(OperationalStoreConfig {
        global_capacity: 10,
        per_identity_capacity: 10,
    });
    let mut success = input("alpha", "1.0.0", "series-ok");
    success.duration_ms = Some(10);
    store.record(success);
    let mut failure = input("alpha", "1.0.0", "series-error");
    failure.level = OperationalEventLevel::Error;
    failure.status = Some(500);
    failure.outcome = Some("http_5xx".into());
    failure.duration_ms = Some(90);
    store.record(failure);
    store.record(input("beta", "1.0.0", "series-other"));

    let series = store.series(
        edger_orchestrator::observability::OperationalEventQuery {
            worker: Some("alpha".into()),
            version: Some("1.0.0".into()),
            ..Default::default()
        },
        60_000,
        15_000,
    );
    assert_eq!(series.window_ms, 60_000);
    assert_eq!(series.bucket_ms, 15_000);
    assert!(series.partial_window);
    assert_eq!(
        series
            .points
            .iter()
            .map(|point| point.request_count)
            .sum::<u64>(),
        2
    );
    assert_eq!(
        series
            .points
            .iter()
            .map(|point| point.error_count)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        series
            .points
            .iter()
            .filter_map(|point| point.duration_p95_ms)
            .max(),
        Some(90)
    );
}

fn state() -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/alpha"),
            WorkerManifest {
                name: "alpha".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index,
        auth: ControlAuth::with_static_key(ROOT_KEY),
    }
}

async fn send(app: axum::Router, uri: &str, key: Option<&str>) -> (StatusCode, Value, String) {
    let mut request = Request::builder().method("GET").uri(uri);
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

#[tokio::test]
async fn events_api_is_root_only_filterable_and_allowlisted() {
    let state = state();
    let mut event = input("alpha", "1.0.0", "request-1");
    event.level = OperationalEventLevel::Error;
    event.status = Some(500);
    event.outcome = Some("worker_error".into());
    event.message = Some("boom authorization=secret-value\nbody-secret".into());
    state.server.operational_events().record(event);
    state
        .server
        .operational_events()
        .record(input("beta", "2.0.0", "request-2"));
    let mut other_namespace = input("alpha", "1.0.0", "request-other-namespace");
    other_namespace.namespace = Some("other".into());
    other_namespace.level = OperationalEventLevel::Error;
    other_namespace.status = Some(500);
    other_namespace.outcome = Some("worker_error".into());
    state.server.operational_events().record(other_namespace);
    let app = build_pipeline(state);

    let (status, _, _) = send(
        app.clone(),
        "/api/admin/observability/events?namespace=default&worker=alpha&version=1.0.0&source=runtime&kind=dispatch&level=error&status=500",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, json, text) = send(
        app,
        "/api/admin/observability/events?namespace=default&worker=alpha&version=1.0.0&source=runtime&kind=dispatch&level=error&status=500",
        Some(ROOT_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let events = json["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["requestId"], "request-1");
    assert_eq!(events[0]["worker"], "alpha");
    assert!(!text.contains("secret-value"));
    assert!(!text.contains("body-secret"));
    assert!(json.get("stats").is_some());
}

#[tokio::test]
async fn series_api_is_root_only_and_returns_reset_aware_buckets() {
    let state = state();
    state
        .server
        .operational_events()
        .record(input("alpha", "1.0.0", "series-api"));
    let app = build_pipeline(state);
    let uri =
        "/api/admin/observability/series?worker=alpha&version=1.0.0&windowMs=60000&bucketMs=15000";

    let (status, _, _) = send(app.clone(), uri, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, json, text) = send(app, uri, Some(ROOT_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["windowMs"], 60_000);
    assert_eq!(json["bucketMs"], 15_000);
    assert_eq!(json["partialWindow"], true);
    assert_eq!(
        json["points"]
            .as_array()
            .expect("series points")
            .iter()
            .map(|point| point["requestCount"].as_u64().unwrap_or_default())
            .sum::<u64>(),
        1
    );
}

#[tokio::test]
async fn real_dispatch_is_queryable_by_response_request_id() {
    let app = build_pipeline(state());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/alpha")
                .header("x-request-id", "correlated-request")
                .header(
                    "traceparent",
                    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("correlated-request")
    );

    let (status, json, text) = send(
        app,
        "/api/admin/observability/events?worker=alpha&version=1.0.0&requestId=correlated-request&traceId=4bf92f3577b34da6a3ce929d0e0e4736",
        Some(ROOT_KEY),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let events = json["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], "dispatch");
    assert_eq!(events[0]["requestId"], "correlated-request");
    assert_eq!(events[0]["traceId"], "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(events[0]["outcome"], "ok");
    assert_eq!(events[0]["status"], 200);
}
