//! EDG-8: the worker slot is released when the response FINISHES BEING
//! PRODUCED (not when the client finishes downloading it). The detach
//! pipeline is a single FIFO (reader reserves bytes → forwarder delivers in
//! order), so:
//!
//! - a slow client A stops reading after the first chunk; a second client B
//!   must still get a 200 within the (1s) queue timeout because the
//!   production-complete signal released the slot; A then receives its full,
//!   intact body from the buffer;
//! - a NUMBERED body stays exactly in order through pauses and the
//!   per-cap backpressure path (no chunk may bypass the queue);
//! - when the response exceeds the per-response detach cap, the reader
//!   backpressures (holding the slot) and B hits WORKER_QUEUE_TIMEOUT;
//! - a disconnect AFTER production completed must not recycle the process
//!   (proven by a module-scope `x-seq` counter);
//! - a producer-side pause: a burst of >16 numbered chunks followed by a
//!   pause WITHOUT closing — when the client resumes, every burst chunk must
//!   arrive before the producer's next frame (measured, with margin):
//!   the forwarder must drain continuously, not wait for a new frame or
//!   `TAG_END`;
//! - `EDGER_STREAM_DETACH_MAX_BYTES=0` disables the pipeline entirely: the
//!   slot is released only when the body is consumed/dropped (legacy);
//! - SSE stays incremental and gzip stays progressive with detach enabled.
//!
//! Requires `deno` on PATH. Ignored by default; run explicitly.

use std::fs;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_isolation::{DenoProcessIsolate, StreamDetachBudget, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, ControlAuth, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use tower::ServiceExt;

/// 32 chunks x 16 KiB = 512 KiB total. The body channel holds 16 chunks, so
/// a client that stops reading after one chunk forces the rest into the
/// detach pipeline (or backpressures the reader at the per-response cap).
const CHUNK_BYTES: usize = 16 * 1024;
const CHUNKS: usize = 32;
const TOTAL_BYTES: usize = CHUNK_BYTES * CHUNKS;

/// Factory whose persistent-process isolates use a detach pipeline of the
/// given sizes (small values keep the e2e deterministic and fast).
#[derive(Clone)]
struct DetachFactory {
    max_bytes: u64,
    budget: Arc<StreamDetachBudget>,
}

impl DetachFactory {
    fn new(max_bytes: u64, budget_bytes: u64) -> Self {
        Self {
            max_bytes,
            budget: Arc::new(StreamDetachBudget::new(budget_bytes)),
        }
    }
}

impl IsolateFactory for DetachFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(
                DenoProcessIsolate::new()
                    .with_stream_detach(self.max_bytes, Arc::clone(&self.budget)),
            ),
        }
    }
}

fn state(root: std::path::PathBuf, factory: DetachFactory) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(factory));
    server.mark_ready(pool.clone());
    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        auth: ControlAuth::with_static_key("test-root"),
    }
}

/// Worker streaming `chunks` x `chunk_bytes`; every chunk carries its own
/// index in the first byte (the rest is 0x78), so ANY permutation of the body
/// is detectable. `pause_ms` > 0 inserts a pause between chunks (the stream
/// does NOT close in the meantime). `x-seq` is a module-scope counter: it
/// only resets when the Deno process is respawned, so it proves whether the
/// SAME persistent process served a request. One process only, short queue
/// timeout: B can only be served in time if A's slot is released at
/// production end (detach), not at drain end.
fn write_stream_worker(
    root: &std::path::Path,
    name: &str,
    chunks: usize,
    chunk_bytes: usize,
    pause_ms: u64,
) {
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
            chunk_bytes = chunk_bytes,
            pause_ms = pause_ms,
        ),
    )
    .unwrap();
}

/// Assert `body` is the full numbered body: chunk i starts with byte `i`,
/// the rest is 0x78, in exact order.
fn assert_numbered_body(body: &[u8], chunks: usize, chunk_bytes: usize) {
    assert_eq!(body.len(), chunks * chunk_bytes, "full body length");
    for (i, chunk) in body.chunks_exact(chunk_bytes).enumerate() {
        assert_eq!(chunk[0], i as u8, "chunk {i} out of order");
        assert!(
            chunk[1..].iter().all(|&byte| byte == 0x78),
            "chunk {i} payload intact"
        );
    }
}

/// Worker that produces a BURST of `burst` numbered chunks (first byte =
/// index) back to back, then pauses `pause_ms` WITHOUT closing, and only
/// then sends one more numbered chunk and closes. The pause is the
/// discriminator window: any chunk still stuck after the 16-slot channel
/// can only arrive early if the forwarder drains continuously.
fn write_burst_worker(root: &std::path::Path, name: &str, burst: usize, pause_ms: u64) {
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
const BURST = {burst};
const CHUNK_BYTES = {chunk_bytes};
const PAUSE_MS = {pause_ms};
Deno.serve(() => {{
  seq += 1;
  const stream = new ReadableStream({{
    async start(c) {{
      for (let i = 0; i < BURST; i++) {{
        const chunk = new Uint8Array(CHUNK_BYTES).fill(0x78);
        chunk[0] = i;
        c.enqueue(chunk);
      }}
      // Pause WITHOUT closing: the next frame is sent only after PAUSE_MS.
      await new Promise((r) => setTimeout(r, PAUSE_MS));
      const post = new Uint8Array(CHUNK_BYTES).fill(0x78);
      post[0] = BURST;
      c.enqueue(post);
      c.close();
    }},
  }});
  return new Response(stream, {{
    headers: {{ "content-type": "text/plain", "x-seq": String(seq) }},
  }});
}});
"#,
            burst = burst,
            chunk_bytes = CHUNK_BYTES,
            pause_ms = pause_ms,
        ),
    )
    .unwrap();
}

async fn send(app: Router, uri: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(uri)
            .header("authorization", "Bearer test-root")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

// The core EDG-8 scenario: A reads only the first chunk and stops (slow
// client); B — sent while A still holds its (unconsumed) body — must receive
// a complete 200 within the 1s queue timeout, and A's full body must stay
// identical to the original 512 KiB (numbered order intact).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn slow_client_detaches_the_slot_and_gets_the_full_body() {
    let root = tempfile::tempdir().unwrap();
    write_stream_worker(root.path(), "big-app", CHUNKS, CHUNK_BYTES, 0);
    // 8 MiB cap / 1 MiB budget: far above the 512 KiB response, so every
    // chunk fits the pipeline while A stops reading.
    let factory = DetachFactory::new(8 * 1024 * 1024, 1024 * 1024);
    let app = build_pipeline(state(root.path().to_path_buf(), factory.clone()));

    // A: take the first chunk, then stop reading (do NOT drop the body).
    let res_a = send(app.clone(), "/big-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk within 15s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), CHUNK_BYTES);
    assert_eq!(first[0], 0, "chunk 0 first");

    // B: must NOT queue out — the slot was released when production ended.
    let b_started = Instant::now();
    let res_b = send(app.clone(), "/big-app").await;
    assert_eq!(
        res_b.status(),
        StatusCode::OK,
        "B must get a 200, not WORKER_QUEUE_TIMEOUT"
    );
    let body_b = axum::body::to_bytes(res_b.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_numbered_body(&body_b, CHUNKS, CHUNK_BYTES);
    assert!(
        b_started.elapsed() < Duration::from_secs(5),
        "B was served promptly, took {:?}",
        b_started.elapsed()
    );

    // A: finishes reading; the body is identical to the original 512 KiB,
    // in exact numbered order.
    let mut expected = Vec::with_capacity(TOTAL_BYTES);
    expected.extend_from_slice(&first);
    while let Some(result) = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A keeps getting chunks (15s timeout)")
    {
        expected.extend_from_slice(&result.expect("chunk ok"));
    }
    assert_numbered_body(&expected, CHUNKS, CHUNK_BYTES);
}

// Order proof through pause + per-cap backpressure: the 128 KiB cap (8 of the
// 16 KiB chunks) is SMALLER than the 512 KiB response, so while A is paused
// the reader must WAIT for the per-response semaphore (backpressure,
// fallback counter) — and when A resumes, EVERY chunk arrives in exact
// order: no newer chunk may bypass the pending queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn numbered_body_stays_in_order_through_pause_and_fallback() {
    let root = tempfile::tempdir().unwrap();
    write_stream_worker(root.path(), "big-app", CHUNKS, CHUNK_BYTES, 0);
    // 128 KiB cap = 8 chunks buffered beyond the 16-slot channel (24 total)
    // < 32 chunks: the reader backpressures on the rest while A is paused.
    let factory = DetachFactory::new(128 * 1024, 1024 * 1024);
    let app = build_pipeline(state(root.path().to_path_buf(), factory.clone()));

    let res_a = send(app.clone(), "/big-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first[0], 0);

    // A pauses: the channel (16) + buffer (8) fill up and the reader waits
    // on the per-response semaphore for the remaining chunks.
    tokio::time::sleep(Duration::from_millis(1_000)).await;

    // A resumes: every chunk must arrive, in exact order, from the mix of
    // channel + queue + fresh production.
    let mut body = Vec::with_capacity(TOTAL_BYTES);
    body.extend_from_slice(&first);
    while let Some(result) = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A keeps getting chunks (15s timeout)")
    {
        let chunk = result.expect("chunk ok");
        assert_eq!(chunk.len(), CHUNK_BYTES, "frames stay 16 KiB");
        body.extend_from_slice(&chunk);
    }
    assert_numbered_body(&body, CHUNKS, CHUNK_BYTES);

    // The fallback counter proves the reader actually waited on the cap
    // (backpressure) instead of bypassing it.
    assert!(
        factory.budget.stats().fallback_cap_total >= 1,
        "the reader must have backpressured on the per-response cap, stats: {:?}",
        factory.budget.stats()
    );
    // And nothing leaked: every reservation was returned.
    assert_eq!(factory.budget.reserved_bytes(), 0, "no budget leak");
}

// Documented fallback: when the response exceeds the per-response detach
// cap, the reader backpressures (holding the slot), so B hits the queue
// timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn oversized_response_falls_back_and_second_client_times_out() {
    let root = tempfile::tempdir().unwrap();
    write_stream_worker(root.path(), "big-app", CHUNKS, CHUNK_BYTES, 0);
    // 128 KiB cap = exactly 8 of the 16 KiB chunks: 8 get buffered, the 9th
    // makes the reader wait (holding the slot) and B times out.
    let app = build_pipeline(state(
        root.path().to_path_buf(),
        DetachFactory::new(128 * 1024, 1024 * 1024),
    ));

    let res_a = send(app.clone(), "/big-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), CHUNK_BYTES);

    // B: A still holds the only process (the backpressure keeps the slot) —
    // B must hit the 1s queue timeout.
    let b_started = Instant::now();
    let res_b = send(app.clone(), "/big-app").await;
    assert_eq!(res_b.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body_b = axum::body::to_bytes(res_b.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_b).unwrap();
    assert_eq!(json["code"], "WORKER_QUEUE_TIMEOUT");
    assert!(
        b_started.elapsed() >= Duration::from_millis(900),
        "B waited the queue budget, took {:?}",
        b_started.elapsed()
    );

    // A's reader is blocked on the per-response semaphore; dropping A's body
    // discards the pipeline (poisoned, exactly like any mid-stream
    // disconnect).
    drop(body_a);
}

// A disconnect AFTER production completed must NOT recycle the process: the
// next request is served by the SAME process (module-scope `x-seq` = 2, not 1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn disconnect_after_production_does_not_recycle_the_process() {
    let root = tempfile::tempdir().unwrap();
    write_stream_worker(root.path(), "big-app", CHUNKS, CHUNK_BYTES, 0);
    let app = build_pipeline(state(
        root.path().to_path_buf(),
        DetachFactory::new(8 * 1024 * 1024, 1024 * 1024),
    ));

    // A: read the first chunk, let production finish (512 KiB over a local
    // UDS takes milliseconds), then DROP the body — a client disconnect AFTER
    // the production-complete signal.
    let res_a = send(app.clone(), "/big-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), CHUNK_BYTES);
    tokio::time::sleep(Duration::from_secs(2)).await;
    drop(body_a); // client disconnect after production completed
                  // Give the disconnect path (queue discard hitting a gone consumer)
                  // a beat before the next request.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // C: the SAME process must serve it (module-scope counter, no reset).
    let res_c = send(app, "/big-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "2",
        "x-seq must not reset: the process was not recycled"
    );
    let body_c = axum::body::to_bytes(res_c.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_numbered_body(&body_c, CHUNKS, CHUNK_BYTES);
}

// Signal ready + IMMEDIATE drop (no settling sleep): the production-complete
// flag — not the timing of the signal observer — must protect the process.
// The deterministic interleaving (drop wins the race while the signal is
// still pending) is proven at pool level by
// `drop_after_production_complete_completes_instead_of_recycling`; this is
// the same observable end-to-end (x-seq must not reset).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn signal_ready_immediate_drop_does_not_recycle() {
    let root = tempfile::tempdir().unwrap();
    write_stream_worker(root.path(), "big-app", CHUNKS, CHUNK_BYTES, 0);
    let app = build_pipeline(state(
        root.path().to_path_buf(),
        DetachFactory::new(8 * 1024 * 1024, 1024 * 1024),
    ));

    // A: read the first chunk, wait just for production to finish (512 KiB
    // over a local UDS is milliseconds — 500ms is a wide margin), then DROP
    // the body immediately.
    let res_a = send(app.clone(), "/big-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), CHUNK_BYTES);
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(body_a); // IMMEDIATE drop after production completed

    // C: the SAME process must serve it (module-scope counter, no reset).
    let res_c = send(app, "/big-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(
        res_c.headers().get("x-seq").unwrap(),
        "2",
        "x-seq must not reset: the process was not recycled"
    );
    let body_c = axum::body::to_bytes(res_c.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_numbered_body(&body_c, CHUNKS, CHUNK_BYTES);
}

// `EDGER_STREAM_DETACH_MAX_BYTES=0` disables the pipeline entirely (no queue,
// no semaphores, no signal): even a response that fits the 16-slot channel
// keeps the slot held until the body is consumed — B must hit the queue
// timeout while A is not reading, and the SAME process serves C after A
// finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn zero_detach_holds_the_slot_until_the_body_ends() {
    let root = tempfile::tempdir().unwrap();
    // 4 chunks x 16 KiB = 64 KiB: fits entirely in the 16-slot channel.
    write_stream_worker(root.path(), "small-app", 4, CHUNK_BYTES, 0);
    // max_bytes = 0: the pipeline is normalized to absent (legacy path).
    let app = build_pipeline(state(
        root.path().to_path_buf(),
        DetachFactory::new(0, 1024 * 1024),
    ));

    let res_a = send(app.clone(), "/small-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    assert_eq!(res_a.headers().get("x-seq").unwrap(), "1");
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first.len(), CHUNK_BYTES);
    // A stops reading — the (legacy) slot stays held.

    // B: must hit the 1s queue timeout (no production-complete signal
    // exists in the legacy path).
    let b_started = Instant::now();
    let res_b = send(app.clone(), "/small-app").await;
    assert_eq!(res_b.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body_b = axum::body::to_bytes(res_b.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_b).unwrap();
    assert_eq!(json["code"], "WORKER_QUEUE_TIMEOUT");
    assert!(
        b_started.elapsed() >= Duration::from_millis(900),
        "B waited the queue budget, took {:?}",
        b_started.elapsed()
    );

    // A finishes: the slot is released when the BODY ends (legacy).
    let mut body = Vec::new();
    body.extend_from_slice(&first);
    while let Some(result) = body_a.next().await {
        body.extend_from_slice(&result.expect("chunk ok"));
    }
    assert_numbered_body(&body, 4, CHUNK_BYTES);

    // C: the SAME process serves it (nothing was recycled).
    let res_c = send(app, "/small-app").await;
    assert_eq!(res_c.status(), StatusCode::OK);
    assert_eq!(res_c.headers().get("x-seq").unwrap(), "2");
    let body_c = axum::body::to_bytes(res_c.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_numbered_body(&body_c, 4, CHUNK_BYTES);
}

// Producer-side pause, drain proof (re-review round 2): the worker produces
// a burst of 24 numbered chunks (> 16 channel slots), pauses 2 s WITHOUT
// closing, and only then sends the next chunk and closes. The client reads
// the first chunk, stops until the whole burst has been produced, and
// resumes: EVERY burst chunk must arrive before the producer sends the
// post-pause chunk — measured against the response headers (which anchor
// the burst: the worker enqueues it synchronously before returning the
// response), with margin. If the forwarder depended on a new frame or on
// TAG_END, the burst would stall for the full 2 s pause and fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn burst_drains_before_next_frame_while_producer_paused() {
    const BURST: usize = 24; // > 16 body-channel slots
    const PAUSE_MS: u64 = 2_000;
    let root = tempfile::tempdir().unwrap();
    write_burst_worker(root.path(), "burst-app", BURST, PAUSE_MS);
    // A cap that holds the whole burst (24 x 16 KiB = 384 KiB): no
    // backpressure is involved — the only thing that can deliver chunks
    // 17..23 early is the forwarder's continuous drain.
    let factory = DetachFactory::new(512 * 1024, 1024 * 1024);
    let app = build_pipeline(state(root.path().to_path_buf(), factory.clone()));

    // The worker enqueues the burst synchronously in `start()` (before the
    // Response is returned), so the headers — and `t0` below — anchor the
    // burst's production time. A forwarder that held the queue until
    // TAG_END would deliver the first chunk only at ≈ t0 + PAUSE_MS and
    // fail the deadline on chunk 0.
    let started = Instant::now();
    let res_a = send(app.clone(), "/burst-app").await;
    assert_eq!(res_a.status(), StatusCode::OK);
    let t0 = started.elapsed(); // ≈ burst production time
    let deadline = t0 + Duration::from_millis(PAUSE_MS - 500);
    let mut body_a = res_a.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A's first chunk within 15s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    assert_eq!(first[0], 0, "chunk 0 first");

    // A stops reading: the burst (a few ms over the UDS) is fully produced
    // long before the 1 s sleep ends; the post-pause chunk will be sent
    // only at ≈ t0 + PAUSE_MS.
    tokio::time::sleep(Duration::from_millis(1_000)).await;

    // A resumes: every burst chunk must arrive before the post-pause chunk
    // is sent — with a 500 ms margin.
    let mut body = Vec::with_capacity((BURST + 1) * CHUNK_BYTES);
    body.extend_from_slice(&first);
    let mut last_burst_index = 0usize;
    while let Some(result) = tokio::time::timeout(Duration::from_secs(15), body_a.next())
        .await
        .expect("A keeps getting chunks (15s timeout)")
    {
        let chunk = result.expect("chunk ok");
        let index = chunk[0] as usize;
        if index < BURST {
            let at = started.elapsed();
            assert!(
                at < deadline,
                "burst chunk {index} arrived at {at:?}, after the deadline {deadline:?} (t0 {t0:?}, post-pause chunk at ≈ t0 + {PAUSE_MS} ms): the forwarder must not wait for a new frame or TAG_END"
            );
            last_burst_index = index;
        }
        body.extend_from_slice(&chunk);
    }
    assert_eq!(last_burst_index, BURST - 1, "the whole burst was measured");
    assert_numbered_body(&body, BURST + 1, CHUNK_BYTES);
}

// With the detach pipeline enabled, SSE must stay INCREMENTAL: the second
// tick reaches the client ~200 ms after the first, not after the stream
// ends.
const SSE_WORKER: &str = r#"Deno.serve(() => {
  const enc = new TextEncoder();
  const stream = new ReadableStream({
    async start(c) {
      for (let i = 0; i < 5; i++) {
        c.enqueue(enc.encode(`data: tick-${i}\n\n`));
        await new Promise((r) => setTimeout(r, 200));
      }
      c.close();
    },
  });
  return new Response(stream, {
    headers: { "content-type": "text/event-stream" },
  });
});
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn sse_with_detach_stays_incremental() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("sse-app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: sse-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmax_processes: 1\nqueue_timeout: 1s\n",
    )
    .unwrap();
    fs::write(dir.join("index.ts"), SSE_WORKER).unwrap();
    let app = build_pipeline(state(
        root.path().to_path_buf(),
        DetachFactory::new(8 * 1024 * 1024, 1024 * 1024),
    ));

    let res = send(app, "/sse-app").await;
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body().into_data_stream();
    let started = Instant::now();
    let first = tokio::time::timeout(Duration::from_secs(10), body.next())
        .await
        .expect("first SSE event within 10s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    let first_at = started.elapsed();
    assert!(String::from_utf8_lossy(&first).contains("tick-0"));

    let second = tokio::time::timeout(Duration::from_secs(10), body.next())
        .await
        .expect("second SSE event within 10s")
        .expect("stream open")
        .expect("chunk ok");
    let second_at = started.elapsed();
    assert!(String::from_utf8_lossy(&second).contains("tick-1"));
    assert!(
        second_at >= first_at + Duration::from_millis(100),
        "SSE must stay incremental with the detach pipeline: first {first_at:?}, second {second_at:?}"
    );
}

// With the detach pipeline enabled, compressed delivery must stay
// PROGRESSIVE: the first (partial) gzip chunk reaches the client — and
// decodes to part1's prefix — long before the worker enqueues part2.
const PROGRESSIVE_WORKER: &str = r#"Deno.serve(() => {
  const enc = new TextEncoder();
  const part1 = "PART1:" + "a".repeat(2048);
  const part2 = "PART2:" + "b".repeat(2048);
  const stream = new ReadableStream({
    start(c) {
      c.enqueue(enc.encode(part1));
      setTimeout(() => {
        c.enqueue(enc.encode(part2));
        c.close();
      }, 500);
    },
  });
  return new Response(stream, { headers: { "content-type": "text/html" } });
});
"#;

/// Incrementally decode whatever a (possibly truncated) gzip stream already
/// carries; stops at the first 0/Err, keeping what decoded so far.
fn partial_gzip_prefix(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = GzDecoder::new(std::io::Cursor::new(bytes));
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match decoder.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs deno on PATH; run explicitly"]
async fn gzip_with_detach_stays_progressive() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("progressive-app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: progressive-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nmax_processes: 1\nqueue_timeout: 1s\n",
    )
    .unwrap();
    fs::write(dir.join("index.ts"), PROGRESSIVE_WORKER).unwrap();
    let app = build_pipeline(state(
        root.path().to_path_buf(),
        DetachFactory::new(8 * 1024 * 1024, 1024 * 1024),
    ));

    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/progressive-app")
                .header("authorization", "Bearer test-root")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );

    let mut body = res.into_body().into_data_stream();
    let started = Instant::now();
    let first = tokio::time::timeout(Duration::from_secs(10), body.next())
        .await
        .expect("first compressed chunk within 10s (includes the deno spawn)")
        .expect("stream open")
        .expect("chunk ok");
    let first_at = started.elapsed();
    assert!(!first.is_empty(), "first compressed chunk must carry data");
    // Decode what the first chunk already carries — BEFORE part2 exists.
    let prefix = partial_gzip_prefix(&first);
    assert!(
        prefix.starts_with(b"PART1:"),
        "first chunk must decode to part1's prefix, got {prefix:?}"
    );

    let rest = tokio::time::timeout(Duration::from_secs(10), body.next())
        .await
        .expect("second compressed chunk within 10s")
        .expect("stream open")
        .expect("chunk ok");
    let second_at = started.elapsed();
    assert!(
        second_at >= first_at + Duration::from_millis(300),
        "part2 is enqueued 500 ms after part1: the first chunk must reach the client well before it — first {first_at:?}, second {second_at:?}"
    );

    // The full body decodes to part1 + part2.
    let mut all: Vec<u8> = first.to_vec();
    all.extend_from_slice(&rest);
    let collected = body.collect::<Vec<_>>().await;
    for chunk in collected {
        all.extend_from_slice(&chunk.expect("chunk ok"));
    }
    let mut decoder = GzDecoder::new(std::io::Cursor::new(&all));
    let mut decoded = Vec::new();
    decoder.read_to_end(&mut decoded).unwrap();
    let expected = format!("PART1:{}PART2:{}", "a".repeat(2048), "b".repeat(2048));
    assert_eq!(decoded, expected.as_bytes(), "full gzip body decodes");
}
