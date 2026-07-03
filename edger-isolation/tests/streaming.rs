//! Stories 15.E/16.D: response bodies stream from the persistent worker as
//! tagged frames (header/chunk/end). Finite bodies come through whole; infinite
//! streams (SSE) flow chunk-by-chunk INCREMENTALLY via `request_stream`; the
//! harness byte cap cleanly truncates runaway streams for buffered callers; and
//! background errors never kill the process.

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

// Story 16.D — the heart of passthrough streaming. A steady SSE worker (tick
// every 200 ms) must deliver its chunks INCREMENTALLY: the second chunk arrives
// measurably later than the first, while the stream is still open. Mutation
// captured: buffering the body in the harness (H frame only after the stream
// ends) makes the first chunk wait for a stream that never ends — recv times
// out and this goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_stream_delivers_chunks_incrementally() {
    let dir = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"Deno.serve(() => {
  let n = 0;
  let id;
  const stream = new ReadableStream({
    start(c) {
      id = setInterval(() => c.enqueue(new TextEncoder().encode(`data: tick-${n++}

`)), 200);
    },
    cancel() { clearInterval(id); },
  });
  return new Response(stream, { headers: { "content-type": "text/event-stream" } });
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

    let mut streamed = proc
        .request_stream(get())
        .await
        .expect("header resolves before the stream ends");
    assert_eq!(streamed.status, 200);

    let started = std::time::Instant::now();
    let first = tokio::time::timeout(Duration::from_secs(5), streamed.chunks.recv())
        .await
        .expect("first chunk within 5s")
        .expect("stream open")
        .expect("chunk ok");
    let first_at = started.elapsed();
    assert!(
        String::from_utf8_lossy(&first).contains("tick-"),
        "sse payload"
    );

    let second = tokio::time::timeout(Duration::from_secs(5), streamed.chunks.recv())
        .await
        .expect("second chunk within 5s")
        .expect("stream open")
        .expect("chunk ok");
    let second_at = started.elapsed();
    assert!(
        String::from_utf8_lossy(&second).contains("tick-"),
        "sse payload"
    );

    // Incremental: the chunks arrived at distinct times (~200ms apart), NOT
    // together after some collection finished.
    assert!(
        second_at >= first_at + Duration::from_millis(100),
        "chunks must arrive incrementally: first at {first_at:?}, second at {second_at:?}"
    );

    // Dropping mid-stream poisons the process (frames still in flight): the
    // next request must fail FAST so the pool respawns a fresh process.
    drop(streamed);
    let after = proc.request(get()).await;
    assert!(
        after.is_err(),
        "poisoned process must not serve a desynced socket, got {after:?}"
    );
}
