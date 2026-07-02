//! Story 15.A E2E: persistent Deno worker over UDS, module loaded once.

#![cfg(feature = "multiproc")]

use std::collections::HashMap;
use std::time::Duration;

use edger_core::SerializedRequest;
use edger_isolation::DenoWorkerProcess;

fn write_worker(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("index.ts"), body).unwrap();
}

fn request(method: &str, uri: &str, body: Option<&[u8]>) -> SerializedRequest {
    SerializedRequest {
        method: method.into(),
        uri: uri.into(),
        headers: vec![],
        body: body.map(|b| bytes::Bytes::copy_from_slice(b)),
        request_id: "uds-test".into(),
        base_href: None,
    }
}

// Mutation captured: if the harness re-imported the module per request (or the
// process were re-spawned), the module-scope counter below would reset and the
// second response would not report call #2 — this asserts the module is loaded
// ONCE and the process is persistent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_worker_serves_multiple_requests_without_reimport() {
    let dir = tempfile::tempdir().unwrap();
    // Module-scope state proves the module is evaluated once and reused.
    write_worker(
        dir.path(),
        r#"let calls = 0;
Deno.serve(async (req: Request) => {
  calls += 1;
  const body = req.body ? await req.text() : "";
  return Response.json({ calls, method: req.method, echo: body });
});
"#,
    );

    let mut worker = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
    )
    .await
    .expect("worker should spawn and become ready");

    // First request over the persistent UDS connection.
    let res1 = worker
        .request(request("POST", "/", Some(b"hello")))
        .await
        .expect("first request");
    assert_eq!(res1.status, 200);
    let body1: serde_json::Value = serde_json::from_slice(res1.body.as_deref().unwrap()).unwrap();
    assert_eq!(body1["calls"], 1);
    assert_eq!(body1["method"], "POST");
    assert_eq!(body1["echo"], "hello");

    // Second request reuses the SAME process + already-imported module.
    let res2 = worker
        .request(request("GET", "/again", None))
        .await
        .expect("second request");
    let body2: serde_json::Value = serde_json::from_slice(res2.body.as_deref().unwrap()).unwrap();
    assert_eq!(body2["calls"], 2, "module was re-imported (counter reset)");
    assert_eq!(body2["method"], "GET");
}

// Mutation captured: dropping the ready-handshake error propagation would let a
// broken worker look healthy; this asserts a module that throws on load fails
// the spawn with a typed error carrying the cause.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_that_throws_on_load_fails_spawn() {
    let dir = tempfile::tempdir().unwrap();
    write_worker(dir.path(), r#"throw new Error("boom at module load");"#);

    let result = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
    )
    .await;
    let err = match result {
        Ok(_) => panic!("spawn must fail when the module throws on load"),
        Err(err) => err,
    };
    assert_eq!(err.code, "UDS_WORKER_FAILED");
    assert!(
        err.message.contains("boom at module load"),
        "error must carry the cause: {}",
        err.message
    );
}

// Latency probe (ignored): warm persistent worker vs. the v1 per-request cost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "perf probe; run with --ignored --nocapture"]
async fn warm_worker_latency_probe() {
    let dir = tempfile::tempdir().unwrap();
    write_worker(dir.path(), r#"Deno.serve(() => new Response("ok"));"#);
    let mut worker = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
    )
    .await
    .unwrap();

    // warm up
    let _ = worker.request(request("GET", "/", None)).await.unwrap();

    let mut samples = Vec::new();
    for _ in 0..50 {
        let t = std::time::Instant::now();
        let r = worker.request(request("GET", "/", None)).await.unwrap();
        assert_eq!(r.status, 200);
        samples.push(t.elapsed());
    }
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    let avg = samples.iter().sum::<Duration>() / samples.len() as u32;
    println!(
        "UDS_WARM_LATENCY avg_us={} p50_us={} p95_us={} n={}",
        avg.as_micros(),
        p50.as_micros(),
        p95.as_micros(),
        samples.len()
    );
}
