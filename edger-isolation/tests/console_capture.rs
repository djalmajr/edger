#![cfg(feature = "multiproc")]

use std::collections::HashMap;
use std::fs;
use std::time::Duration;

use edger_core::SerializedRequest;
use edger_isolation::{ConsoleLogContext, DenoWorkerProcess};

fn request() -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "console-capture".into(),
        base_href: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn console_flood_is_drained_bounded_and_sanitized_without_blocking_worker() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("index.ts"),
        r#"
export default {
  async fetch() {
    console.log("authorization=Bearer secret-value");
    console.log("x".repeat(5000));
    for (let i = 0; i < 300; i++) console.log(`flood-${i}`);
    await new Promise((resolve) => setTimeout(resolve, 1100));
    console.log("after-rate-window");
    return new Response("ok");
  }
}
addEventListener("beforeunload", () => console.error("worker-draining"));
"#,
    )
    .unwrap();

    let (sender, mut receiver) = tokio::sync::mpsc::channel(512);
    let mut process = DenoWorkerProcess::spawn_with_console(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(3),
        &HashMap::new(),
        None,
        sender.clone(),
        ConsoleLogContext {
            namespace: Some("default".into()),
            worker: "console-fixture".into(),
            version: "1.0.0".into(),
        },
    )
    .await
    .unwrap();

    let response = process.request(request()).await.unwrap();
    assert_eq!(response.status, 200);
    process
        .shutdown("test-console-capture", Duration::from_millis(500))
        .await;
    drop(process);

    let mut recycled = DenoWorkerProcess::spawn_with_console(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(3),
        &HashMap::new(),
        None,
        sender,
        ConsoleLogContext {
            namespace: Some("default".into()),
            worker: "console-fixture".into(),
            version: "1.0.0".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(recycled.request(request()).await.unwrap().status, 200);
    recycled
        .shutdown("test-console-capture", Duration::from_millis(500))
        .await;

    let mut records = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(50), receiver.recv()).await
    {
        records.push(record);
    }
    assert!(records.iter().any(|record| record.message == "[redacted]"));
    assert!(records.iter().any(|record| record.truncated));
    assert!(records.iter().any(|record| record.dropped_before > 0));
    assert!(records
        .iter()
        .all(|record| !record.message.contains("secret-value")));
    assert!(records.iter().all(|record| {
        record.context.worker == "console-fixture" && record.context.version == "1.0.0"
    }));
    assert!(records.iter().all(|record| !record.process_id.is_empty()));
    assert!(records
        .iter()
        .any(|record| record.message == "worker-draining"));
    let process_ids = records
        .iter()
        .map(|record| record.process_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        process_ids.len(),
        2,
        "recycle must allocate a new process ID"
    );
}
