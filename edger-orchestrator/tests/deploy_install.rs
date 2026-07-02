//! Epic 14 vertical slice: zip install + worker rescan (stories 14.01/14.02).

use std::fs;
use std::io::Write;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::ExecutionKind;
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_isolation::{DenoFacade, DenoIsolate, WasmIsolate};
use edger_orchestrator::{
    build_pipeline, load_manifests_from_dirs, AuthGate, AuthGateConfig, ExtensionRegistry,
    OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct RuntimeFactory;

impl IsolateFactory for RuntimeFactory {
    fn create_isolate(&self, worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        match worker_ref.kind {
            ExecutionKind::WasmModule { .. } => {
                Box::new(WasmIsolate::from_worker_config(&worker_ref.config))
            }
            _ => Box::new(DenoIsolate::new(DenoFacade::new())),
        }
    }
}

fn state_with_root(root: std::path::PathBuf) -> OrchestratorState {
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(RuntimeFactory));
    server.mark_ready(pool.clone());

    OrchestratorState {
        server,
        pool,
        index: load_manifests_from_dirs(&[root]).unwrap(),
        registry: ExtensionRegistry::new(),
        auth: AuthGate::new(
            AuthGateConfig::default(),
            Arc::new(AuthExtension::new(
                Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
                Some("test-root".into()),
            )),
        ),
    }
}

async fn send(
    app: Router,
    method: &str,
    uri: &str,
    api_key: Option<&str>,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, serde_json::Value, String) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", content_type);
    if let Some(key) = api_key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let res = app
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json, text)
}

fn zip_package(files: &[(&str, &str)]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

fn app_zip(version: &str, body_marker: &str) -> Vec<u8> {
    zip_package(&[
        (
            "manifest.yaml",
            &format!("name: zip-app\nversion: \"{version}\"\nentrypoint: index.ts\nkind: fetch\n"),
        ),
        (
            "index.ts",
            &format!("Deno.serve(() => new Response(\"{body_marker}\"));"),
        ),
    ])
}

// Mutation captured: dropping the `ManifestIndex::insert` call after the
// atomic rename (install writes to disk but never indexes) leaves the GET
// below at 404 and this test goes red.
#[tokio::test]
async fn install_zip_deploys_worker_without_restart() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        app_zip("1.0.0", "deployed-v1"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");
    assert_eq!(json["name"], "zip-app");
    assert_eq!(json["version"], "1.0.0");
    assert_eq!(json["url"], "/zip-app");
    assert_eq!(json["kind"], "FetchHandler");

    let (status, _json, text) = send(
        app,
        "GET",
        "/zip-app",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(text, "deployed-v1");
}

// Mutation captured: extracting entries with `entry.name()` joined to the
// destination (instead of `enclosed_name`) writes `evil.txt` outside the
// staging dir and both assertions below go red.
#[tokio::test]
async fn install_rejects_zip_slip() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    let malicious = zip_package(&[
        ("manifest.yaml", "name: evil-app\nentrypoint: index.ts\n"),
        ("../evil.txt", "escaped"),
    ]);
    let (status, json, _text) = send(
        app,
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        malicious,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "DEPLOY_PATH_DENIED");
    assert!(
        !root.path().parent().unwrap().join("evil.txt").exists(),
        "zip-slip escaped the worker root"
    );
    assert!(
        fs::read_dir(root.path()).unwrap().next().is_none(),
        "failed install must not leave residue in the root"
    );
}

// Mutation captured: skipping `validate_package_manifest` accepts a package
// with no identity/entrypoint and this test goes red.
#[tokio::test]
async fn install_rejects_package_without_manifest() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    let (status, json, _text) = send(
        app,
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        zip_package(&[("notes.txt", "not a worker")]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "DEPLOY_INVALID_PACKAGE");
}

// Mutation captured: removing the `workers:install` permission gate lets the
// viewer key deploy and the 403 assertion goes red.
#[tokio::test]
async fn install_requires_auth_and_permission() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    let (status, _json, _text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/install",
        None,
        "application/zip",
        app_zip("1.0.0", "x"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, created, text) = send(
        app.clone(),
        "POST",
        "/api/admin/keys",
        Some("test-root"),
        "application/json",
        br#"{"name":"viewer","role":"viewer","permissions":["workers:read"],"namespaces":["*"]}"#
            .to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");
    let viewer_key = created["rawKey"].as_str().unwrap().to_string();

    let (status, json, _text) = send(
        app,
        "POST",
        "/api/admin/workers/install",
        Some(&viewer_key),
        "application/zip",
        app_zip("1.0.0", "x"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["code"], "FORBIDDEN");
}

// Mutation captured: dropping the index COLLISION check (or the rollback of
// the target dir on insert failure) makes the second install return 201 and
// this test goes red.
#[tokio::test]
async fn install_duplicate_version_conflicts_and_new_version_coexists() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    let (status, _json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        app_zip("1.0.0", "deployed-v1"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");

    let (status, json, _text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        app_zip("1.0.0", "deployed-v1-again"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        json["code"] == "COLLISION" || json["code"] == "DEPLOY_TARGET_EXISTS",
        "unexpected code: {json}"
    );

    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        app_zip("2.0.0", "deployed-v2"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");
    assert_eq!(json["version"], "2.0.0");

    // latest resolves to the freshly installed v2.
    let (status, _json, text) = send(
        app,
        "GET",
        "/zip-app",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, "deployed-v2");
}

fn write_manual_worker(root: &std::path::Path, name: &str, marker: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!("name: {name}\nversion: \"1.0.0\"\nentrypoint: index.html\n"),
    )
    .unwrap();
    fs::write(
        dir.join("index.html"),
        format!("<!doctype html><html><head></head><body>{marker}</body></html>"),
    )
    .unwrap();
}

// Mutation captured: making rescan apply behave like dry-run (never touching
// the index) keeps the manual worker at 404 after apply and this test goes
// red.
#[tokio::test]
async fn rescan_indexes_manually_copied_worker_and_removes_deleted_one() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    write_manual_worker(root.path(), "manual-app", "manual-online");

    // Dry-run reports the diff without applying it.
    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/rescan",
        Some("test-root"),
        "application/json",
        b"{}".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert_eq!(json["dryRun"], true);
    assert_eq!(json["added"][0], "manual-app@1.0.0");
    let (status, _json, _text) = send(
        app.clone(),
        "GET",
        "/manual-app",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "dry-run must not index");

    // Apply indexes the worker; it serves without restart.
    let (status, json, _text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/rescan",
        Some("test-root"),
        "application/json",
        br#"{"dryRun":false}"#.to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["added"][0], "manual-app@1.0.0");
    let (status, _json, text) = send(
        app.clone(),
        "GET",
        "/manual-app",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unexpected body: {text}");
    assert!(text.contains("manual-online"));

    // Deleting from disk + apply removes it from the index.
    fs::remove_dir_all(root.path().join("manual-app")).unwrap();
    let (status, json, _text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/rescan",
        Some("test-root"),
        "application/json",
        br#"{"dryRun":false}"#.to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["removed"][0], "manual-app@1.0.0");
    let (status, _json, _text) = send(
        app,
        "GET",
        "/manual-app",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// Mutation captured: a dry-run that mutates the index reports an empty diff
// on the second call and the repeated-diff assertion goes red.
#[tokio::test]
async fn rescan_dry_run_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));
    write_manual_worker(root.path(), "idem-app", "idem");

    for _ in 0..2 {
        let (status, json, _text) = send(
            app.clone(),
            "POST",
            "/api/admin/workers/rescan",
            Some("test-root"),
            "application/json",
            b"{}".to_vec(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["added"][0], "idem-app@1.0.0");
        assert_eq!(json["removed"].as_array().unwrap().len(), 0);
    }
}

// Mutation captured: raising the install route's DefaultBodyLimit above
// MAX_BODY_BYTES accepts the oversized package and this test goes red.
#[tokio::test]
async fn install_rejects_body_above_cap() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    let oversized = vec![0u8; edger_orchestrator::MAX_BODY_BYTES + 1];
    let (status, _json, _text) = send(
        app,
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        oversized,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

async fn body_of(app: Router, uri: &str) -> (StatusCode, String) {
    let (status, _json, text) =
        send(app, "GET", uri, Some("test-root"), "text/plain", Vec::new()).await;
    (status, text)
}

// Mutation captured: making `latest` resolution ignore the `enabled` flag (or
// making disable target the wrong version) breaks the rollback: after
// disabling v2, `/zip-app` would still serve v2 and the rollback assertion
// goes red.
#[tokio::test]
async fn deploy_v2_then_rollback_to_v1() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    for (version, marker) in [("1.0.0", "deployed-v1"), ("2.0.0", "deployed-v2")] {
        let (status, _json, text) = send(
            app.clone(),
            "POST",
            "/api/admin/workers/install",
            Some("test-root"),
            "application/zip",
            app_zip(version, marker),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "install {version}: {text}");
    }

    // latest serves the freshly installed v2.
    assert_eq!(
        body_of(app.clone(), "/zip-app").await,
        (StatusCode::OK, "deployed-v2".into())
    );
    // pinned routes serve their exact version.
    assert_eq!(
        body_of(app.clone(), "/zip-app@1.0.0").await,
        (StatusCode::OK, "deployed-v1".into())
    );
    assert_eq!(
        body_of(app.clone(), "/zip-app@2.0.0").await,
        (StatusCode::OK, "deployed-v2".into())
    );

    // Rollback: disable v2 -> latest falls back to v1 without a restart.
    let (status, json, _text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/zip-app/disable?version=2.0.0",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "disabled");
    assert_eq!(
        body_of(app.clone(), "/zip-app").await,
        (StatusCode::OK, "deployed-v1".into())
    );
    // pinned v1 keeps serving throughout the cycle.
    assert_eq!(
        body_of(app.clone(), "/zip-app@1.0.0").await,
        (StatusCode::OK, "deployed-v1".into())
    );

    // Re-enable v2 -> latest returns to v2 (roll-forward).
    let (status, _json, _text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/zip-app/enable?version=2.0.0",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body_of(app.clone(), "/zip-app").await,
        (StatusCode::OK, "deployed-v2".into())
    );

    // Disabling the OLDER version (not latest) must target exactly v1: latest
    // stays v2 and the pinned v1 route stops resolving. Holds only if disable
    // honors the requested version instead of always hitting latest.
    // Mutation captured: making disable ignore `version` and always target
    // latest disables v2 here, so `/zip-app` drops to v1 and this goes red.
    let (status, _json, _text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/zip-app/disable?version=1.0.0",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body_of(app.clone(), "/zip-app").await,
        (StatusCode::OK, "deployed-v2".into())
    );
    assert_eq!(
        body_of(app, "/zip-app@1.0.0").await.0,
        StatusCode::NOT_FOUND
    );
}

// Mutation captured: dropping the per-version status in the admin listing (or
// listing only one version per name) makes this per-version state assertion go
// red.
#[tokio::test]
async fn admin_lists_each_version_with_its_state() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    for version in ["1.0.0", "2.0.0"] {
        send(
            app.clone(),
            "POST",
            "/api/admin/workers/install",
            Some("test-root"),
            "application/zip",
            app_zip(version, version),
        )
        .await;
    }
    send(
        app.clone(),
        "POST",
        "/api/admin/workers/zip-app/disable?version=2.0.0",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;

    let (status, json, _text) = send(
        app,
        "GET",
        "/api/admin/workers",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows: Vec<_> = json["workers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["name"] == "zip-app")
        .map(|w| {
            (
                w["version"].as_str().unwrap(),
                w["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(rows.contains(&("1.0.0", "loaded")), "rows: {rows:?}");
    assert!(rows.contains(&("2.0.0", "disabled")), "rows: {rows:?}");
}

// A public worker whose handler throws on first request — exercises the
// per-worker error log.
fn throwing_public_zip() -> Vec<u8> {
    zip_package(&[
        (
            "manifest.yaml",
            "name: boom-app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nvisibility: public\n",
        ),
        (
            "index.ts",
            "Deno.serve(() => { throw new Error(\"boom on first request\"); });",
        ),
    ])
}

// Mutation captured: dropping `auth_required` from the install response (or
// deriving it independently of visibility) breaks these assertions.
#[tokio::test]
async fn install_response_reports_auth_required_from_visibility() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    // protected worker -> authRequired true
    let (status, json, text) = send(
        app.clone(),
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        app_zip("1.0.0", "x"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "unexpected body: {text}");
    assert_eq!(json["visibility"], "protected");
    assert_eq!(json["authRequired"], true);
    assert_eq!(json["url"], "/zip-app");

    // public worker -> authRequired false
    let (status, json, _text) = send(
        app,
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        throwing_public_zip(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(json["visibility"], "public");
    assert_eq!(json["authRequired"], false);
}

// Mutation captured: not recording worker dispatch errors (or the errors
// endpoint ignoring the per-worker log) leaves `errors` empty and this goes
// red.
#[tokio::test]
async fn worker_first_request_error_is_visible_via_admin_api() {
    let root = tempfile::tempdir().unwrap();
    let app = build_pipeline(state_with_root(root.path().to_path_buf()));

    send(
        app.clone(),
        "POST",
        "/api/admin/workers/install",
        Some("test-root"),
        "application/zip",
        throwing_public_zip(),
    )
    .await;

    // No errors before the first request.
    let (status, json, _text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/boom-app/errors",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["errors"].as_array().unwrap().len(), 0);

    // First request fails (handler throws) -> surfaces a 5xx.
    let (status, _json, _text) = send(
        app.clone(),
        "GET",
        "/boom-app",
        None,
        "text/plain",
        Vec::new(),
    )
    .await;
    assert!(
        status.is_server_error(),
        "expected worker error, got {status}"
    );

    // The failure is now visible per worker, without stdout diving.
    let (status, json, _text) = send(
        app.clone(),
        "GET",
        "/api/admin/workers/boom-app/errors",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let errors = json["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "unexpected: {json}");
    assert_eq!(errors[0]["code"], "WORKER_ERROR");
    assert!(errors[0]["status"].as_u64().unwrap() >= 500);
    assert!(!errors[0]["requestId"].as_str().unwrap().is_empty());
    // Mutation captured: dropping ANSI stripping in `record` leaves terminal
    // escape codes in the stored message and this goes red.
    assert!(
        !errors[0]["message"].as_str().unwrap().contains('\u{1b}'),
        "message must be ANSI-stripped"
    );

    // The Workers listing summary reflects the same failure per worker.
    // Mutation captured: an empty/omitted summary makes the count assertion red.
    let (status, json, _text) = send(
        app,
        "GET",
        "/api/admin/workers/error-summary",
        Some("test-root"),
        "text/plain",
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["summary"]["boom-app"]["count"], 1,
        "unexpected: {json}"
    );
    assert_eq!(
        json["summary"]["boom-app"]["latest"]["code"],
        "WORKER_ERROR"
    );
}
