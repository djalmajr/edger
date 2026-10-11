//! EDG-21: the per-version enable/disable toggle persists inside the version
//! dir (`.edger-enabled`, next to `.edger-revision`) and survives restart,
//! rescan and state export/import. The restore happens while the index is
//! built, before any `minProcesses` prewarm runs, so a persisted-disabled
//! version never spawns a floor process after a restart.

use std::fs;
use std::io::Read;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, load_manifests_from_roots, prewarm_min_process_workers, ControlAuth,
    OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use serde_json::Value;
use tower::ServiceExt;

const ROOT_KEY: &str = "test-root";

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

/// A fetch worker with a `minProcesses` floor, so the prewarm assertions
/// discriminate an enabled from a disabled version.
fn write_worker(root: &std::path::Path, directory: &str, _name: &str, manifest: &str) {
    let worker = root.join(directory);
    fs::create_dir_all(&worker).unwrap();
    fs::write(worker.join("manifest.yaml"), manifest).unwrap();
    fs::write(
        worker.join("index.ts"),
        "Deno.serve(() => new Response('ok'));",
    )
    .unwrap();
}

fn state_for(root: &std::path::Path) -> (OrchestratorState, axum::Router) {
    let index = load_manifests_from_roots(&[], None, &[root.to_path_buf()]).unwrap();
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());
    let state = OrchestratorState {
        server,
        pool,
        index,
        auth: ControlAuth::with_static_key(ROOT_KEY),
    };
    (state.clone(), build_pipeline(state))
}

async fn send(app: &axum::Router, method: &str, uri: &str) -> (StatusCode, Value, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {ROOT_KEY}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
}

// The restore step under test: a disable done through the API is applied
// again when the index is rebuilt from disk the way boot does, the marker is
// what the reload reads, and the boot prewarm skips the persisted-disabled
// version. Re-enabling persists too and reloads enabled.
#[tokio::test]
async fn disabled_version_survives_a_restart_and_reenabling_persists() {
    let root = tempfile::tempdir().unwrap();
    write_worker(
        root.path(),
        "hello@1.0.0",
        "hello",
        "name: hello\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nttl: 30s\nminProcesses: 2\nmaxProcesses: 2\n",
    );
    let marker = root.path().join("hello@1.0.0").join(".edger-enabled");
    let (_state, app) = state_for(root.path());

    let (status, json, text) = send(
        &app,
        "POST",
        "/api/admin/workers/hello/disable?version=1.0.0",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["status"], "disabled");
    assert_eq!(json["prewarm"], "not_configured");
    assert_eq!(fs::read_to_string(&marker).unwrap(), "enabled=false\n");

    // "Restart": rebuild the index from disk exactly as boot does.
    let restarted = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
    let inventory = restarted.admin_workers();
    assert_eq!(inventory[0].status, "disabled");
    assert_eq!(
        restarted.resolve_worker("hello", None).unwrap_err().code,
        "NOT_FOUND"
    );
    // The boot prewarm must not spawn the floor of a persisted-disabled
    // version.
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    let spawned = prewarm_min_process_workers(&restarted, &pool)
        .await
        .unwrap();
    assert_eq!(spawned, 0, "persisted-disabled version must not prewarm");

    // Re-enable and restart again: the toggle survives in the other
    // direction too.
    let (status, json, text) = send(
        &app,
        "POST",
        "/api/admin/workers/hello/enable?version=1.0.0",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["status"], "loaded");
    assert_eq!(json["prewarm"], "scheduled");
    assert_eq!(fs::read_to_string(&marker).unwrap(), "enabled=true\n");

    let restarted = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
    assert_eq!(
        restarted.resolve_worker("hello", None).unwrap().version,
        "1.0.0"
    );
    // Control: the same fresh load without the marker would prewarm the
    // floor, so the 0 above is the marker's effect, not the pool's.
    fs::remove_file(&marker).unwrap();
    let enabled_load = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    let spawned = prewarm_min_process_workers(&enabled_load, &pool)
        .await
        .unwrap();
    assert_eq!(spawned, 2, "the enabled version's floor must prewarm");
}

// A persisted toggle records the operator's last explicit choice and wins
// over `manifest.yaml`'s `enabled` at load. A version that is indexed loads
// with the manifest default (`enabled` absent or `true`); the marker
// `enabled=false` keeps it disabled across a restart. (A manifest with
// `enabled: false` is skipped by the scan entirely — pre-existing behavior —
// so a marker can only override versions that actually load; the reader
// itself applies the marker in both directions, covered by the
// `persisted_enabled_marker_wins_over_the_manifest_at_load` unit test.)
#[test]
fn persisted_toggle_beats_the_manifest_in_both_directions() {
    let root = tempfile::tempdir().unwrap();
    write_worker(
        root.path(),
        "manifest-on@1.0.0",
        "manifest-on",
        "name: manifest-on\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nenabled: true\n",
    );
    write_worker(
        root.path(),
        "no-toggle@1.0.0",
        "no-toggle",
        "name: no-toggle\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
    );
    // the operator explicitly disabled the manifest-on version.
    fs::write(
        root.path().join("manifest-on@1.0.0").join(".edger-enabled"),
        "enabled=false\n",
    )
    .unwrap();

    let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
    assert_eq!(
        index.resolve_worker("manifest-on", None).unwrap_err().code,
        "NOT_FOUND",
        "the marker must beat the manifest enabled: true"
    );
    assert_eq!(
        index.resolve_worker("no-toggle", None).unwrap().version,
        "1.0.0",
        "without a marker the manifest default (true) applies"
    );
}

// A failed persist answers an error, leaves the in-memory flag unchanged and
// writes no marker; the retry once the disk problem is gone succeeds.
#[tokio::test]
#[cfg(unix)]
async fn failed_persist_leaves_memory_unchanged() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    write_worker(
        root.path(),
        "hello@1.0.0",
        "hello",
        "name: hello\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nttl: 30s\n",
    );
    let dir = root.path().join("hello@1.0.0");
    let marker = dir.join(".edger-enabled");
    let (state, app) = state_for(root.path());

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    // A real non-writable check (root would still be able to write).
    let probe = dir.join(".write-probe");
    if fs::write(&probe, b"x").is_ok() {
        let _ = fs::remove_file(&probe);
        eprintln!(
            "skipping failed_persist_leaves_memory_unchanged: running with write-through permissions (root)"
        );
        return;
    }
    let (status, json, text) = send(
        &app,
        "POST",
        "/api/admin/workers/hello/disable?version=1.0.0",
    )
    .await;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{text}");
    assert_eq!(json["code"], "DEPLOY_IO", "{text}");
    // memory: the version is still enabled; disk: no marker was left behind.
    assert_eq!(
        state.index.resolve_worker("hello", None).unwrap().version,
        "1.0.0"
    );
    assert!(!marker.exists());

    // the failure was the permission, not the code: the retry succeeds and
    // the marker lands.
    let (status, json, text) = send(
        &app,
        "POST",
        "/api/admin/workers/hello/disable?version=1.0.0",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json["status"], "disabled");
    assert_eq!(fs::read_to_string(&marker).unwrap(), "enabled=false\n");
}

// The marker lives inside the version dir, so the state export carries it;
// re-importing the export into a fresh root loads the version with the
// toggle applied.
#[tokio::test]
async fn state_export_carries_the_persisted_toggle_and_import_applies_it() {
    let root = tempfile::tempdir().unwrap();
    write_worker(
        root.path(),
        "hello@1.0.0",
        "hello",
        "name: hello\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nttl: 30s\nminProcesses: 1\nmaxProcesses: 1\n",
    );
    let (_state, app) = state_for(root.path());

    let (status, _json, text) = send(
        &app,
        "POST",
        "/api/admin/workers/hello/disable?version=1.0.0",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");

    let (status, _json, text) = send(&app, "GET", "/api/admin/state/export").await;
    assert_eq!(status, StatusCode::OK, "{text}");

    // The export streams a zip body; read the raw bytes to inspect entries.
    let request = Request::builder()
        .uri("/api/admin/state/export")
        .header("authorization", format!("Bearer {ROOT_KEY}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let mut marker_entry = None;
    let mut names: Vec<String> = Vec::new();
    {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(body.clone())).unwrap();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let name = file.name().to_string();
            if file.is_dir() {
                continue;
            }
            if name == "user-roots/0/hello@1.0.0/.edger-enabled" {
                let mut contents = String::new();
                file.read_to_string(&mut contents).unwrap();
                marker_entry = Some(contents);
            }
            names.push(name);
        }
    }
    assert!(
        names
            .iter()
            .any(|name| name == "user-roots/0/hello@1.0.0/manifest.yaml"),
        "the version dir must be in the export: {names:?}"
    );
    assert_eq!(
        marker_entry.as_deref(),
        Some("enabled=false\n"),
        "the persisted toggle must travel with the version"
    );

    // Import: unpack the user root of the export into a fresh root and load
    // it the way boot does — the toggle is applied and the floor is not
    // prewarmed.
    let fresh = tempfile::tempdir().unwrap();
    {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(body.clone())).unwrap();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let name = file.name().to_string();
            if file.is_dir() {
                continue;
            }
            let Some(rel) = name.strip_prefix("user-roots/0/") else {
                continue;
            };
            let dest = fresh.path().join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let mut contents = Vec::new();
            file.read_to_end(&mut contents).unwrap();
            fs::write(&dest, contents).unwrap();
        }
    }
    let imported = load_manifests_from_roots(&[], None, &[fresh.path().to_path_buf()]).unwrap();
    assert_eq!(
        imported.resolve_worker("hello", None).unwrap_err().code,
        "NOT_FOUND",
        "the imported version must load disabled"
    );
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    let spawned = prewarm_min_process_workers(&imported, &pool).await.unwrap();
    assert_eq!(spawned, 0, "the imported disabled version must not prewarm");
}

// A version with no persisted toggle keeps today's behavior: the manifest
// value (default true) applies at load, and nothing is written when there is
// no change to persist.
#[test]
fn no_toggle_keeps_the_manifest_behavior() {
    let root = tempfile::tempdir().unwrap();
    write_worker(
        root.path(),
        "hello@1.0.0",
        "hello",
        "name: hello\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
    );
    let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
    assert_eq!(
        index.resolve_worker("hello", None).unwrap().version,
        "1.0.0"
    );
    assert!(
        !root
            .path()
            .join("hello@1.0.0")
            .join(".edger-enabled")
            .exists(),
        "boot must not invent a toggle"
    );
}
