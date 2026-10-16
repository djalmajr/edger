//! (EDG-34, round 4, F1) `ManifestIndex::resolve_default_version` returns
//! the SAME version as `resolve_worker(name, None).version` in every case
//! the rule has today — promoted pointer, no pointer, staged, disabled and
//! absent worker — while cloning only the version string, never the
//! `WorkerRef`. The pointer cases go through the operator path (the
//! `POST /api/admin/workers/{name}/promote` route), the rest use the index
//! directly.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerManifest,
};
use edger_orchestrator::{
    build_pipeline, ControlAuth, ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

const ROOT_KEY: &str = "test-root";

struct OkIsolate;

#[async_trait]
impl Isolate for OkIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 204,
            headers: vec![],
            body: None,
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 204,
            headers: vec![],
            body: None,
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

struct OkFactory;

impl IsolateFactory for OkFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        Box::new(OkIsolate)
    }
}

/// Create the version dir (production layout `<root>/<name>/<version>`)
/// and insert the manifest into the index; `staged` writes the staged
/// revision marker.
fn insert_version(
    index: &mut ManifestIndex,
    root: &Path,
    name: &str,
    version: &str,
    enabled: bool,
    staged: bool,
) {
    let dir = root.join(name).join(version);
    std::fs::create_dir_all(&dir).unwrap();
    if staged {
        std::fs::write(dir.join(".edger-revision"), "rev-1\nstaged=true\n").unwrap();
    }
    index
        .insert(
            dir,
            WorkerManifest {
                name: name.into(),
                version: Some(version.into()),
                entrypoint: Some("index.ts".into()),
                enabled: Some(enabled),
                ..Default::default()
            },
        )
        .unwrap_or_else(|err| panic!("insert {name}@{version}: {err}"));
}

/// A pipeline with the static root key (the operator path for promote).
async fn app(index: ManifestIndex) -> (Router, ManifestIndex) {
    let auth = ControlAuth::with_static_key(ROOT_KEY);
    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(OkFactory));
    server.mark_ready(pool.clone());
    let state = OrchestratorState {
        server,
        pool,
        index: index.clone(),
        auth,
    };
    let app = build_pipeline(state);
    (app, index)
}

/// Promote through the admin route (the operator path).
async fn promote(app: &Router, name: &str, version: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/admin/workers/{name}/promote?version={version}"
                ))
                .header("authorization", format!("Bearer {ROOT_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "promote {name}@{version}"
    );
}

/// The equivalence assertion: `resolve_default_version` agrees with
/// `resolve_worker(name, None).version` (and the expected value).
fn assert_same(index: &ManifestIndex, name: &str, expected: Option<&str>) {
    let via_worker = index
        .resolve_worker(name, None)
        .ok()
        .map(|worker| worker.version);
    assert_eq!(
        index.resolve_default_version(name),
        via_worker,
        "resolve_default_version must mirror resolve_worker(name, None) for {name}"
    );
    assert_eq!(
        index.resolve_default_version(name),
        expected.map(str::to_owned),
        "expected {expected:?} for {name}"
    );
}

/// Every case the rule has today: no pointer (highest enabled,
/// non-staged), staged excluded, promoted pointer (beats the highest),
/// pointer re-promoted, pointer that no longer names an enabled
/// non-staged version, disabled versions and the absent worker.
#[tokio::test]
async fn resolve_default_version_matches_resolve_worker_in_all_cases() {
    let root = tempfile::tempdir().unwrap();
    let mut index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);

    // --- "app": three enabled non-staged versions, no pointer.
    insert_version(&mut index, root.path(), "app", "1.0.0", true, false);
    insert_version(&mut index, root.path(), "app", "1.5.0", true, false);
    insert_version(&mut index, root.path(), "app", "2.0.0", true, false);
    assert_same(&index, "app", Some("2.0.0"));

    let (app, mut index) = app(index).await;

    // --- Promoted pointer: the pointer beats the highest.
    promote(&app, "app", "1.5.0").await;
    assert_same(&index, "app", Some("1.5.0"));
    // Re-promote: the pointer follows.
    promote(&app, "app", "1.0.0").await;
    assert_same(&index, "app", Some("1.0.0"));

    // --- "staged": a staged version is excluded even when it is the
    // highest; with no pointer the rule falls back to the highest
    // enabled, non-staged.
    insert_version(&mut index, root.path(), "staged", "1.0.0", true, false);
    insert_version(&mut index, root.path(), "staged", "3.0.0", true, true);
    assert_same(&index, "staged", Some("1.0.0"));

    // --- "mixed": a staged HIGHER version plus a non-staged one, and a
    // promoted pointer to the non-staged version.
    insert_version(&mut index, root.path(), "mixed", "2.0.0", true, false);
    insert_version(&mut index, root.path(), "mixed", "5.0.0", true, true);
    promote(&app, "mixed", "2.0.0").await;
    assert_same(&index, "mixed", Some("2.0.0"));

    // --- "pointer-dead": the pointer names a version that is no longer
    // enabled (the marker path disables it): the pointer must fall back
    // to the highest enabled, non-staged. (cPanel's mutual disable is
    // name-specific, so instead the version dir is removed and a lower
    // version promoted: the pointer then names a version absent from the
    // enabled set — the same `filter` the rule applies.)
    insert_version(
        &mut index,
        root.path(),
        "pointer-dead",
        "1.0.0",
        true,
        false,
    );
    insert_version(
        &mut index,
        root.path(),
        "pointer-dead",
        "2.0.0",
        true,
        false,
    );
    promote(&app, "pointer-dead", "2.0.0").await;
    index.remove_worker("pointer-dead", "2.0.0").unwrap();
    assert_same(&index, "pointer-dead", Some("1.0.0"));

    // --- "disabled": every version disabled: nothing resolves.
    insert_version(&mut index, root.path(), "disabled", "1.0.0", false, false);
    insert_version(&mut index, root.path(), "disabled", "2.0.0", false, false);
    assert_same(&index, "disabled", None);

    // --- "unparseable": a version that is not semver: the semver
    // resolution fails, so nothing resolves (both see the same error).
    insert_version(
        &mut index,
        root.path(),
        "unparseable",
        "not-semver",
        true,
        false,
    );
    assert_same(&index, "unparseable", None);

    // --- Absent worker.
    assert_same(&index, "absent", None);
}
