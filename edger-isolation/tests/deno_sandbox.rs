//! Deno CLI bridge sandbox: workers must not escape their own directory.

#![cfg(feature = "deno")]

use std::fs;
use std::path::Path;

use edger_core::{parse_worker_config, SerializedRequest, WorkerConfig, WorkerManifest};
use edger_isolation::deno::DenoCliRunner;

fn worker_config(dir: &Path) -> WorkerConfig {
    let manifest = WorkerManifest {
        name: "sandbox".into(),
        entrypoint: Some("index.ts".into()),
        ..WorkerManifest::default()
    };
    let mut config = parse_worker_config(&manifest);
    config.worker_dir = Some(dir.to_path_buf());
    config
}

fn get_request() -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "sandbox-test".into(),
        base_href: None,
    }
}

fn write_worker(dir: &Path, handler_body: &str) {
    fs::write(
        dir.join("index.ts"),
        format!("Deno.serve(() => {{ {handler_body} }});"),
    )
    .expect("write worker entrypoint");
}

// Mutation captured: reverting the bridge to `deno eval` (or dropping the
// `--allow-read=<worker_dir>` restriction) grants full read access, the
// secret leaks into the response, and this test goes red.
#[tokio::test]
async fn worker_cannot_read_outside_worker_dir() {
    let root = tempfile::tempdir().unwrap();
    let secret_path = root.path().join("secret.txt");
    fs::write(&secret_path, "top-secret").unwrap();

    let worker_dir = root.path().join("worker");
    fs::create_dir_all(&worker_dir).unwrap();
    write_worker(
        &worker_dir,
        &format!(
            "return new Response(Deno.readTextFileSync({:?}));",
            secret_path.to_string_lossy()
        ),
    );

    let runner = DenoCliRunner::default();
    let err = runner
        .execute_fetch(get_request(), &worker_config(&worker_dir))
        .expect_err("read outside worker_dir must be denied");
    assert_eq!(err.code, "DENO_EXEC_FAILED");
    assert!(
        err.message.contains("NotCapable") || err.message.contains("PermissionDenied"),
        "expected a permission error, got: {}",
        err.message
    );
}

// Mutation captured: adding `--allow-write` (or reverting to `deno eval`)
// lets the worker mutate its own directory and this test goes red.
#[tokio::test]
async fn worker_cannot_write_even_inside_worker_dir() {
    let worker_dir = tempfile::tempdir().unwrap();
    write_worker(
        worker_dir.path(),
        r#"Deno.writeTextFileSync("./owned.txt", "x"); return new Response("wrote");"#,
    );

    let runner = DenoCliRunner::default();
    let err = runner
        .execute_fetch(get_request(), &worker_config(worker_dir.path()))
        .expect_err("write must be denied");
    assert_eq!(err.code, "DENO_EXEC_FAILED");
    assert!(
        err.message.contains("NotCapable") || err.message.contains("PermissionDenied"),
        "expected a permission error, got: {}",
        err.message
    );
}

// Mutation captured: narrowing `--allow-read` below the worker dir breaks
// legitimate same-dir reads and this test goes red.
#[tokio::test]
async fn worker_reads_own_files() {
    let worker_dir = tempfile::tempdir().unwrap();
    fs::write(worker_dir.path().join("data.txt"), "worker-data").unwrap();
    write_worker(
        worker_dir.path(),
        r#"return new Response(Deno.readTextFileSync("./data.txt"));"#,
    );

    let runner = DenoCliRunner::default();
    let res = runner
        .execute_fetch(get_request(), &worker_config(worker_dir.path()))
        .expect("same-dir read must stay allowed");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.unwrap().as_ref(), b"worker-data");
}
