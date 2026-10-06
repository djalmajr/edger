//! EDG-9 slice 2: when the orchestrator abandons a stream (the client
//! disconnects before the end frame), it sends a CANCEL control frame
//! (`{"__control":"cancel"}`) to the harness BEFORE draining. The harness
//! aborts the request's `AbortSignal`, cancels the body reader (without
//! awaiting — a producer may ignore the cancel) and answers with an end
//! frame carrying `{"cancelled":true}`. Inside the abandon drain that end is
//! a CLEAN end: the read half is restored, the process is reused, the
//! lifecycle is `stream_abandoned_drained` with the `cancelled` sub-cause and
//! the `abandoned_cancelled_total` counter moves.
//!
//! Endless streams (SSE) — which can never reach `TAG_END` on their own and
//! used to recycle the process at the drain's time limit (4–7 s cold start)
//! — now end at the cancel and the process is reused. A STALE cancel (the
//! response already ended before it arrived) is ignored by the harness's
//! main loop; socket order guarantees it precedes the next request frame.
//!
//! `x-seq` is a module-scope counter inside every worker: it only resets
//! when the Deno process is respawned, so it proves reuse vs recycle
//! end-to-end.
//!
//! Scenarios:
//! - an infinite SSE (50 ms ticks) read partially and abandoned: NO recycle
//!   — the next request uses the SAME process and the `cancelled` reason is
//!   recorded on the lifecycle;
//! - the worker's handler observes `request.signal.aborted === true` after
//!   the abandon (the state is kept in a module global and exposed on a
//!   second route);
//! - a producer that IGNORES the cancel (its source `cancel()` never
//!   settles): the harness still writes the end frame and the process is
//!   reused;
//! - stale cancels: 50 iterations alternating a tiny response whose body is
//!   dropped right after the header (the end frame may already be out the
//!   socket) with a normal GET checked for its EXACT body — every GET is
//!   correct and nothing is recycled;
//! - the drain disabled (`max_ms = 0`): the pre-slice-2 recycle applies and
//!   no cancel is written;
//! - (amendment) a 10 000-chunk body with no input frame: the body pump
//!   registers EXACTLY ONE frame observation (the harness counts its
//!   `nextFrame` registrations in test mode and emits
//!   `[harness-test] frame-observations=<n>` on stderr after the end frame
//!   when the `x-edger-test` request header is present — a per-chunk race
//!   would register one per chunk).
//!
//! Requires `deno` on PATH. Ignored by default; run explicitly.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{ConsoleLogContext, DenoProcessIsolate, StreamDetachBudget, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{
    IsolateFactory, PoolConfig, WorkerLifecycleEvent, WorkerLifecycleEventKind, WorkerPool,
};
use futures_util::StreamExt;
use tower::ServiceExt;

/// Factory whose persistent-process isolates use a detach pipeline AND the
/// abandon drain with the given limits (the cancel frame is written whenever
/// the drain is enabled, EDG-9 slice 2).
#[derive(Clone)]
struct CancelFactory {
    detach_max_bytes: u64,
    budget: Arc<StreamDetachBudget>,
    drain_max_bytes: u64,
    drain_max_ms: u64,
    /// Optional console capture (the focal proof reads the harness's
    /// stderr `[harness-test] frame-observations=<n>` record).
    console_tx: Option<edger_isolation::ConsoleLogSender>,
}

impl CancelFactory {
    fn new(detach_max_bytes: u64, drain_max_bytes: u64, drain_max_ms: u64) -> Self {
        Self {
            detach_max_bytes,
            budget: Arc::new(StreamDetachBudget::new(1024 * 1024)),
            drain_max_bytes,
            drain_max_ms,
            console_tx: None,
        }
    }

    fn with_console(mut self, sender: edger_isolation::ConsoleLogSender) -> Self {
        self.console_tx = Some(sender);
        self
    }
}

impl IsolateFactory for CancelFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => {
                let isolate = match self.console_tx.clone() {
                    Some(sender) => DenoProcessIsolate::with_console(
                        sender,
                        ConsoleLogContext {
                            namespace: None,
                            worker: worker_ref.name.clone(),
                            version: worker_ref.version.clone(),
                        },
                    ),
                    None => DenoProcessIsolate::new(),
                };
                Box::new(
                    isolate
                        .with_stream_detach(self.detach_max_bytes, Arc::clone(&self.budget))
                        // On consumer loss the drain writes the cancel control
                        // frame first (EDG-9 slice 2) and then drains.
                        .with_abandon_drain_limits(self.drain_max_bytes, self.drain_max_ms),
                )
            }
        }
    }
}

/// The pipeline + pool with the SAME abandon-drain limits the isolates get,
/// and a lifecycle capture channel: the e2e asserts the pool's real
/// termination reason (`stream_abandoned_drained`) and the `cancelled`
/// sub-cause on the `WorkerLifecycleEvent`s the bin's operational consumer
/// receives.
fn state_with_lifecycle(
    root: std::path::PathBuf,
    factory: CancelFactory,
) -> (
    OrchestratorState,
    tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
) {
    let server = ServerState::new_unready();
    let drain_max_bytes = factory.drain_max_bytes;
    let drain_max_ms = factory.drain_max_ms;
    let (lifecycle_tx, lifecycle_rx) = tokio::sync::mpsc::channel(64);
    let pool = WorkerPool::with_factory_and_lifecycle_abandon_drain(
        PoolConfig::default(),
        Arc::new(factory),
        Some(lifecycle_tx),
        edger_worker::AbandonDrainLimits {
            max_bytes: drain_max_bytes,
            max_ms: drain_max_ms,
        },
    );
    server.mark_ready(pool.clone());
    (
        OrchestratorState {
            server,
            pool,
            index: load_manifests_from_dirs(&[root]).unwrap(),
            auth: ControlAuth::with_static_key("test-root"),
        },
        lifecycle_rx,
    )
}

/// Return the first lifecycle event of `kind` within `ms`, skipping the
/// others (DrainStarted, …).
async fn lifecycle_event_of(
    rx: &mut tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
    kind: WorkerLifecycleEventKind,
    ms: u64,
) -> Option<WorkerLifecycleEvent> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if let Ok(event) = rx.try_recv() {
            if event.kind == kind {
                return Some(event);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Infinite SSE worker: one tick every 50 ms, forever — the stream can
/// never reach `TAG_END` on its own, so only the cancel ends it. `x-seq`
/// counts the served SSE requests (module-scope: resets only on respawn).
fn write_sse_worker(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmax_processes: 1\nqueue_timeout: 1s\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"let seq = 0;
Deno.serve(() => {
  seq += 1;
  const enc = new TextEncoder();
  const stream = new ReadableStream({
    async start(c) {
      for (let i = 0; ; i++) {
        c.enqueue(enc.encode(`data: tick-${i}\n\n`));
        await new Promise((r) => setTimeout(r, 50));
      }
    },
  });
  return new Response(stream, {
    headers: { "content-type": "text/event-stream", "x-seq": String(seq) },
  });
});
"#,
    )
    .unwrap();
}

/// (Amendment) Focal-proof worker: 10 000 x 100 B chunks back-to-back (fast,
/// in memory) — a long body with NO input frame, to prove the body pump
/// registers exactly ONE frame observation instead of one per chunk.
fn write_obs_count_worker(root: &std::path::Path, name: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!(
            "name: {name}\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmax_processes: 1\nqueue_timeout: 1s\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"let seq = 0;
Deno.serve(() => {
  seq += 1;
  const enc = new TextEncoder();
  const stream = new ReadableStream({
    start(c) {
      for (let i = 0; i < 10000; i++) {
        c.enqueue(enc.encode(`chunk-${i}-`.padEnd(100, ".")));
      }
      c.close();
    },
  });
  return new Response(stream, {
    headers: { "content-type": "text/event-stream", "x-seq": String(seq) },
  });
});
"#,
    )
    .unwrap();
}

async fn send(app: Router, uri: &str) -> axum::http::Response<Body> {
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("the request completes within 30s (cold start included)")
    .expect("request ok");
    response
}

/// The same `send` with one extra request header (the `x-edger-test`
/// frame-observation counter flag in the focal proof).
async fn send_with_header(
    app: &Router,
    uri: &str,
    header_name: &str,
    header_value: &str,
) -> axum::http::Response<Body> {
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.clone().oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .header(header_name, header_value)
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("the request completes within 30s (cold start included)")
    .expect("request ok");
    response
}

/// The core EDG-9 slice 2 scenario: an INFINITE SSE stream is read
/// partially and abandoned. The drain writes the cancel control frame, the
/// harness aborts the body and answers with the cancel end frame — a clean
/// end even though the stream is endless. NO recycle: the next request uses
/// the SAME process (x-seq = 2) and the `cancelled` reason rides the
/// lifecycle. Without the cancel (the pre-slice-2 behavior) the drain would
/// stop at the time limit and recycle — x-seq would reset and the stats
/// would show `time_limit`, killing this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn infinite_sse_abandon_cancels_and_reuses_the_process() {
    let root = tempfile::tempdir().unwrap();
    write_sse_worker(root.path(), "cancel-app");
    // Generous limits: the cancel ends the drain long before any limit.
    let factory = CancelFactory::new(8 * 1024 * 1024, 8 * 1024 * 1024, 2000);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/cancel-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first SSE tick within 15s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    assert!(
        String::from_utf8_lossy(&first).contains("tick-0"),
        "the first tick must be tick-0: {first:?}"
    );

    drop(body_a); // A abandons the (infinite) stream mid-production
                  // The drain writes the cancel, the harness answers with the
                  // cancel end frame — well inside the drain budget.
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_cancelled_total >= 1,
        "the drain must have stopped on the cancel end frame, stats: {stats:?}"
    );

    // (EDG-9 slice 2) The REUSE reason AND the NEW sub-cause must reach the
    // lifecycle the bin's operational consumer turns into the operational
    // event — the same path as the slice-1 drain sub-causes.
    let drained = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::DrainCompleted,
        2_000,
    )
    .await;
    let drained_ok = drained.as_ref().is_some_and(|event| {
        event.reason == "stream_abandoned_drained" && event.detail.as_deref() == Some("cancelled")
    });
    assert!(
        drained_ok,
        "the lifecycle must show stream_abandoned_drained with the cancelled sub-cause, got: {drained:?}"
    );

    // The SAME process must serve the next request (module-scope counter,
    // no reset) — no 4–7 s cold start.
    let c_started = Instant::now();
    let res_c = send(app, "/cancel-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "2",
        "x-seq must not reset: the infinite stream was cancelled and the process reused"
    );
    assert!(
        c_started.elapsed() < Duration::from_secs(5),
        "C was served by the warm process, took {:?}",
        c_started.elapsed()
    );
    let mut body_c = res_c.into_body().into_data_stream();
    let first_c = tokio::time::timeout(Duration::from_secs(5), body_c.next())
        .await
        .expect("C's first tick")
        .expect("stream open")
        .expect("chunk ok");
    assert!(
        String::from_utf8_lossy(&first_c).contains("tick-0"),
        "the new response starts at tick-0 again"
    );
    drop(body_c);
}

/// The worker's handler must OBSERVE the cancel: the request's
/// `AbortSignal` is aborted by the harness, and the module-scope flag (set
/// by the signal listener) is exposed on a second route.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn the_handler_observes_the_abort_signal_after_the_cancel() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("signal-app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: signal-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmax_processes: 1\nqueue_timeout: 1s\n",
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"let seq = 0;
let aborted = false;
Deno.serve((req) => {
  // The worker sees the worker-relative path (the orchestrator strips the
  // worker-name prefix): `/signal-app/state` arrives as `/state`.
  const url = new URL(req.url);
  if (url.pathname === "/state") {
    return new Response(JSON.stringify({ aborted, seq }), {
      headers: { "content-type": "application/json" },
    });
  }
  seq += 1;
  if (req.signal.aborted) aborted = true;
  req.signal.addEventListener("abort", () => { aborted = true; });
  const enc = new TextEncoder();
  const stream = new ReadableStream({
    async start(c) {
      for (let i = 0; ; i++) {
        c.enqueue(enc.encode(`data: tick-${i}\n\n`));
        await new Promise((r) => setTimeout(r, 50));
      }
    },
  });
  return new Response(stream, {
    headers: { "content-type": "text/event-stream", "x-seq": String(seq) },
  });
});
"#,
    )
    .unwrap();

    let factory = CancelFactory::new(8 * 1024 * 1024, 8 * 1024 * 1024, 2000);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/signal-app/sse").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first SSE tick within 15s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    assert!(
        String::from_utf8_lossy(&first).contains("tick-0"),
        "the first tick must be tick-0: {first:?}"
    );

    drop(body_a); // A abandons the stream; the harness aborts the request
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_cancelled_total >= 1,
        "the drain must have stopped on the cancel end frame, stats: {stats:?}"
    );
    let drained = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::DrainCompleted,
        2_000,
    )
    .await;
    assert!(
        drained
            .as_ref()
            .is_some_and(|event| event.detail.as_deref() == Some("cancelled")),
        "the lifecycle must carry the cancelled sub-cause, got: {drained:?}"
    );

    // The worker's own view: the request's signal WAS aborted (module global
    // exposed on the second route — same process, so no reset). The state
    // response is finite; the timeout guards against a body that never ends.
    let res_state = send(app, "/signal-app/state").await;
    assert_eq!(res_state.status(), StatusCode::OK);
    let body_state = tokio::time::timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(res_state.into_body(), usize::MAX),
    )
    .await
    .expect("the state response ends (finite JSON body)")
    .unwrap();
    let state: serde_json::Value =
        serde_json::from_slice(&body_state).expect("the state route returns JSON");
    assert_eq!(
        state.get("aborted"),
        Some(&serde_json::Value::Bool(true)),
        "the handler must have observed request.signal.aborted === true: {state:?}"
    );
}

/// A producer that IGNORES the cancel: the ReadableStream source's
/// `cancel()` never settles. The harness must NOT await `reader.cancel()` —
/// it writes the cancel end frame anyway, the drain ends cleanly and the
/// process is reused (a harness that awaits the cancel would hang past the
/// drain's time limit and recycle, resetting x-seq).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn a_producer_that_ignores_cancel_still_ends_cleanly_and_reuses_the_process() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("stubborn-app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: stubborn-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmax_processes: 1\nqueue_timeout: 1s\n",
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"let seq = 0;
Deno.serve(() => {
  seq += 1;
  const enc = new TextEncoder();
  const stream = new ReadableStream({
    async start(c) {
      for (let i = 0; ; i++) {
        c.enqueue(enc.encode(`data: tick-${i}\n\n`));
        await new Promise((r) => setTimeout(r, 50));
      }
    },
    // Ignores the cancel: never settles. The harness must not await it.
    cancel() {
      return new Promise(() => {});
    },
  });
  return new Response(stream, {
    headers: { "content-type": "text/event-stream", "x-seq": String(seq) },
  });
});
"#,
    )
    .unwrap();

    let factory = CancelFactory::new(8 * 1024 * 1024, 8 * 1024 * 1024, 2000);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/stubborn-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first SSE tick within 15s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    assert!(
        String::from_utf8_lossy(&first).contains("tick-0"),
        "the first tick must be tick-0: {first:?}"
    );

    drop(body_a); // A abandons the stream
                  // The harness writes the end frame WITHOUT awaiting the
                  // (never-settling) reader.cancel(); 1.5 s is a wide margin.
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_cancelled_total >= 1,
        "the drain must have stopped on the cancel end frame, stats: {stats:?}"
    );
    let drained = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::DrainCompleted,
        2_000,
    )
    .await;
    assert!(
        drained
            .as_ref()
            .is_some_and(|event| event.detail.as_deref() == Some("cancelled")),
        "the lifecycle must carry the cancelled sub-cause, got: {drained:?}"
    );

    // The SAME process must serve the next request (no recycle).
    let res_c = send(app, "/stubborn-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "2",
        "x-seq must not reset: the ignoring producer did not block the cancel end"
    );
    let mut body_c = res_c.into_body().into_data_stream();
    let first_c = tokio::time::timeout(Duration::from_secs(5), body_c.next())
        .await
        .expect("C's first tick")
        .expect("stream open")
        .expect("chunk ok");
    assert!(
        String::from_utf8_lossy(&first_c).contains("tick-0"),
        "the new response starts at tick-0 again"
    );
    drop(body_c);
}

/// The STALE cancel scenario: the drain writes the cancel control frame
/// AFTER the harness already sent the natural end frame, so the harness's
/// MAIN LOOP (no active stream) receives the cancel and must ignore it.
///
/// Mechanism (deterministic): the flash response is 32 instant 300-byte
/// chunks; the body channel between the forwarder and the client holds only
/// 16 chunks (multiproc.rs), and the test never reads the flash body. The
/// harness writes all C frames + the natural E within microseconds of the
/// request; the forwarder fills the 16-slot channel and blocks, holding
/// permits, so the reader parks on the per-response reservation (the 512B
/// cap) while mid-stream. The test then drops the response: the forwarder's
/// blocked delivery fails, the consumer-gone cancel fires, and the parked
/// reader enters the drain — which writes the cancel AFTER the natural E
/// went out. The harness's main loop sees the stale cancel (socket order
/// guarantees it precedes the next request frame) and must ignore it: the
/// drain reads the plain end frame and the process is reused, so x-seq runs
/// 1..=50 on ONE process.
///
/// A stale cancel misread as a REQUEST would hit the worker's `/` route (a
/// cancel frame has no uri — `http://edger.local/`) and its distinguishable
/// `PHANTOM` response would desync the socket, breaking the x-seq/body
/// assertions and killing the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn stale_cancels_do_not_corrupt_the_next_requests() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("stale-app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: stale-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmax_processes: 1\nqueue_timeout: 1s\n",
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        r#"let seq = 0;
const PAYLOAD = new Array(4096).fill("P").join("");
Deno.serve((req) => {
  // The worker sees the worker-relative path (the orchestrator strips the
  // worker-name prefix): `/stale-app/f` arrives as `/f`.
  const url = new URL(req.url);
  if (url.pathname === "/") {
    // A stale cancel MISREAD as a request has no uri (`http://edger.local/
    // `) and lands here: distinguishable, so the desync breaks the test.
    return new Response("PHANTOM", {
      headers: { "content-type": "text/plain" },
    });
  }
  if (url.pathname === "/f") {
    // 32 instant chunks (> the 16-slot body channel): the harness writes
    // all the C frames and the natural E in microseconds; the orchestrator
    // forwarder fills the channel and blocks while the test drops the
    // response — the drain fires mid-stream and its cancel goes stale.
    const stream = new ReadableStream({
      start(controller) {
        for (let i = 0; i < 32; i++) {
          controller.enqueue(new Uint8Array(300).fill(0x78));
        }
        controller.close();
      },
    });
    return new Response(stream, {
      headers: { "content-type": "application/octet-stream" },
    });
  }
  seq += 1;
  return new Response(PAYLOAD, {
    headers: { "content-type": "text/plain", "x-seq": String(seq) },
  });
});
"#,
    )
    .unwrap();

    // The 512-byte per-response cap (first arg) is what makes the reader
    // PARK on the reservation while the 16-slot body channel fills: the
    // 300-byte chunks cannot pass while the forwarder blocks undelivered.
    let factory = CancelFactory::new(512, 8 * 1024 * 1024, 2000);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let expected: Vec<u8> = vec![b'P'; 4096];
    for i in 1..=50u8 {
        // The tiny response: abandon it right after the header. The drain
        // writes the cancel; if the end frame already went out, the cancel
        // is stale and the harness must ignore it (and the drain reads the
        // plain end frame instead).
        let res_flash = send(app.clone(), "/stale-app/f").await;
        assert_eq!(
            res_flash.status(),
            StatusCode::OK,
            "the flash response {i} must be 200"
        );
        drop(res_flash);

        // The normal GET: the EXACT body, in order, on the same process.
        let res_p = send(app.clone(), "/stale-app/p").await;
        assert_eq!(
            res_p.status(),
            StatusCode::OK,
            "GET {i} must be 200 (a stale cancel must not be misread as a request)"
        );
        assert_eq!(
            res_p.headers().get("x-seq").unwrap(),
            i.to_string().as_str(),
            "x-seq must run 1..=50 on ONE process: no recycle across the 50 stale-cancel races"
        );
        let body_p = axum::body::to_bytes(res_p.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            body_p.as_ref(),
            expected.as_slice(),
            "GET {i} must return the exact body"
        );
    }

    // Nothing may have been recycled for ANY reason (drain failure or
    // otherwise): the recycle sub-causes must all be zero. AND at least one
    // drain must have run and ended on the NATURAL end frame (`drained`,
    // not `cancelled`): that is the proof the cancel went stale — the
    // harness's main loop ignored it and the drain read the plain E.
    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_drained_total >= 1,
        "at least one drain must have ended on the natural end frame (the stale cancel ignored by the harness's main loop), stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_socket_poisoned_total, 0,
        "no socket may have been poisoned, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drain_time_limit_total, 0,
        "no drain may have hit the time limit, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drain_bytes_limit_total, 0,
        "no drain may have hit the byte limit, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drain_stream_error_total, 0,
        "no drain may have hit a stream error, stats: {stats:?}"
    );

    // No recycle lifecycle event may have fired either. (The channel may
    // still hold the DrainCompleted events of the drains that ran; a
    // Terminated event is what a recycle would emit.)
    while let Ok(event) = lifecycle_rx.try_recv() {
        assert_ne!(
            event.kind,
            WorkerLifecycleEventKind::Terminated,
            "no recycle may have happened across the stale-cancel loop, got: {event:?}"
        );
    }
}

/// The drain DISABLED (`max_ms = 0`): the pre-slice-2 behavior applies —
/// the socket is abandoned mid-response, the process is RECYCLED and no
/// cancel is written (the infinite stream keeps producing into the dead
/// socket until the terminate kills it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn disabled_drain_recycles_without_cancel() {
    let root = tempfile::tempdir().unwrap();
    write_sse_worker(root.path(), "legacy-sse-app");
    // max_ms = 0: the drain is disabled (either limit at 0 disables it) —
    // and with it, the cancel frame.
    let factory = CancelFactory::new(8 * 1024 * 1024, 8 * 1024 * 1024, 0);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/legacy-sse-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first SSE tick within 15s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    assert!(
        String::from_utf8_lossy(&first).contains("tick-0"),
        "the first tick must be tick-0: {first:?}"
    );

    drop(body_a); // A abandons the (infinite) stream
                  // The pre-slice-2 recycle (terminate) follows.
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_socket_poisoned_total >= 1,
        "with the drain disabled the socket is poisoned, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_cancelled_total, 0,
        "with the drain disabled no cancel is written and no cancel end counted"
    );

    // The recycle reason AND the sub-cause must reach the lifecycle.
    let terminated = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::Terminated,
        2_000,
    )
    .await;
    let terminated_ok = terminated.as_ref().is_some_and(|event| {
        event.reason == "socket_poisoned" && event.detail.as_deref() == Some("socket_poisoned")
    });
    assert!(
        terminated_ok,
        "the disabled-drain recycle must show the socket_poisoned reason and sub-cause, got: {terminated:?}"
    );

    // A FRESH process must serve the next request (x-seq reset to 1).
    let res_c = send(app, "/legacy-sse-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "1",
        "x-seq must reset: the drain-less path recycles, as before slice 2"
    );
    let mut body_c = res_c.into_body().into_data_stream();
    let first_c = tokio::time::timeout(Duration::from_secs(5), body_c.next())
        .await
        .expect("C's first tick")
        .expect("stream open")
        .expect("chunk ok");
    assert!(
        String::from_utf8_lossy(&first_c).contains("tick-0"),
        "the fresh process starts at tick-0"
    );
    drop(body_c);
}

// (Amendment) The body pump registers EXACTLY ONE frame observation per
// response: a 10 000-chunk body with no input frame must not accumulate
// one `.then` reaction per chunk on the durable frame promise (they would
// stay retained until the next frame and grow memory with the chunk count).
// The harness counts its `nextFrame` registrations in test mode — armed for
// the response only — and emits `[harness-test] frame-observations=<n>` on
// stderr after the end frame when the `x-edger-test` request header is
// present (captured here as a console record); a per-chunk race registers
// one per chunk (10 001+) and fails this assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn a_10k_chunk_body_registers_exactly_one_frame_observation() {
    let root = tempfile::tempdir().unwrap();
    write_obs_count_worker(root.path(), "obs-app");
    let (console_tx, mut console_rx) = tokio::sync::mpsc::channel(128);
    let factory =
        CancelFactory::new(8 * 1024 * 1024, 8 * 1024 * 1024, 5_000).with_console(console_tx);
    let (state, _lifecycle_rx) = state_with_lifecycle(root.path().to_path_buf(), factory);
    let app = build_pipeline(state);

    let res_a = send_with_header(&app, "/obs-app", "x-edger-test", "on").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res_a.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body.len(), 10_000 * 100, "the full 10k-chunk body");

    // The harness emits the count right after the end frame; poll the
    // console records briefly.
    let deadline = Instant::now() + Duration::from_secs(10);
    let count = loop {
        if let Ok(record) = console_rx.try_recv() {
            if let Some(count) = record
                .message
                .strip_prefix("[harness-test] frame-observations=")
            {
                break count.to_string();
            }
        }
        assert!(
            Instant::now() < deadline,
            "the frame-observation count must arrive within 10s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        count, "1",
        "exactly ONE frame observation for the whole body (a per-chunk race registers one per chunk)"
    );
}
