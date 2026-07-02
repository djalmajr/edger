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
