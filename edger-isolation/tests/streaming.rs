//! Story 15.E: the persistent worker reads response bodies as a bounded stream.
//! A finite multi-chunk body comes through whole; an infinite stream is bounded
//! by the byte cap and does NOT hang or desync the persistent process — a second
//! request on the same connection still succeeds.

#![cfg(feature = "multiproc")]

use std::collections::HashMap;
use std::time::Duration;

use edger_core::SerializedRequest;
use edger_isolation::DenoWorkerProcess;

fn get() -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "stream-test".into(),
        base_href: None,
    }
}

fn write_worker(dir: &std::path::Path, src: &str) {
    std::fs::write(dir.join("index.ts"), src).unwrap();
}

// Mutation captured: reverting `drainBounded` to `await response.arrayBuffer()`
// makes the infinite-stream case hang forever (the request read times out),
// flipping the second test red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finite_multi_chunk_stream_delivers_whole_body() {
    let dir = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"Deno.serve(() => {
  const enc = new TextEncoder();
  const stream = new ReadableStream({
    start(c) {
      c.enqueue(enc.encode("chunk-a;"));
      c.enqueue(enc.encode("chunk-b;"));
      c.enqueue(enc.encode("chunk-c"));
      c.close();
    },
  });
  return new Response(stream);
});
"#,
    );
    let mut proc = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
        None,
    )
    .await
    .expect("spawn");

    let res = proc.request(get()).await.expect("finite stream responds");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.as_deref().unwrap(), b"chunk-a;chunk-b;chunk-c");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn infinite_stream_is_bounded_and_process_survives() {
    let dir = tempfile::tempdir().unwrap();
    // A stream that never closes; drainBounded must stop at the byte cap.
    write_worker(
        dir.path(),
        r#"Deno.serve(() => {
  const stream = new ReadableStream({
    pull(c) { c.enqueue(new Uint8Array(1024).fill(120)); },
  });
  return new Response(stream);
});
"#,
    );
    let mut env = HashMap::new();
    env.insert("EDGER_STREAM_MAX_BYTES".to_string(), "4096".to_string());

    let mut proc = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &env,
        None,
    )
    .await
    .expect("spawn");

    // First request: infinite stream is bounded near the 4 KiB cap, not hung.
    let first = proc
        .request(get())
        .await
        .expect("infinite stream returns bounded, does not hang");
    assert_eq!(first.status, 200);
    let len = first.body.as_deref().map(<[u8]>::len).unwrap_or(0);
    assert!(
        (4096..=16384).contains(&len),
        "body should be bounded near the cap, got {len} bytes"
    );

    // Second request on the SAME persistent connection: proves the stream did
    // not desync the frame protocol and the process is still healthy.
    let second = proc
        .request(get())
        .await
        .expect("process survives to serve a second request");
    assert_eq!(second.status, 200);
    let len2 = second.body.as_deref().map(<[u8]>::len).unwrap_or(0);
    assert!((4096..=16384).contains(&len2), "second bounded, got {len2}");
}

// Mutation captured: dropping the STREAM_MAX_MS total-time budget in
// `drainBounded` lets this steady SSE-style stream (200 ms gaps, under the idle
// timeout) run until the 8 MiB byte cap (~38 h), so the request read times out
// (20 s) and this test goes red. This is the exact hang the builtin preview
// surfaced on the real `sse` worker (setInterval every 1 s).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steady_sse_stream_is_bounded_by_total_time() {
    let dir = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"Deno.serve(() => {
  const msg = new TextEncoder().encode("data: tick\n\n");
  let id;
  const stream = new ReadableStream({
    start(c) { id = setInterval(() => c.enqueue(msg), 200); },
    cancel() { clearInterval(id); },
  });
  return new Response(stream, { headers: { "content-type": "text/event-stream" } });
});
"#,
    );
    // Idle high so it never fires; total-time low so IT is what bounds the read.
    let mut env = HashMap::new();
    env.insert("EDGER_STREAM_MAX_MS".to_string(), "600".to_string());
    env.insert("EDGER_STREAM_IDLE_MS".to_string(), "5000".to_string());

    let mut proc = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &env,
        None,
    )
    .await
    .expect("spawn");

    let started = std::time::Instant::now();
    let res = proc
        .request(get())
        .await
        .expect("steady SSE stream returns within the total-time budget, no hang");
    let elapsed = started.elapsed();
    assert_eq!(res.status, 200);
    // A few ticks captured, then bounded — not empty, not runaway.
    let len = res.body.as_deref().map(<[u8]>::len).unwrap_or(0);
    assert!((12..=1200).contains(&len), "bounded SSE snapshot, got {len} bytes");
    assert!(
        elapsed < Duration::from_secs(5),
        "must return near the 600ms budget, took {elapsed:?}"
    );

    // Process is not stuck: it serves a second request.
    let second = proc.request(get()).await.expect("process survives");
    assert_eq!(second.status, 200);
}

// Mutation captured: removing the global `unhandledrejection`/`error` handlers
// from the harness lets a background error thrown by user code AFTER a response
// (here a `setTimeout` that throws + a floating rejection) terminate the
// persistent Deno process, so the SECOND request fails with `[UDS_IO] Broken
// pipe`. This is the root cause behind the preview's `/sse` crash — a cancelled
// SSE stream's `setInterval` tick throwing into a closed controller is one such
// background error. Deterministic via setTimeout so it does not hinge on a
// stream-cancel timing race.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_error_after_response_keeps_process_alive() {
    let dir = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"Deno.serve(() => {
  setTimeout(() => { throw new Error("background boom"); }, 100);
  Promise.reject(new Error("floating rejection"));
  return new Response("handled");
});
"#,
    );
    let mut proc = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
        None,
    )
    .await
    .expect("spawn");

    let first = proc.request(get()).await.expect("first responds");
    assert_eq!(first.status, 200);
    assert_eq!(first.body.as_deref().unwrap(), b"handled");

    // Let the scheduled background error fire before the next request.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // The process must survive the uncaught background error from user code.
    let second = proc
        .request(get())
        .await
        .expect("process survives a background error from user code");
    assert_eq!(second.status, 200);
    assert_eq!(second.body.as_deref().unwrap(), b"handled");
}
