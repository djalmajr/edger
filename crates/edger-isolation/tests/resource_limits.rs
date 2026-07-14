//! Story 15.D: per-worker memory cap is enforced by the V8 heap limit — a
//! worker that leaks past its cap is killed; the same worker under a larger cap
//! survives. Proves the cap (not the allocation) is what kills, and that a dead
//! worker's process does not take down the host (each worker is its own process).

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
        request_id: "limits-test".into(),
        base_href: None,
    }
}

// A worker that fills the V8 old space with ~2M small live objects on request.
// (Small objects land in old space, unlike large strings which go to
// large-object space and dodge the cap.) Under a 48 MB heap cap V8 aborts
// (OOM); under 256 MB it fits and responds.
const HOG: &str = r#"Deno.serve(() => {
  const keep = [];
  for (let i = 0; i < 2_000_000; i++) keep.push({ a: i, b: i + 1 });
  return new Response("kept:" + keep.length);
});
"#;

fn write_hog(dir: &std::path::Path) {
    std::fs::write(dir.join("index.ts"), HOG).unwrap();
}

// Mutation captured: dropping the `--max-old-space-size` cap in `spawn` lets the
// 48 MB worker allocate ~120 MB successfully, so the first assertion (it must be
// killed) goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_cap_kills_only_the_worker_that_exceeds_it() {
    // Same worker code, two caps. Under 48 MB it must die; under 256 MB it lives.
    let tight_dir = tempfile::tempdir().unwrap();
    write_hog(tight_dir.path());
    let mut tight = DenoWorkerProcess::spawn(
        tight_dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
        Some(48),
    )
    .await
    .expect("spawn under tight cap");

    let tight_result = tight.request(get()).await;
    assert!(
        tight_result.is_err(),
        "worker exceeding its 48 MB cap must be killed, got {tight_result:?}"
    );

    // A separate worker with a generous cap runs the SAME allocation fine —
    // proving the kill was the cap, and that the tight worker's death did not
    // affect this independent process (host healthy).
    let roomy_dir = tempfile::tempdir().unwrap();
    write_hog(roomy_dir.path());
    let mut roomy = DenoWorkerProcess::spawn(
        roomy_dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
        Some(256),
    )
    .await
    .expect("spawn under roomy cap");

    let res = roomy.request(get()).await.expect("roomy worker responds");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.as_deref().unwrap(), b"kept:2000000");
}
