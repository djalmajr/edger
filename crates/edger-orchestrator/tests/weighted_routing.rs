//! Weighted rollout by session cohort (story 25.03).

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use bytes::Bytes;
use edger_core::{
    Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig, WorkerRef,
};
use edger_orchestrator::routing_policy::{cohort_bucket, version_for_weight_bucket};
use edger_orchestrator::{
    build_pipeline, clear_persisted_routing_policy, load_manifests_from_roots,
    parse_routing_policy, persist_routing_policy, rescan_workers, ControlAuth, ManifestIndex,
    OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<SerializedRequest>>>);

struct Echo {
    recorder: Recorder,
    version: String,
}

impl IsolateFactory for Recorder {
    fn create_isolate(&self, worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(Echo {
            recorder: self.clone(),
            version: worker_ref.version.clone(),
        })
    }
}

#[async_trait]
impl Isolate for Echo {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.recorder.0.lock().unwrap().push(req);
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("set-cookie".into(), "app_session=1; Path=/".into())],
            body: Some(Bytes::from(self.version.clone())),
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
        unreachable!("fetch fixture")
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }
}

fn write_version(root: &Path, directory: &str, version: &str, extra: &str) {
    let dir = root.join(directory);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        format!("name: app\nversion: '{version}'\nentrypoint: index.ts\nkind: fetch\n{extra}"),
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        "export default () => new Response('ok')",
    )
    .unwrap();
}

fn public_traffic(body: &str) -> edger_orchestrator::RoutingPolicy {
    parse_routing_policy(body.as_bytes()).unwrap()
}

fn split_80_20() -> edger_orchestrator::RoutingPolicy {
    public_traffic(
        r#"{"name":"app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":80},{"version":"2.0.0","weight":20}]}}"#,
    )
}

fn load_pair(root: &Path, extra: &str) -> ManifestIndex {
    write_version(root, "v1", "1.0.0", extra);
    write_version(root, "v2", "2.0.0", extra);
    load_manifests_from_roots(&[], None, &[root.to_path_buf()]).unwrap()
}

fn pipeline(index: ManifestIndex, weighted: bool) -> (Router, Recorder, ManifestIndex) {
    let server = ServerState::new_unready();
    if weighted {
        server.enable_weighted_routing();
    }
    let recorder = Recorder::default();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(recorder.clone()));
    server.mark_ready(pool.clone());
    let app = build_pipeline(OrchestratorState {
        server,
        pool,
        index: index.clone(),
        auth: ControlAuth::with_static_key("root-key"),
    });
    (app, recorder, index)
}

struct HttpResult {
    status: StatusCode,
    body: String,
    set_cookies: Vec<String>,
}

async fn send(app: Router, path: &str, host: Option<&str>, cookie: Option<&str>) -> HttpResult {
    let mut req = Request::builder().uri(path);
    if let Some(host) = host {
        req = req.header("host", host);
    }
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    req = req.header("x-tenant-id", "forged-tenant");
    let response = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let set_cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect();
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    HttpResult {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
        set_cookies,
    }
}

fn cohort_cookies(cookies: &[String]) -> Vec<&String> {
    cookies
        .iter()
        .filter(|cookie| cookie.starts_with("edger_cohort="))
        .collect()
}

fn cohort_n(index: u32) -> String {
    format!("{index:08x}-0000-4000-8000-{index:012x}")
}

#[test]
fn ten_thousand_cohorts_land_near_eighty_twenty_and_stay_stable() {
    let policy = split_80_20();
    let traffic = policy.traffic.as_ref().unwrap();
    let mut low = 0u32;
    let mut high = 0u32;
    let mut apps_differ = false;
    for index in 0..10_000u32 {
        let cohort = cohort_n(index);
        let bucket = cohort_bucket("app", &cohort);
        let version = version_for_weight_bucket(traffic, bucket).unwrap();
        match version {
            "1.0.0" => low += 1,
            "2.0.0" => high += 1,
            other => panic!("unexpected version {other}"),
        }
        assert_eq!(
            version_for_weight_bucket(traffic, cohort_bucket("app", &cohort)),
            Some(version)
        );
        if cohort_bucket("app", &cohort) != cohort_bucket("other-app", &cohort) {
            apps_differ = true;
        }
    }
    assert!(
        apps_differ,
        "distinct apps must not share every cohort decision"
    );
    // n=10000, p=0.8, sd=40. ±200 is 5 standard deviations.
    assert!(
        (7800..=8200).contains(&low) && (1800..=2200).contains(&high),
        "80/20 tolerance ±200 counts, got 1.0.0={low} 2.0.0={high}"
    );
}

#[tokio::test]
async fn flag_off_keeps_legacy_default_and_emits_no_cohort_cookie() {
    let root = tempfile::tempdir().unwrap();
    let index = load_pair(root.path(), "");
    let policy = public_traffic(
        r#"{"name":"app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    );
    persist_routing_policy(&index, &policy).unwrap();
    let (app, recorder, index) = pipeline(index, false);
    let response = send(app, "/app", Some("gateway.example.com"), None).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, "2.0.0");
    assert!(cohort_cookies(&response.set_cookies).is_empty());
    assert!(response
        .set_cookies
        .iter()
        .any(|cookie| cookie.starts_with("app_session=")));
    assert_eq!(index.default_version("app"), None);
    let requests = recorder.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .headers
        .iter()
        .all(|(name, _)| !name.eq_ignore_ascii_case("x-tenant-id")));
}

#[tokio::test]
async fn flag_on_splits_path_emits_cookie_and_keeps_the_worker_cookie() {
    let root = tempfile::tempdir().unwrap();
    let index = load_pair(root.path(), "");
    persist_routing_policy(&index, &split_80_20()).unwrap();
    let (app, recorder, index) = pipeline(index, true);
    let first = send(app.clone(), "/app", Some("gateway.example.com"), None).await;
    assert_eq!(first.status, StatusCode::OK);
    let cohort = cohort_cookies(&first.set_cookies);
    assert_eq!(cohort.len(), 1);
    let cookie = cohort[0];
    assert!(cookie.contains("Path=/"), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(
        !cookie.contains("1.0.0") && !cookie.contains("2.0.0"),
        "{cookie}"
    );
    assert!(first
        .set_cookies
        .iter()
        .any(|value| value.starts_with("app_session=")));
    let second = send(
        app,
        "/app",
        Some("gateway.example.com"),
        Some(cookie.as_str()),
    )
    .await;
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(second.body, first.body);
    assert!(cohort_cookies(&second.set_cookies).is_empty());
    assert_eq!(index.default_version("app"), None);
    let requests = recorder.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|req| {
        req.headers
            .iter()
            .all(|(name, _)| !name.eq_ignore_ascii_case("x-tenant-id"))
    }));
}

#[tokio::test]
async fn weight_change_and_delete_apply_on_the_next_request() {
    let root = tempfile::tempdir().unwrap();
    let index = load_pair(root.path(), "");
    persist_routing_policy(&index, &split_80_20()).unwrap();
    let traffic = split_80_20().traffic.unwrap();
    let cohort = (0..10_000u32)
        .map(cohort_n)
        .find(|cohort| {
            version_for_weight_bucket(&traffic, cohort_bucket("app", cohort)) == Some("1.0.0")
        })
        .unwrap();
    let (app, _, _) = pipeline(index.clone(), true);
    let cookie = format!("edger_cohort={cohort}");
    let first = send(app, "/app", None, Some(&cookie)).await;
    assert_eq!(first.body, "1.0.0");

    let only_two = public_traffic(
        r#"{"name":"app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"2.0.0","weight":100}]}}"#,
    );
    persist_routing_policy(&index, &only_two).unwrap();
    let (app, _, _) = pipeline(index.clone(), true);
    let changed = send(app, "/app", None, Some(&cookie)).await;
    assert_eq!(changed.body, "2.0.0");

    clear_persisted_routing_policy(&index, "app").unwrap();
    let (app, _, index) = pipeline(index, true);
    let deleted = send(app, "/app", None, Some(&cookie)).await;
    assert_eq!(deleted.status, StatusCode::OK);
    assert_eq!(deleted.body, "2.0.0");
    assert!(cohort_cookies(&deleted.set_cookies).is_empty());
    assert_eq!(index.default_version("app"), None);
}

#[tokio::test]
async fn pinned_version_ignores_weights() {
    let root = tempfile::tempdir().unwrap();
    let index = load_pair(root.path(), "");
    let policy = public_traffic(
        r#"{"name":"app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"2.0.0","weight":100}]}}"#,
    );
    persist_routing_policy(&index, &policy).unwrap();
    let (app, _, _) = pipeline(index, true);
    let response = send(app, "/app@1.0.0", None, None).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, "1.0.0");
    assert!(cohort_cookies(&response.set_cookies).is_empty());
}

#[tokio::test]
async fn malformed_cohort_is_renewed() {
    let root = tempfile::tempdir().unwrap();
    let index = load_pair(root.path(), "");
    persist_routing_policy(&index, &split_80_20()).unwrap();
    let (app, _, _) = pipeline(index, true);
    let response = send(
        app,
        "/app",
        None,
        Some(&format!("edger_cohort={}", "x".repeat(80))),
    )
    .await;
    assert_eq!(response.status, StatusCode::OK);
    let cookies = cohort_cookies(&response.set_cookies);
    assert_eq!(cookies.len(), 1);
    assert!(!cookies[0].contains(&"x".repeat(80)));
}

#[tokio::test]
async fn host_path_plugin_and_homepage_use_the_same_split() {
    let root = tempfile::tempdir().unwrap();
    let host_index = load_pair(root.path(), "hosts: [shop.example.com]\n");
    persist_routing_policy(&host_index, &split_80_20()).unwrap();
    let cohort = cohort_n(3);
    let expected = version_for_weight_bucket(
        split_80_20().traffic.as_ref().unwrap(),
        cohort_bucket("app", &cohort),
    )
    .unwrap()
    .to_string();
    let cookie = format!("theme=dark; edger_cohort={cohort}");
    let (app, _, _) = pipeline(host_index, true);
    let host = send(app, "/catalog", Some("shop.example.com"), Some(&cookie)).await;
    assert_eq!(host.status, StatusCode::OK);
    assert_eq!(host.body, expected);

    let plugin_root = tempfile::tempdir().unwrap();
    let plugin_index = load_pair(plugin_root.path(), "base: /shop\n");
    persist_routing_policy(&plugin_index, &split_80_20()).unwrap();
    let (app, _, _) = pipeline(plugin_index, true);
    let plugin = send(app, "/shop/item", None, Some(&cookie)).await;
    assert_eq!(plugin.status, StatusCode::OK);
    assert_eq!(plugin.body, expected);

    let home_root = tempfile::tempdir().unwrap();
    let home_index = load_pair(home_root.path(), "base: /\n");
    persist_routing_policy(&home_index, &split_80_20()).unwrap();
    let home = home_index
        .select_weighted_worker("app", &cohort, &split_80_20(), None, None)
        .unwrap()
        .unwrap();
    assert_eq!(home_index.homepage().unwrap().name, "app");
    assert_eq!(home.version, expected);
}

#[tokio::test]
async fn ineligible_traffic_version_is_unavailable_for_every_cohort() {
    let root = tempfile::tempdir().unwrap();
    let index = load_pair(root.path(), "");
    persist_routing_policy(&index, &split_80_20()).unwrap();
    fs::write(
        root.path().join("v2").join(".edger-revision"),
        "revision-v2\nstaged=true\n",
    )
    .unwrap();
    let (app, recorder, _) = pipeline(index, true);
    let staged = send(
        app,
        "/app",
        None,
        Some("edger_cohort=00000000-0000-4000-8000-000000000001"),
    )
    .await;
    assert_eq!(staged.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(recorder.0.lock().unwrap().is_empty());

    let root = tempfile::tempdir().unwrap();
    let index = load_pair(root.path(), "");
    persist_routing_policy(&index, &split_80_20()).unwrap();
    fs::remove_dir_all(root.path().join("v2")).unwrap();
    rescan_workers(&index, false).unwrap();
    let (app, recorder, _) = pipeline(index, true);
    let removed = send(
        app,
        "/app",
        None,
        Some("edger_cohort=00000000-0000-4000-8000-000000000001"),
    )
    .await;
    assert_eq!(removed.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(recorder.0.lock().unwrap().is_empty());

    let root = tempfile::tempdir().unwrap();
    let index = load_pair(root.path(), "");
    persist_routing_policy(&index, &split_80_20()).unwrap();
    index
        .set_worker_enabled("app", Some("2.0.0"), false)
        .unwrap();
    let (app, recorder, _) = pipeline(index, true);
    let disabled = send(app, "/app", None, None).await;
    assert_eq!(disabled.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(recorder.0.lock().unwrap().is_empty());

    let root = tempfile::tempdir().unwrap();
    write_version(root.path(), "v1", "1.0.0", "");
    write_version(root.path(), "v2", "2.0.0", "visibility: internal\n");
    let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
    let err = index
        .select_weighted_worker("app", &cohort_n(1), &split_80_20(), None, None)
        .unwrap_err();
    assert_eq!(err.code, "ROUTING_UNAVAILABLE");

    let root = tempfile::tempdir().unwrap();
    write_version(root.path(), "v1", "1.0.0", "");
    write_version(root.path(), "v2", "2.0.0", "hosts: [shop.example.com]\n");
    let index = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
    persist_routing_policy(&index, &split_80_20()).unwrap();
    let (app, recorder, _) = pipeline(index, true);
    let missing_alias = send(
        app,
        "/catalog",
        Some("shop.example.com"),
        Some("edger_cohort=00000000-0000-4000-8000-000000000001"),
    )
    .await;
    assert_eq!(missing_alias.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(recorder.0.lock().unwrap().is_empty());
}

#[test]
fn core_origin_does_not_enter_the_split() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("cpanel");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("manifest.yaml"),
        "name: cpanel\nversion: '1.0.0'\nentrypoint: index.ts\nkind: fetch\n",
    )
    .unwrap();
    fs::write(
        dir.join("index.ts"),
        "export default () => new Response('ok')",
    )
    .unwrap();
    let index = load_manifests_from_roots(&[root.path().to_path_buf()], None, &[]).unwrap();
    let policy = public_traffic(
        r#"{"name":"cpanel","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    );
    let selected = index
        .select_weighted_worker("cpanel", &cohort_n(1), &policy, None, None)
        .unwrap();
    assert!(selected.is_none());
}
