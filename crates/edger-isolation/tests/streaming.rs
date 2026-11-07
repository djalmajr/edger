//! Stories 15.E/16.D: response bodies stream from the persistent worker as
//! tagged frames (header/chunk/end). Finite bodies come through whole; infinite
//! streams (SSE) flow chunk-by-chunk INCREMENTALLY via `request_stream`; the
//! harness byte cap cleanly truncates runaway streams for buffered callers; and
//! background errors never kill the process.

#![cfg(feature = "multiproc")]

use std::collections::HashMap;
use std::time::Duration;

use edger_core::{parse_worker_config, Isolate, SerializedRequest, WorkerManifest};
use edger_isolation::{
    AbandonDrain, DenoProcessIsolate, DenoWorkerProcess, StreamDetach, StreamDetachBudget,
};

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

fn get_path(uri: &str) -> SerializedRequest {
    SerializedRequest {
        uri: uri.into(),
        ..get()
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
    // EDG-32: keep the per-worker Deno cache inside this test's own tempdir.
    let deno_cache = tempfile::tempdir().unwrap();
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
        Some(deno_cache.path()),
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
    // EDG-32: keep the per-worker Deno cache inside this test's own tempdir.
    let deno_cache = tempfile::tempdir().unwrap();
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
        Some(deno_cache.path()),
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
    // EDG-32: keep the per-worker Deno cache inside this test's own tempdir.
    let deno_cache = tempfile::tempdir().unwrap();
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
        Some(deno_cache.path()),
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
    // EDG-32: keep the per-worker Deno cache inside this test's own tempdir.
    let deno_cache = tempfile::tempdir().unwrap();
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
        Some(deno_cache.path()),
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

/// EDG-55: a body cut at `EDGER_STREAM_MAX_BYTES` ends cleanly for the client
/// and keeps the process, but the production state says it was truncated, so a
/// completed-execution caller can refuse it. Covers both reader paths: the
/// detach pipeline (`detach` on) and the legacy pump (`EDGER_STREAM_DETACH_MAX_BYTES=0`).
async fn truncation_is_reported(detach_on: bool) {
    let dir = tempfile::tempdir().unwrap();
    let deno_cache = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"Deno.serve((req) => {
  const path = new URL(req.url).pathname;
  if (path === "/short") return new Response("short");
  if (path === "/exact") {
    // Exactly the cap (4096 bytes), then the end: the harness stops at the
    // cap before it can see the end, so this counts as cut too.
    return new Response(new Uint8Array(4096).fill(120));
  }
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
        Some(deno_cache.path()),
    )
    .await
    .expect("spawn");
    let detach = StreamDetach {
        max_bytes: 1 << 20,
        budget: std::sync::Arc::new(StreamDetachBudget::new(1 << 24)),
        drain: AbandonDrain::default(),
        max_duration: None,
    };
    let policy = detach_on.then_some(&detach);

    // Runs one request to the end of its body; returns the body length and
    // the truncation flag of its production state.
    async fn run(
        proc: &mut DenoWorkerProcess,
        uri: &str,
        policy: Option<&StreamDetach>,
    ) -> (usize, bool) {
        let mut res = proc
            .request_stream_with_detach(get_path(uri), policy)
            .await
            .expect("stream starts");
        let mut length = 0;
        while let Some(chunk) = res.chunks.recv().await {
            length += chunk.expect("a cut body still ends cleanly").len();
        }
        let state = res
            .production_complete
            .expect("the production state carries the truncation flag");
        (length, state.is_truncated())
    }

    // The runaway stream is cut at the cap: clean end for the client, flagged.
    let (length, truncated) = run(&mut proc, "/", policy).await;
    assert!(
        (4096..=16384).contains(&length),
        "cut near the cap: {length}"
    );
    assert!(truncated, "a body cut at the cap must be flagged");
    // The process is reused: a short body (control) is whole and NOT flagged.
    let (length, truncated) = run(&mut proc, "/short", policy).await;
    assert_eq!(length, 5);
    assert!(!truncated, "a whole body under the cap is not truncated");
    // A body of exactly the cap length counts as cut (fail closed).
    let (length, truncated) = run(&mut proc, "/exact", policy).await;
    assert_eq!(length, 4096);
    assert!(truncated, "a body of exactly the cap length counts as cut");
    // And the flag does not stick to the next response.
    let (_, truncated) = run(&mut proc, "/short", policy).await;
    assert!(!truncated);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_cut_at_the_byte_cap_is_flagged_truncated_with_detach() {
    truncation_is_reported(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_cut_at_the_byte_cap_is_flagged_truncated_without_detach() {
    truncation_is_reported(false).await;
}

/// EDG-55: the BUFFERED calls of the Deno isolate (`execute_fetch`, what an
/// ephemeral `ttl: 0` worker runs) carry the host's truncation mark: the
/// isolate reports it through `take_response_truncation` after a body the
/// harness cut at `EDGER_STREAM_MAX_BYTES`, still answers the ordinary call
/// with the truncated body and success, reads it once (it clears), and never
/// derives it from what the guest sends (header or body).
async fn buffered_truncation_mark(detach_on: bool) {
    let dir = tempfile::tempdir().unwrap();
    let deno_cache = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"Deno.serve((req) => {
  const path = new URL(req.url).pathname;
  if (path === "/short") return new Response("short");
  if (path === "/forged") {
    // Looks like every mark a guest could try to forge: never a mark.
    return new Response('{"truncated":true}', {
      headers: { "x-edger-truncated": "true", "x-truncated": "1" },
    });
  }
  if (path === "/exact") return new Response(new Uint8Array(4096).fill(120));
  const stream = new ReadableStream({
    pull(c) { c.enqueue(new Uint8Array(1024).fill(120)); },
  });
  return new Response(stream);
});
"#,
    );
    let mut env = std::collections::HashMap::new();
    env.insert("EDGER_STREAM_MAX_BYTES".to_string(), "4096".to_string());
    let mut config = parse_worker_config(&WorkerManifest {
        name: "buffered-cut".into(),
        entrypoint: Some("index.ts".into()),
        env: Some(env),
        ..WorkerManifest::default()
    });
    config.worker_dir = Some(dir.path().to_path_buf());
    let isolate = DenoProcessIsolate::new().with_per_worker_deno_cache_root(deno_cache.path());
    let mut isolate = if detach_on {
        isolate.with_stream_detach(
            1 << 20,
            std::sync::Arc::new(StreamDetachBudget::new(1 << 24)),
        )
    } else {
        isolate
    };

    // Control: nothing is marked before any call.
    assert!(!isolate.take_response_truncation());

    // The runaway body is cut: success for the ordinary call, with the mark.
    let res = isolate
        .execute_fetch(get_path("/"), &config)
        .await
        .expect("an ordinary buffered call keeps the truncated body");
    assert_eq!(res.status, 200);
    let length = res.body.as_ref().map_or(0, |body| body.len());
    assert!(
        (4096..=16384).contains(&length),
        "cut near the cap: {length}"
    );
    assert!(isolate.take_response_truncation(), "the cut body is marked");
    assert!(
        !isolate.take_response_truncation(),
        "reading clears the mark"
    );

    // Control below the cap: whole body, no mark.
    let res = isolate
        .execute_fetch(get_path("/short"), &config)
        .await
        .unwrap();
    assert_eq!(res.body.unwrap().as_ref(), b"short");
    assert!(!isolate.take_response_truncation());

    // A guest that SAYS it was truncated is not marked.
    let res = isolate
        .execute_fetch(get_path("/forged"), &config)
        .await
        .unwrap();
    assert_eq!(res.body.unwrap().as_ref(), br#"{"truncated":true}"#);
    assert!(
        !isolate.take_response_truncation(),
        "a guest cannot forge the mark"
    );

    // Exactly the cap counts as cut (fails closed, like the streamed path).
    let res = isolate
        .execute_fetch(get_path("/exact"), &config)
        .await
        .unwrap();
    assert_eq!(res.body.unwrap().len(), 4096);
    assert!(isolate.take_response_truncation());

    // The mark is sticky across buffered calls until read (a fullstack
    // adapter may call twice in one dispatch), and execute_routes shares it.
    isolate
        .execute_routes(get_path("/"), &config)
        .await
        .unwrap();
    isolate
        .execute_routes(get_path("/short"), &config)
        .await
        .unwrap();
    assert!(
        isolate.take_response_truncation(),
        "an earlier cut is not erased"
    );
    assert!(!isolate.take_response_truncation());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_buffered_call_cut_at_the_byte_cap_is_marked_by_the_host_with_detach() {
    buffered_truncation_mark(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_buffered_call_cut_at_the_byte_cap_is_marked_by_the_host_without_detach() {
    buffered_truncation_mark(false).await;
}
