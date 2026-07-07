//! Story 15.A E2E: persistent Deno worker over UDS, module loaded once.

#![cfg(feature = "multiproc")]

use std::collections::HashMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use edger_core::SerializedRequest;
use edger_isolation::DenoWorkerProcess;

fn write_worker(dir: &Path, body: &str) {
    fs::write(dir.join("index.ts"), body).unwrap();
}

fn request(method: &str, uri: &str, body: Option<&[u8]>) -> SerializedRequest {
    SerializedRequest {
        method: method.into(),
        uri: uri.into(),
        headers: vec![],
        body: body.map(bytes::Bytes::copy_from_slice),
        request_id: "uds-test".into(),
        base_href: None,
    }
}

#[cfg(unix)]
struct EnvVarGuard {
    key: &'static str,
    old: Option<OsString>,
}

#[cfg(unix)]
impl EnvVarGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let old = env::var_os(key);
        env::set_var(key, value);
        Self { key, old }
    }
}

#[cfg(unix)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.old {
            env::set_var(self.key, value);
        } else {
            env::remove_var(self.key);
        }
    }
}

#[cfg(unix)]
fn find_deno_executable() -> Option<PathBuf> {
    if let Ok(executable) = env::var("EDGER_DENO_BIN") {
        if !executable.trim().is_empty() {
            let path = PathBuf::from(executable);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    if let Some(path_var) = env::var_os("PATH") {
        if let Some(path) = env::split_paths(&path_var)
            .map(|dir| dir.join("deno"))
            .find(|path| path.is_file())
        {
            return Some(path);
        }
    }
    env::var("HOME")
        .ok()
        .map(|home| Path::new(&home).join(".deno/bin/deno"))
        .filter(|path| path.is_file())
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn write_no_bundle_deno_wrapper(dir: &Path, real_deno: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let wrapper = dir.join("deno-no-bundle");
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
if [ "$1" = "bundle" ]; then
  echo "unexpected deno bundle invocation" >&2
  exit 97
fi
exec {} "$@"
"#,
            shell_quote(real_deno)
        ),
    )
    .expect("write deno wrapper");
    let mut permissions = fs::metadata(&wrapper)
        .expect("wrapper metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).expect("make wrapper executable");
    wrapper
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
        None,
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

// Mutation captured: restoring unconditional `deno bundle` in spawn would hit
// the wrapper's blocked bundle command and the worker would fail before ready.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn single_file_worker_uses_direct_entrypoint_when_bundler_command_is_blocked() {
    let Some(real_deno) = find_deno_executable() else {
        eprintln!("skipping single_file_worker_uses_direct_entrypoint_when_bundler_command_is_blocked: deno executable not found");
        return;
    };
    let wrapper_dir = tempfile::tempdir().unwrap();
    let wrapper = write_no_bundle_deno_wrapper(wrapper_dir.path(), &real_deno);
    let _env_guard = EnvVarGuard::set("EDGER_DENO_BIN", wrapper.as_os_str());

    let dir = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"Deno.serve(() => new Response("fast path"));"#,
    );

    let mut worker = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
        None,
    )
    .await
    .expect("single-file worker should spawn without deno bundle");

    let res = worker
        .request(request("GET", "/", None))
        .await
        .expect("single-file request");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.unwrap().as_ref(), b"fast path");
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
        None,
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
        None,
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

// Story 20.11: a graceful shutdown fires the worker's `beforeunload` handler and
// drains its `EdgeRuntime.waitUntil()` promises within the grace budget, acking
// the drained count. Without the control frame + harness dispatch the process
// would just be killed and no cleanup would run.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_shutdown_dispatches_beforeunload_and_drains_wait_until() {
    if find_deno_executable().is_none() {
        eprintln!("skipping graceful_shutdown_dispatches_beforeunload: deno not found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"addEventListener("beforeunload", () => {
  EdgeRuntime.waitUntil(new Promise((resolve) => setTimeout(resolve, 50)));
});
Deno.serve(() => new Response("ok"));
"#,
    );

    let mut worker = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
        None,
    )
    .await
    .expect("worker spawns and becomes ready");

    let res = worker
        .request(request("GET", "/", None))
        .await
        .expect("request");
    assert_eq!(res.status, 200);

    // Graceful shutdown, 2s budget: the worker registered exactly one waitUntil.
    let drained = worker.shutdown("terminate", Duration::from_secs(2)).await;
    assert_eq!(
        drained,
        Some(1),
        "beforeunload fired and one waitUntil promise was drained"
    );
}

// Deno KV: --unstable-kv is enabled, so Deno.openKv() works. An in-memory KV
// opened once at module scope keeps a counter across requests, proving the API is
// available. The backend (a path the app manages, or remote) is the app's choice.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deno_kv_persists_across_requests() {
    if find_deno_executable().is_none() {
        eprintln!("skipping deno_kv_persists_across_requests: deno not found");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write_worker(
        dir.path(),
        r#"const kv = await Deno.openKv(":memory:");
Deno.serve(async () => {
  const cur = (await kv.get(["counter"])).value ?? 0;
  const next = (cur as number) + 1;
  await kv.set(["counter"], next);
  return Response.json({ count: next });
});
"#,
    );

    let mut worker = DenoWorkerProcess::spawn(
        dir.path(),
        Some("index.ts"),
        Duration::from_secs(20),
        &HashMap::new(),
        None,
    )
    .await
    .expect("worker spawns");

    let r1 = worker
        .request(request("GET", "/", None))
        .await
        .expect("req1");
    let b1: serde_json::Value = serde_json::from_slice(r1.body.as_deref().unwrap()).unwrap();
    assert_eq!(b1["count"], 1);

    let r2 = worker
        .request(request("GET", "/", None))
        .await
        .expect("req2");
    let b2: serde_json::Value = serde_json::from_slice(r2.body.as_deref().unwrap()).unwrap();
    assert_eq!(b2["count"], 2, "KV counter did not persist across requests");
}
