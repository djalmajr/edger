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

/// File-level lock for the tests below that mutate workers or run a state
/// export: the mutation plane (`MUTATION_PLANE` in `deploy.rs`, D36) is a
/// process-global static, and these tests run in parallel in one test
/// binary. The slot keys include each test's own temp root, but the export
/// flag is shared by ALL of them: a test holding `exporting` makes every
/// other test's mutation fail with `STATE_EXPORT_IN_PROGRESS`, and an
/// in-flight slot from ANY root blocks `begin_state_export` with
/// `STATE_BUSY`. Serializing the mutating/exporting tests for their whole
/// body removes the timing flake (PR #98 CI). Read-only tests (index
/// loads, marker file writes into their own temp dirs) never claim the
/// plane and stay unlocked.
static MUTATION_PLANE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// A zip package with root-level entries (the install / files-upload shape),
/// keeping the temp dir alive for the caller.
fn zip_bytes(entries: &[(&str, &str)]) -> (Vec<u8>, tempfile::TempDir) {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("upload.zip");
    let file = fs::File::create(&path).unwrap();
    {
        let mut writer = zip::ZipWriter::new(file);
        for (name, contents) in entries {
            writer
                .start_file(name.to_string(), SimpleFileOptions::default())
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
    }
    (fs::read(&path).unwrap(), temp)
}

fn state_for(root: &std::path::Path) -> (OrchestratorState, axum::Router) {
    state_for_roots(&[], None, &[root.to_path_buf()])
}

/// Same as `state_for`, but loads the index from bundled and core-overlay
/// roots (cPanel family) plus the user root, the way boot does.
fn state_for_roots(
    bundled: &[std::path::PathBuf],
    overlay: Option<&std::path::PathBuf>,
    user: &[std::path::PathBuf],
) -> (OrchestratorState, axum::Router) {
    let index = load_manifests_from_roots(bundled, overlay, user).unwrap();
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

async fn send_bytes(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Vec<u8>,
) -> (StatusCode, Value, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {ROOT_KEY}"))
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, text)
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
    let _guard = MUTATION_PLANE_LOCK.lock().await;
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
// over `manifest.yaml`'s `enabled` at load, in BOTH directions through the
// real scan (boot reload): the marker `enabled=false` keeps an
// `enabled: true` manifest version disabled, and the marker `enabled=true`
// keeps a version whose manifest later says `enabled: false` enabled — the
// scan must not drop the dir before `insert_with_origin` can read the
// marker.
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
        "manifest-off@1.0.0",
        "manifest-off",
        "name: manifest-off\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nenabled: false\n",
    );
    write_worker(
        root.path(),
        "no-toggle@1.0.0",
        "no-toggle",
        "name: no-toggle\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
    );
    // the operator explicitly disabled the manifest-on version and enabled
    // the manifest-off one.
    fs::write(
        root.path().join("manifest-on@1.0.0").join(".edger-enabled"),
        "enabled=false\n",
    )
    .unwrap();
    fs::write(
        root.path()
            .join("manifest-off@1.0.0")
            .join(".edger-enabled"),
        "enabled=true\n",
    )
    .unwrap();

    let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
    assert_eq!(
        index.resolve_worker("manifest-on", None).unwrap_err().code,
        "NOT_FOUND",
        "the marker must beat the manifest enabled: true"
    );
    assert_eq!(
        index.resolve_worker("manifest-off", None).unwrap().version,
        "1.0.0",
        "the marker must beat the manifest enabled: false through the scan"
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

    let _guard = MUTATION_PLANE_LOCK.lock().await;
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
    let _guard = MUTATION_PLANE_LOCK.lock().await;
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
            let name = file.name().unwrap().into_owned();
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
            let name = file.name().unwrap().into_owned();
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

// The `.edger-enabled` marker is operator state, not package content: a
// package that ships one (e.g. a state export imported back as a deploy)
// has it discarded on the cPanel replace branch — the version still gets the
// cPanel mutual disable at load and the marker must not remain on disk.
#[tokio::test]
async fn install_drops_a_shipped_enabled_marker_on_the_older_cpanel_branch() {
    let _guard = MUTATION_PLANE_LOCK.lock().await;
    let bundled = tempfile::tempdir().unwrap();
    let overlay = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    // the active cPanel version, from the core overlay (never core-bundled,
    // so the install takes the cPanel family branch).
    let overlay_root = overlay.path();
    let active = overlay_root.join("cpanel@2.0.0");
    fs::create_dir_all(&active).unwrap();
    fs::write(
        active.join("manifest.yaml"),
        "name: cpanel\nversion: \"2.0.0\"\nentrypoint: index.html\nkind: static\n",
    )
    .unwrap();
    fs::write(active.join("index.html"), "<html>2.0.0</html>").unwrap();

    // the package ships an enabled toggle for the older version.
    let (zip_bytes, _temp) = zip_bytes(&[
        (
            "manifest.yaml",
            "name: cpanel\nversion: \"1.0.0\"\nentrypoint: index.html\nkind: static\nenabled: true\n",
        ),
        ("index.html", "<html>1.0.0</html>"),
        (".edger-enabled", "enabled=true\n"),
    ]);
    let (state, app) = state_for_roots(
        &[bundled.path().to_path_buf()],
        Some(&overlay.path().to_path_buf()),
        &[user.path().to_path_buf()],
    );
    let (status, json, text) =
        send_bytes(&app, "POST", "/api/admin/workers/install", zip_bytes).await;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    assert_eq!(
        json.get("activation"),
        Some(&Value::String("inactive".into())),
        "the older cPanel version must stay disabled: {json}"
    );
    // the install lands in the core-overlay root; the shipped marker must
    // not survive there while the manifest itself is untouched by the purge.
    let installed_dir = std::path::PathBuf::from(json.get("source").unwrap().as_str().unwrap());
    let canonical_overlay = std::fs::canonicalize(overlay_root).unwrap();
    assert!(
        installed_dir
            .parent()
            .map(|parent| parent.starts_with(&canonical_overlay))
            .unwrap_or(false),
        "the core family installs into the core overlay root: {}",
        installed_dir.display()
    );
    assert!(
        !installed_dir.join(".edger-enabled").exists(),
        "the shipped marker must be discarded, not persisted"
    );
    assert!(
        installed_dir.join("manifest.yaml").exists(),
        "the manifest itself is untouched by the marker purge"
    );

    // boot reload: the mutual disable still holds for the shipped manifest's
    // enabled: true — no marker on disk, the version stays inactive.
    let reloaded = load_manifests_from_roots(
        &[bundled.path().to_path_buf()],
        Some(&overlay.path().to_path_buf()),
        &[user.path().to_path_buf()],
    )
    .unwrap();
    assert_eq!(
        reloaded.resolve_worker("cpanel", None).unwrap().version,
        "2.0.0"
    );
    assert_eq!(
        reloaded
            .admin_workers()
            .into_iter()
            .find(|worker| worker.version == "1.0.0")
            .unwrap()
            .status,
        "disabled"
    );
    drop(state);
}

// The files delete API refuses to delete the reserved marker (it is managed
// by the enable/disable endpoints), answering the per-path FORBIDDEN error
// without touching the file.
#[tokio::test]
async fn files_delete_rejects_the_reserved_marker() {
    let _guard = MUTATION_PLANE_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    write_worker(
        root.path(),
        "hello@1.0.0",
        "hello",
        "name: hello\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
    );
    let dir = root.path().join("hello@1.0.0");
    let marker = dir.join(".edger-enabled");
    fs::write(&marker, "enabled=false\n").unwrap();
    let (state, app) = state_for(root.path());

    let (status, json, text) = send_bytes(
        &app,
        "POST",
        "/api/admin/workers/hello/files/delete?version=1.0.0",
        r#"{"paths":[".edger-enabled"]}"#.as_bytes().to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(json.get("deleted"), Some(&Value::Array(vec![])), "{json}");
    let errors = json
        .get("errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(errors.len(), 1, "{json}");
    assert_eq!(
        errors[0].get("path"),
        Some(&Value::String(".edger-enabled".into()))
    );
    assert_eq!(
        errors[0].get("code"),
        Some(&Value::String("FORBIDDEN".into())),
        "{json}"
    );
    assert!(
        marker.is_file(),
        "the marker must not be deleted by the files API"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "enabled=false\n");
    drop(state);
}

// The files upload API skips a zip-root entry named like the marker (and its
// temp files): the upload succeeds, the regular file lands, and the marker
// keeps its operator-set value.
#[tokio::test]
async fn files_upload_cannot_replace_the_marker() {
    let _guard = MUTATION_PLANE_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    write_worker(
        root.path(),
        "hello@1.0.0",
        "hello",
        "name: hello\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
    );
    let dir = root.path().join("hello@1.0.0");
    let marker = dir.join(".edger-enabled");
    fs::write(&marker, "enabled=true\n").unwrap();
    let (state, app) = state_for(root.path());

    let (zip_bytes, _temp) = zip_bytes(&[
        ("notes.txt", "hello from the upload"),
        (".edger-enabled", "enabled=false\n"),
        (".edger-enabled-writer.tmp", "enabled=false\n"),
    ]);
    let (status, json, text) = send_bytes(
        &app,
        "POST",
        "/api/admin/workers/hello/files?version=1.0.0",
        zip_bytes,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload body {text}; json {json}");
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "enabled=true\n",
        "the marker must keep its operator-set value"
    );
    assert_eq!(
        fs::read_to_string(dir.join("notes.txt")).unwrap(),
        "hello from the upload",
        "regular files must still land"
    );
    assert!(!dir.join(".edger-enabled-writer.tmp").exists());
    // disk and memory agree: the marker still names the manifest value, so
    // the version loads enabled both in memory and after a reload.
    drop(state);
    let reloaded = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
    assert_eq!(
        reloaded.resolve_worker("hello", None).unwrap().version,
        "1.0.0"
    );
}
