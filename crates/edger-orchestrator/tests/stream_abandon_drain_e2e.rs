//! EDG-9: when a streaming response body is discarded BEFORE the pool sees
//! the end (client disconnect, `HEAD` on a streaming page, fetch abort,
//! 304 from the worker), the reader no longer poisons the socket and forces
//! a process recycle (4–7 s cold start on the next request). Instead, for a
//! finite response the reader DRAINS the abandoned response in discard mode
//! — keep reading and discarding frames, no budget/queue — until the clean
//! `TAG_END`, bounded by `EDGER_STREAM_ABANDON_DRAIN_MAX_BYTES` and
//! `EDGER_STREAM_ABANDON_DRAIN_MAX_MS` (`0` in either one disables the
//! drain, restoring the pre-EDG-9 recycle).
//!
//! `x-seq` is a module-scope counter inside every worker: it only resets
//! when the Deno process is respawned, so it proves reuse vs recycle
//! end-to-end.
//!
//! Scenarios (slice 2: the drain writes the CANCEL control frame first —
//! a harness that answers it ends the response with the cancel end frame,
//! which is ALSO a clean end):
//! - an early client disconnect mid-production: the harness answers the
//!   cancel, the process is REUSED (x-seq continues) — and this is the
//!   mutation sentinel: a pool that does not wait for the drain, or a
//!   reader that does not drain, both recycle and reset x-seq;
//! - a `HEAD` request on the same streaming page: the body never
//!   materializes, the drain is plain (`drained`) and the process is
//!   reused;
//! - an oversized abandoned response (a SINGLE chunk > the byte limit; the
//!   harness writes all its frames before the next socket frame): the
//!   drain hits the BYTE limit and the process is RECYCLED (x-seq resets);
//! - a producer that FREEZES its event loop (a synchronous busy-wait
//!   longer than the time limit — it cannot process the cancel): the
//!   drain hits the TIME limit and the process is RECYCLED;
//! - an infinite (SSE) stream: the harness answers the cancel, the
//!   process is REUSED (x-seq continues);
//! - an in-stream pull error before the cancel is read (the `E {error}`
//!   is in flight): the drain stops on the error end and the process is
//!   RECYCLED (`stream_error`);
//! - limits `0`: the drain is disabled and the pre-EDG-9 recycle applies.
//!
//! The guard scenarios (byte/time limits, `stream_error`) share a
//! deterministic drain-entry fixture: a burst of 20 × 300 B chunks fills
//! the 16-slot body channel, the forwarder blocks holding detach-cap
//! permits, and the reader parks on a reserve — the drop fails the stuck
//! send (the discard flag is set) and the parked reader wakes and drains
//! instead of racing the flag.
//!
//! Requires `deno` on PATH. Ignored by default; run explicitly.

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, StreamDetachBudget, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{
    IsolateFactory, PoolConfig, WorkerLifecycleEvent, WorkerLifecycleEventKind, WorkerPool,
};
use futures_util::StreamExt;
use tower::ServiceExt;

/// 16 KiB chunks: small enough that the whole test corpus stays in the
/// megabytes, large enough to make the byte-limit test a few chunks deep.
const CHUNK_BYTES: usize = 16 * 1024;

/// Factory whose persistent-process isolates use a detach pipeline AND the
/// EDG-9 abandon drain with the given limits.
#[derive(Clone)]
struct DrainFactory {
    detach_max_bytes: u64,
    budget: Arc<StreamDetachBudget>,
    drain_max_bytes: u64,
    drain_max_ms: u64,
}

impl DrainFactory {
    fn new(detach_max_bytes: u64, drain_max_bytes: u64, drain_max_ms: u64) -> Self {
        Self {
            detach_max_bytes,
            budget: Arc::new(StreamDetachBudget::new(1024 * 1024)),
            drain_max_bytes,
            drain_max_ms,
        }
    }
}

impl IsolateFactory for DrainFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(
                DenoProcessIsolate::new()
                    .with_stream_detach(self.detach_max_bytes, Arc::clone(&self.budget))
                    // EDG-9: on consumer loss, drain the abandoned response
                    // (discard-mode to a clean end frame) instead of
                    // poisoning the socket and recycling the process.
                    .with_abandon_drain_limits(self.drain_max_bytes, self.drain_max_ms),
            ),
        }
    }
}

/// The pipeline + pool with the SAME abandon-drain limits the isolates get,
/// and a lifecycle capture channel: the e2e asserts the pool's real
/// termination reason (`stream_abandoned_drained` / `stream_abandoned_`)
/// and the drain sub-cause (`bytes_limit`, `time_limit`, `stream_error`,
/// `socket_poisoned`) on the `WorkerLifecycleEvent`s the bin's operational
/// consumer receives.
fn state_with_lifecycle(
    root: std::path::PathBuf,
    factory: DrainFactory,
) -> (
    OrchestratorState,
    tokio::sync::mpsc::Receiver<WorkerLifecycleEvent>,
) {
    let server = ServerState::new_unready();
    // (EDG-9) The pool's completion wait must mirror the isolates' drain
    // budget: a body dropped before production completed waits up to
    // `drain_max_ms + grace` for the completion signal before recycling —
    // and `Duration::ZERO` when either limit is `0` (drain disabled).
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

/// Worker streaming `chunks` x 16 KiB; every chunk carries its own index in
/// the first byte (the rest is 0x78). `pause_ms` > 0 stretches production so
/// an early disconnect happens MID-production (before `TAG_END`). `x-seq`
/// is a module-scope counter: it only resets when the Deno process is
/// respawned, so it proves whether the SAME persistent process served a
/// request. One process only; the queue timeout is irrelevant here (at most
/// one in-flight request per test).
fn write_stream_worker(root: &std::path::Path, name: &str, chunks: usize, pause_ms: u64) {
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
        format!(
            r#"let seq = 0;
const CHUNKS = {chunks};
const CHUNK_BYTES = {chunk_bytes};
const PAUSE_MS = {pause_ms};
Deno.serve(() => {{
  seq += 1;
  const stream = new ReadableStream({{
    async start(c) {{
      for (let i = 0; i < CHUNKS; i++) {{
        const chunk = new Uint8Array(CHUNK_BYTES).fill(0x78);
        chunk[0] = i;
        c.enqueue(chunk);
        if (PAUSE_MS > 0) await new Promise((r) => setTimeout(r, PAUSE_MS));
      }}
      c.close();
    }},
  }});
  return new Response(stream, {{
    headers: {{ "content-type": "text/plain", "x-seq": String(seq) }},
  }});
}});
"#,
            chunks = chunks,
            chunk_bytes = CHUNK_BYTES,
            pause_ms = pause_ms,
        ),
    )
    .unwrap();
}

/// Infinite SSE worker: the stream never closes, so an abandon drain can
/// never reach `TAG_END` and must always stop at its time limit.
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
    // 200 ms ticks: the real slow-producer case. The infinite stream still
    // can never reach TAG_END, and the frame interval exceeds the pool's
    // 250 ms grace, so the reader's `time_limit` report may land AFTER the
    // relay wait expires — the assertion below accepts both the reader's
    // cause and the pool's explicit `relay_timeout`, never `None`.
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
        await new Promise((r) => setTimeout(r, 200));
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
    .expect("the request completes within 30s (cold start included)");
    response.expect("request ok")
}

async fn send_head(app: Router, uri: &str) -> axum::http::Response<Body> {
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        app.oneshot(
            Request::builder()
                .method("HEAD")
                .uri(uri)
                .header("authorization", "Bearer test-root")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("the HEAD request completes within 30s (cold start included)");
    response.expect("request ok")
}

/// Assert `body` is the full numbered body: chunk i starts with byte `i`,
/// the rest is 0x78, in exact order.
fn assert_numbered_body(body: &[u8], chunks: usize) {
    assert_eq!(body.len(), chunks * CHUNK_BYTES, "full body length");
    let (numbered_chunks, _rest) = body.as_chunks::<CHUNK_BYTES>();
    for (i, chunk) in numbered_chunks.iter().enumerate() {
        assert_eq!(chunk[0], i as u8, "chunk {i} out of order");
        assert!(
            chunk[1..].iter().all(|&byte| byte == 0x78),
            "chunk {i} payload intact"
        );
    }
}

// The core EDG-9 scenario: the client disconnects MID-production (after the
// first chunk of a ~800 ms response). The reader drains the abandoned
// response in discard mode to the clean end frame (within the limits), the
// socket is restored, and the NEXT request is served by the SAME process
// (x-seq = 2, no cold start). This test is the mutation sentinel: a pool
// that does not wait for the drain (recycles on the drop) or a reader that
// does not drain (poisons the socket) both reset x-seq and fail it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn early_disconnect_drains_and_reuses_the_process() {
    let root = tempfile::tempdir().unwrap();
    // 8 x 16 KiB = 128 KiB, 100 ms between chunks: ~800 ms of production,
    // so a disconnect after the first chunk happens mid-production.
    write_stream_worker(root.path(), "drain-app", 8, 100);
    // Detach cap far above the response (no backpressure); drain limits
    // generous enough that the ~112 KiB remainder drains cleanly.
    let factory = DrainFactory::new(8 * 1024 * 1024, 512 * 1024, 2000);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/drain-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk within 15s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), CHUNK_BYTES);
    assert_eq!(first[0], 0, "chunk 0 first");

    // A disconnects MID-production: the body is dropped while the worker is
    // still producing the remaining ~7 chunks.
    drop(body_a);
    // The reader drains the remainder (~700 ms of production left) plus the
    // pool's completion wait; 2 s is a wide margin on local hardware.
    tokio::time::sleep(Duration::from_millis(2_000)).await;

    // (EDG-9 slice 2) The harness ANSWERED the cancel frame: the response
    // ended with the CANCEL end frame — a clean end, sub-cause
    // `cancelled` (not a plain `drained`).
    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_cancelled_total >= 1,
        "the harness must have answered the cancel, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drained_total, 0,
        "the sub-cause is `cancelled`, not `drained`, stats: {stats:?}"
    );

    // (EDG-9) The lifecycle the bin's operational consumer receives must
    // carry the REUSE reason — not a generic `completed` — with the cancel
    // sub-cause in the detail.
    let drained = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::DrainCompleted,
        2_000,
    )
    .await;
    let drained_ok = drained.as_ref().is_some_and(|event| {
        event.reason == "stream_abandoned_drained" && event.detail == Some("cancelled")
    });
    assert!(
        drained_ok,
        "the lifecycle must show stream_abandoned_drained (reuse) with the cancelled sub-cause, got: {drained:?}"
    );

    // C: the SAME process must serve it (module-scope counter, no reset) —
    // no 4–7 s cold start.
    let c_started = Instant::now();
    let res_c = send(app, "/drain-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "2",
        "x-seq must not reset: the process was drained and reused, not recycled"
    );
    assert!(
        c_started.elapsed() < Duration::from_secs(5),
        "C was served by the warm process, took {:?}",
        c_started.elapsed()
    );
    let body_c = axum::body::to_bytes(res_c.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_numbered_body(&body_c, 8);
}

// A `HEAD` request on a streaming page: the worker streams a full body, the
// client never receives it — the body is discarded before `TAG_END`, the
// same abandon drain applies, and the process is reused for the following
// GET.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn head_request_drains_and_reuses_the_process() {
    let root = tempfile::tempdir().unwrap();
    write_stream_worker(root.path(), "head-app", 8, 100);
    let factory = DrainFactory::new(8 * 1024 * 1024, 512 * 1024, 2000);
    let (state, mut lifecycle_rx) = state_with_lifecycle(root.path().to_path_buf(), factory);
    let app = build_pipeline(state);

    // HEAD: no body is delivered to the client; the worker's streaming body
    // is discarded mid-production.
    let res_head = send_head(app.clone(), "/head-app").await;
    assert_eq!(res_head.status(), StatusCode::OK);
    assert_eq!(res_head.headers().get("x-seq").unwrap(), "1");

    // Give the drain (~800 ms of production) a wide margin before C.
    tokio::time::sleep(Duration::from_millis(2_000)).await;

    // (EDG-9) The REUSE reason must reach the lifecycle (the bin's
    // operational consumer surfaces it on the event).
    let drained = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::DrainCompleted,
        2_000,
    )
    .await;
    let drained_ok = drained
        .as_ref()
        .is_some_and(|event| event.reason == "stream_abandoned_drained");
    assert!(
        drained_ok,
        "the lifecycle must show stream_abandoned_drained (reuse), got: {drained:?}"
    );

    // The SAME process must serve the GET (no recycle, no reset).
    let res_c = send(app, "/head-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "2",
        "x-seq must not reset: the HEAD body was drained, the process reused"
    );
    let body_c = axum::body::to_bytes(res_c.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_numbered_body(&body_c, 8);
}

// An oversized abandoned response: a SINGLE chunk larger than the byte
// limit — the harness writes every frame of that chunk before reading the
// next socket frame (the cancel cannot cut it short). The drain crosses
// the byte limit while discarding it, leaves the socket desynced, and the
// process is RECYCLED (x-seq resets for the next request).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn oversized_abandon_recycles_at_the_byte_limit() {
    let root = tempfile::tempdir().unwrap();
    // 20 × 300 B burst (the deterministic drain entry — see the module
    // docs) plus a single 1 MiB chunk (> the 64 KiB byte limit).
    write_burst_worker(root.path(), "oversized-app", BurstEnding::Oversized);
    // 512 B detach cap (the burst makes the reader park on the reserve);
    // 64 KiB byte limit — crossed by the SINGLE 1 MiB chunk; generous time
    // limit (the drain stops on the bytes, not the time).
    let factory = DrainFactory::new(512, 64 * 1024, 5_000);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/oversized-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), BURST_CHUNK_BYTES);

    // Let the harness flush the burst, the 1 MiB chunk and the natural end
    // frame, THEN disconnect: the drain's cancel arrives stale (the
    // harness's main loop ignores it) and the drain crosses the byte limit
    // on the in flight 1 MiB chunk.
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(body_a);
    // The drain stops within a few ms of the drop (the 1 MiB frame is the
    // last thing to cross the limit); the recycle follows. 2 s is a wide
    // margin.
    tokio::time::sleep(Duration::from_millis(2_000)).await;

    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_drain_bytes_limit_total >= 1,
        "the drain must have stopped at the byte limit, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_cancelled_total, 0,
        "the stale cancel was ignored — no cancel end, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drained_total, 0,
        "an over-limit drain must not count as a clean drain"
    );

    // (EDG-9, amendment 2) The recycle reason AND the sub-cause must reach
    // the lifecycle the bin's operational consumer turns into the
    // operational event. The drain stopped at the byte limit WITHOUT
    // restoring the socket: the termination classifies the socket as not
    // reclaimed — reason `socket_poisoned` — keeping `bytes_limit` as the
    // sub-cause in the detail.
    let terminated = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::Terminated,
        2_000,
    )
    .await;
    let terminated_ok = terminated.as_ref().is_some_and(|event| {
        event.reason == "socket_poisoned" && event.detail == Some("bytes_limit")
    });
    assert!(
        terminated_ok,
        "the recycle must show the socket_poisoned reason with the bytes_limit sub-cause, got: {terminated:?}"
    );

    // C: a FRESH process must serve it (x-seq reset to 1) — burst + the
    // huge chunk.
    let res_c = send(app, "/oversized-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "1",
        "x-seq must reset: the process was recycled at the byte limit"
    );
    let body_c = tokio::time::timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(res_c.into_body(), usize::MAX),
    )
    .await
    .expect("C's full body within 15s")
    .unwrap();
    assert_eq!(
        body_c.len(),
        BURST_CHUNKS * BURST_CHUNK_BYTES + HUGE_CHUNK_BYTES,
        "burst + the huge chunk"
    );
    let huge_start = BURST_CHUNKS * BURST_CHUNK_BYTES;
    assert!(
        body_c[huge_start..huge_start + 4]
            .iter()
            .all(|&b| b == 0x4f),
        "the huge chunk payload intact"
    );
}

// A producer that CANNOT process the cancel before the time limit: after
// flushing the burst it blocks the EVENT LOOP with a synchronous busy-wait
// longer than the drain budget — the harness cannot even READ the cancel
// frame the drain writes. The drain's frame reads stall until the budget is
// exhausted (`drain_time_limit`), the socket is left desynced, and the
// process is RECYCLED.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn slow_abandon_recycles_at_the_time_limit() {
    let root = tempfile::tempdir().unwrap();
    // Burst + a 4 s synchronous busy-wait (>> the 500 ms drain budget AND
    // the 750 ms relay wait): the harness event loop is frozen before the
    // drain writes the cancel.
    write_burst_worker(root.path(), "slow-app", BurstEnding::Freezes);
    // 512 B detach cap (the burst makes the reader park on the reserve);
    // generous byte limit; 500 ms time limit — the frozen harness answers
    // nothing, so the drain runs out the budget.
    let factory = DrainFactory::new(512, 8 * 1024 * 1024, 500);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/slow-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), BURST_CHUNK_BYTES);

    // Wait for the busy-wait to start (it begins 100 ms after the burst and
    // lasts 4 s) and freeze the harness event loop — THEN disconnect: the
    // drain's cancel is written into the kernel buffer but the frozen
    // harness never reads or answers it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(body_a);
    // The drain runs out its 500 ms budget on the stalled reads; the
    // recycle follows. 2 s is a wide margin.
    tokio::time::sleep(Duration::from_millis(2_000)).await;

    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_drain_time_limit_total >= 1,
        "the drain must have stopped at the time limit, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_cancelled_total, 0,
        "the frozen harness never processed the cancel, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drained_total, 0,
        "an over-limit drain must not count as a clean drain"
    );

    // (EDG-9, amendment 2) The recycle reason AND a REAL sub-cause must
    // reach the lifecycle: the reader's `time_limit` cause when the relay
    // resolves in time, or the pool's explicit `relay_timeout` when the
    // wait (500 ms budget + 250 ms grace) expired first — never `None`.
    // The STATS counter above is the strict sub-cause evidence.
    let terminated = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::Terminated,
        2_000,
    )
    .await;
    let terminated_ok = terminated.as_ref().is_some_and(|event| {
        event.reason == "socket_poisoned"
            && matches!(event.detail, Some("time_limit") | Some("relay_timeout"))
    });
    assert!(
        terminated_ok,
        "the recycle must show the socket_poisoned reason with the time_limit or relay_timeout sub-cause, got: {terminated:?}"
    );

    // C: a FRESH process must serve it (x-seq reset to 1). The fresh
    // process runs the same fixture: the burst is instant, then its 4 s
    // event-loop freeze delays the end frame — bound the read.
    let res_c = send(app, "/slow-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "1",
        "x-seq must reset: the process was recycled at the time limit"
    );
    let body_c = tokio::time::timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(res_c.into_body(), usize::MAX),
    )
    .await
    .expect("C's full body within 15s (the 4 s event-loop freeze included)")
    .unwrap();
    assert_eq!(
        body_c.len(),
        BURST_CHUNKS * BURST_CHUNK_BYTES,
        "the full burst on the fresh process"
    );
    assert_eq!(body_c.first(), Some(&0), "chunk 0 first");
}

// An infinite (SSE) stream: with slice 2 the drain writes the CANCEL frame
// and the harness ANSWERS it (amendment 1) — the infinite body is aborted
// and the response ends with the cancel end frame: a CLEAN end. The socket
// is in sync, the process is REUSED. (The time limit is no longer the
// outcome — it stays as a guard for harnesses that cannot answer in time;
// see `slow_abandon_recycles_at_the_time_limit`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn sse_abandon_is_cancelled_and_reuses_the_process() {
    let root = tempfile::tempdir().unwrap();
    write_sse_worker(root.path(), "sse-app");
    // Generous byte limit; generous time limit too: the point is the
    // cancel, not the limit (the relay wait = limit + 250 ms grace must
    // comfortably outlast the cancel round-trip).
    let factory = DrainFactory::new(8 * 1024 * 1024, 8 * 1024 * 1024, 2_000);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/sse-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first SSE tick within 15s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    assert!(String::from_utf8_lossy(&first).contains("tick-0"));

    drop(body_a); // A disconnects; the stream never ends on its own
                  // The drain writes the cancel as the next frames flow; the harness
                  // aborts the body and ends with the cancel end frame. 1.5 s is a wide
                  // margin.
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_cancelled_total >= 1,
        "the harness must have answered the cancel, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drained_total, 0,
        "the sub-cause is `cancelled`, not `drained`, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drain_time_limit_total, 0,
        "the cancel answered before the time limit, stats: {stats:?}"
    );

    // (EDG-9 slice 2) The REUSE reason must reach the lifecycle — with the
    // cancel sub-cause in the detail — NOT a termination.
    let drained = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::DrainCompleted,
        2_000,
    )
    .await;
    let drained_ok = drained.as_ref().is_some_and(|event| {
        event.reason == "stream_abandoned_drained" && event.detail == Some("cancelled")
    });
    assert!(
        drained_ok,
        "the lifecycle must show stream_abandoned_drained (reuse) with the cancelled sub-cause, got: {drained:?}"
    );

    // The SAME process must serve the next request (x-seq continues — no
    // cold start).
    let c_started = Instant::now();
    let res_c = send(app, "/sse-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "2",
        "x-seq must not reset: the SSE stream was cancelled and the process reused"
    );
    assert!(
        c_started.elapsed() < Duration::from_millis(5_000),
        "C was served by the warm process, took {:?}",
        c_started.elapsed()
    );
}

// Limits `0` disable the drain entirely (the pre-EDG-9 behavior): the
// socket is abandoned on the disconnect, the process is RECYCLED
// immediately — no drain is attempted, no clean end is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn disabled_drain_recycles_as_before() {
    let root = tempfile::tempdir().unwrap();
    // The same finite, fast response as the core scenario: with the drain
    // enabled it would be reused — with `0` limits it must be recycled.
    write_stream_worker(root.path(), "legacy-app", 8, 100);
    // max_ms = 0: the drain is disabled (either limit at 0 disables it).
    let factory = DrainFactory::new(8 * 1024 * 1024, 8 * 1024 * 1024, 0);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/legacy-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), CHUNK_BYTES);

    drop(body_a); // A disconnects mid-production
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_socket_poisoned_total >= 1,
        "with the drain disabled the socket is poisoned, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drained_total, 0,
        "a disabled drain must never drain"
    );

    // (EDG-9) The pool knows the sub-cause up front when the policy is
    // disabled: the socket was abandoned and poisoned.
    let terminated = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::Terminated,
        2_000,
    )
    .await;
    let terminated_ok = terminated.as_ref().is_some_and(|event| {
        event.reason == "socket_poisoned" && event.detail == Some("socket_poisoned")
    });
    assert!(
        terminated_ok,
        "the disabled-drain recycle must show the socket_poisoned reason and sub-cause, got: {terminated:?}"
    );

    let res_c = send(app, "/legacy-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "1",
        "x-seq must reset: the drain-less path recycles, as before EDG-9"
    );
    let body_c = axum::body::to_bytes(res_c.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_numbered_body(&body_c, 8);
}

/// (EDG-9 slice 2) The fixture workers for the GUARD scenarios: a burst of
/// `BURST_CHUNKS` × `BURST_CHUNK_BYTES` (300 B) chunks — enough to fill the
/// 16-slot body channel so the forwarder blocks holding detach-cap permits
/// and the READER PARKS ON A RESERVE — followed by one of three endings:
///   * `Freezes`: the event loop is frozen with a SYNCHRONOUS busy-wait
///     longer than the drain's time limit right after the burst — the
///     harness cannot even read the cancel frame the drain writes
///     (`drain_time_limit`);
///   * `Oversized`: a SINGLE chunk larger than the drain byte limit — the
///     harness writes every frame of it before reading the next socket
///     frame (the cancel cannot cut it short; `drain_bytes_limit`);
///   * `Errors`: the body stream errors right after the burst — the
///     `E {error}` end frame is already in flight when the cancel arrives
///     (`stream_error`).
///
/// The burst makes the drain entry DETERMINISTIC: the test drops the
/// response, the forwarder's stuck send fails (the discard flag is set) and
/// the parked reader wakes and drains instead of racing the flag.
const BURST_CHUNKS: usize = 20;
const BURST_CHUNK_BYTES: usize = 300;
const HUGE_CHUNK_BYTES: usize = 1024 * 1024;

enum BurstEnding {
    /// Freeze the event loop (busy-wait) after the burst.
    Freezes,
    /// Enqueue a single 1 MiB chunk after the burst, then close.
    Oversized,
    /// Fail the PULL right after the burst (an in-stream body error).
    Errors,
}

fn write_burst_worker(root: &std::path::Path, name: &str, ending: BurstEnding) {
    // The burst loop shared by all endings.
    let burst = format!(
        r#"      for (let i = 0; i < {chunks}; i++) {{
        const chunk = new Uint8Array({size}).fill(0x79);
        chunk[0] = i;
        c.enqueue(chunk);
      }}"#,
        chunks = BURST_CHUNKS,
        size = BURST_CHUNK_BYTES,
    );
    let source = match ending {
        // Let the harness flush the burst to the socket, then block the
        // EVENT LOOP synchronously for 4 s — longer than the drain's time
        // budget AND the pool's relay wait: the harness cannot read (let
        // alone answer) the cancel frame the drain writes.
        BurstEnding::Freezes => format!(
            r#"    async start(c) {{
{burst}
      await new Promise((r) => setTimeout(r, 100));
      const t0 = performance.now();
      while (performance.now() - t0 < 4000) {{}}
      c.close();
    }}"#,
            burst = burst
        ),
        // A SINGLE chunk larger than the drain byte limit. The harness
        // writes all its frames before reading the next socket frame.
        BurstEnding::Oversized => format!(
            r#"    start(c) {{
{burst}
      c.enqueue(new Uint8Array({huge}).fill(0x4f));
      c.close();
    }}"#,
            burst = burst,
            huge = HUGE_CHUNK_BYTES
        ),
        // The pull FAILS: the error happens on the pull after the burst is
        // drained, so the `E {error}` end frame is in flight before the
        // drain's cancel arrives. (A `c.error()` inside `start` would
        // reject the ReadableStream construction itself — the harness
        // would see a handler error, not an in-stream body error.)
        BurstEnding::Errors => format!(
            r#"    start(c) {{
{burst}
    }},
    pull(c) {{
      c.error(new Error("in-stream pull failure"));
    }}"#,
            burst = burst
        ),
    };
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
        format!(
            r#"let seq = 0;
Deno.serve(() => {{
  seq += 1;
  const stream = new ReadableStream({{
{source}
  }});
  return new Response(stream, {{
    headers: {{ "content-type": "application/octet-stream", "x-seq": String(seq) }},
  }});
}});
"#,
            source = source,
        ),
    )
    .unwrap();
}

// The stream FAILS before the cancel is read: the burst is flushed and the
// pull errors immediately, so the `E {error}` frame is in flight when the
// drop (and the drain's cancel) arrive. The harness's main loop ignores the
// STALE cancel, the drain reads the error end frame, and the process is
// RECYCLED with the `stream_error` sub-cause.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn error_during_drain_recycles_with_the_stream_error_cause() {
    let root = tempfile::tempdir().unwrap();
    write_burst_worker(root.path(), "error-app", BurstEnding::Errors);
    // 512 B detach cap (the burst makes the reader park on the reserve);
    // generous byte/time limits: the drain must stop on the ERROR end
    // frame, not on a limit.
    let factory = DrainFactory::new(512, 8 * 1024 * 1024, 5_000);
    let (state, mut lifecycle_rx) =
        state_with_lifecycle(root.path().to_path_buf(), factory.clone());
    let app = build_pipeline(state);

    let res_a = send(app.clone(), "/error-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), BURST_CHUNK_BYTES);

    // Let the harness flush the burst AND the error end frame (in flight),
    // THEN disconnect: the drain's cancel is stale (the main loop ignores
    // it) and the drain stops on the in flight `E {error}`.
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(body_a);
    tokio::time::sleep(Duration::from_millis(2_000)).await;

    let stats = factory.budget.stats();
    assert!(
        stats.abandoned_drain_stream_error_total >= 1,
        "the drain must have stopped on the error end frame, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_cancelled_total, 0,
        "the stale cancel was ignored — no cancel end, stats: {stats:?}"
    );
    assert_eq!(
        stats.abandoned_drained_total, 0,
        "an error drain must not count as a clean drain"
    );

    // (EDG-9, amendment 2) The stream_error sub-cause must reach the
    // lifecycle the bin's operational consumer surfaces. The in-band error
    // leaves the socket unrecovered: the termination carries the reason
    // `socket_poisoned`, keeping `stream_error` as the sub-cause in the
    // detail.
    let terminated = lifecycle_event_of(
        &mut lifecycle_rx,
        WorkerLifecycleEventKind::Terminated,
        2_000,
    )
    .await;
    let terminated_ok = terminated.as_ref().is_some_and(|event| {
        event.reason == "socket_poisoned" && event.detail == Some("stream_error")
    });
    assert!(
        terminated_ok,
        "the recycle must show the socket_poisoned reason with the stream_error sub-cause, got: {terminated:?}"
    );

    // C: a FRESH process must serve it (x-seq reset to 1).
    let res_c = send(app, "/error-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "1",
        "x-seq must reset: the process was recycled on the stream error"
    );
}
