//! Admin API integration tests (Story 08.02).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use edger_core::{ApiKeyStore, Middleware, RequestContext, SerializedRequest, WorkerManifest};
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use edger_ext_gateway::{GatewayExtension, GatewayRateLimitConfig};
use edger_ext_keyval::SqlKeyValueProvider;
use edger_ext_turso::LocalSqliteProvider;
use edger_isolation::MockIsolate;
use edger_orchestrator::{
    build_pipeline, collect_extensions, AuthGate, AuthGateConfig, ManifestIndex, OrchestratorState,
    ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tower::ServiceExt;

struct StubFactory;

impl IsolateFactory for StubFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn edger_core::Isolate> {
        Box::new(MockIsolate::new())
    }
}

fn test_state() -> OrchestratorState {
    test_state_with_gateway(GatewayExtension::middleware())
}

fn test_state_with_gateway(gateway: Arc<dyn Middleware>) -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/workers/todos"),
            WorkerManifest {
                entrypoint: Some("index.html".into()),
                name: "todos".into(),
                version: Some("1.0.0".into()),
                visibility: Some("public".into()),
                ..Default::default()
            },
        )
        .unwrap();
    index
        .insert(
            PathBuf::from("/workers/acme-api"),
            WorkerManifest {
                name: "@acme/api".into(),
                version: Some("2.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();

    let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
    store
        .insert_key(
            "super-secret-token",
            "operator",
            "viewer",
            &["workers:read".into()],
            &["@acme".into()],
            None,
        )
        .unwrap();

    let auth = Arc::new(AuthExtension::new(store, Some("root-secret".into())));
    let mut registry = collect_extensions(vec![gateway]).unwrap();
    registry.register_auth_provider(auth.clone()).unwrap();
    let durable_sql = Arc::new(LocalSqliteProvider::in_memory());
    registry
        .register_durable_sql_provider(durable_sql.clone())
        .unwrap();
    let key_value = Arc::new(SqlKeyValueProvider::new(durable_sql));
    registry
        .register_key_value_provider(key_value.clone())
        .unwrap();
    registry.register_queue_provider(key_value).unwrap();

    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
    server.mark_ready(pool.clone());

    OrchestratorState {
        server,
        pool,
        index,
        registry,
        auth: AuthGate::new(AuthGateConfig::default(), auth),
    }
}

fn gateway_request(
    request_id: &str,
    uri: &str,
    ip: &str,
    authorization: Option<&str>,
) -> SerializedRequest {
    let mut headers = vec![("x-forwarded-for".into(), ip.into())];
    if let Some(value) = authorization {
        headers.push(("authorization".into(), value.into()));
    }
    SerializedRequest {
        method: "GET".into(),
        uri: uri.into(),
        headers,
        body: None,
        request_id: request_id.into(),
        base_href: None,
    }
}

async fn json_get(path: &str, token: Option<&str>) -> (StatusCode, serde_json::Value) {
    let app = build_pipeline(test_state());
    app_json_get(app, path, token).await
}

async fn app_json_get(
    app: Router,
    path: &str,
    token: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().uri(path);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let res = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn app_get_status(app: Router, path: &str, token: Option<&str>) -> StatusCode {
    let mut builder = Request::builder().uri(path);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

async fn json_post(
    app: Router,
    path: &str,
    token: Option<&str>,
    body: serde_json::Value,
    origin: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        builder = builder
            .header("origin", origin)
            .header("host", "127.0.0.1:19080");
    }
    let res = app
        .oneshot(
            builder
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn json_post_with_request_id(
    app: Router,
    path: &str,
    token: Option<&str>,
    body: serde_json::Value,
    request_id: &str,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("x-request-id", request_id);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let res = app
        .oneshot(
            builder
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn admin_workers_requires_root_and_does_not_fall_through_to_api_stub() {
    let (status, body) = json_get("/api/admin/workers", None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "UNAUTHORIZED");
    assert_ne!(body["code"], "API_STUB");
}

#[tokio::test]
async fn root_lists_workers_with_operational_metadata() {
    let (status, body) = json_get("/api/admin/workers", Some("root-secret")).await;

    assert_eq!(status, StatusCode::OK);
    let workers = body["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 2);
    assert_eq!(workers[0]["name"], "@acme/api");
    assert_eq!(workers[0]["namespace"], "@acme");
    assert_eq!(workers[0]["version"], "2.0.0");
    assert_eq!(workers[0]["status"], "loaded");
    assert_eq!(workers[0]["visibility"], "protected");
    assert!(workers[0]["source"].as_str().unwrap().contains("acme-api"));

    assert_eq!(workers[1]["name"], "todos");
    assert_eq!(workers[1]["visibility"], "public");
}

#[tokio::test]
async fn admin_extensions_requires_root_before_exposing_operational_inventory() {
    let (missing_status, missing_body) = json_get("/api/admin/extensions", None).await;
    assert_eq!(missing_status, StatusCode::UNAUTHORIZED);
    assert_eq!(missing_body["code"], "UNAUTHORIZED");

    let (non_root_status, non_root_body) =
        json_get("/api/admin/extensions", Some("super-secret-token")).await;
    assert_eq!(non_root_status, StatusCode::FORBIDDEN);
    assert_eq!(non_root_body["code"], "FORBIDDEN");
    assert!(!non_root_body.to_string().contains("gateway"));
}

#[tokio::test]
async fn root_lists_operational_extension_inventory_for_middleware_and_provider() {
    let gateway = Arc::new(GatewayExtension::new());
    let ctx = RequestContext::new("extension-inventory-test");
    let mut request = gateway_request(
        "inventory-request",
        "/Users/djalmajr/secret?token=should-not-leak",
        "203.0.113.40",
        Some("Bearer should-not-leak"),
    );
    assert!(gateway.on_request(&mut request, &ctx).unwrap().is_none());

    let app = build_pipeline(test_state_with_gateway(gateway));
    let (status, body) = app_json_get(app, "/api/admin/extensions", Some("root-secret")).await;

    assert_eq!(status, StatusCode::OK);
    let body_text = body.to_string();
    assert!(!body_text.contains("root-secret"));
    assert!(!body_text.contains("should-not-leak"));
    assert!(!body_text.contains("/Users/djalmajr"));

    let extensions = body["extensions"].as_array().unwrap();
    let gateway = extensions
        .iter()
        .find(|ext| ext["name"] == "gateway")
        .expect("gateway extension should be listed");
    assert_eq!(gateway["id"], "extension:gateway");
    assert_eq!(gateway["kind"], "middleware");
    assert_eq!(gateway["status"], "enabled");
    assert_eq!(gateway["configSource"], "staticRegistration");
    assert_eq!(gateway["dependencies"], serde_json::json!([]));
    assert!(gateway["version"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(
        gateway["capabilities"],
        serde_json::json!(["menu:Gateway", "middleware", "onRequest", "onResponse"])
    );
    assert_eq!(
        gateway["manifest"],
        serde_json::json!({
            "config": {
                "keys": [],
                "redacted": true,
                "source": "staticRegistration"
            },
            "hooks": ["onRequest", "onResponse"],
            "menus": [{"name": "Gateway"}],
            "provides": ["middleware"],
            "requirements": []
        })
    );
    assert_eq!(gateway["diagnostics"]["requests"]["total"], 1);
    assert_eq!(gateway["diagnostics"]["rateLimit"]["enabled"], false);
    assert_eq!(
        gateway["diagnostics"]["recentDecisions"][0]["path"],
        "[redacted]"
    );

    let auth = extensions
        .iter()
        .find(|ext| ext["name"] == "auth")
        .expect("auth provider should be listed");
    assert_eq!(auth["id"], "extension:auth");
    assert_eq!(auth["kind"], "authProvider");
    assert_eq!(auth["status"], "enabled");
    assert_eq!(
        auth["capabilities"],
        serde_json::json!(["apiKeys", "authProvider"])
    );
    assert_eq!(
        auth["manifest"]["provides"],
        serde_json::json!(["apiKeys", "authProvider"])
    );
    assert_eq!(auth["manifest"]["config"]["keys"], serde_json::json!([]));
    assert_eq!(auth["manifest"]["config"]["redacted"], true);

    let keyval = extensions
        .iter()
        .find(|ext| ext["name"] == "keyval")
        .expect("keyval service provider should be listed");
    assert_eq!(keyval["id"], "extension:keyval");
    assert_eq!(keyval["kind"], "serviceProvider");
    assert_eq!(keyval["status"], "enabled");
    assert_eq!(
        keyval["capabilities"],
        serde_json::json!(["provider:keyValue", "provider:queue"])
    );
    assert_eq!(
        keyval["dependencies"],
        serde_json::json!(["provider:durableSql"])
    );
    assert_eq!(
        keyval["manifest"],
        serde_json::json!({
            "config": {
                "keys": [],
                "redacted": true,
                "source": "staticRegistration"
            },
            "hooks": [],
            "menus": [],
            "provides": ["provider:keyValue", "provider:queue"],
            "requirements": ["provider:durableSql"]
        })
    );
}

#[tokio::test]
async fn local_extension_validation_contract_reports_manifest_status_diagnostics_and_redaction() {
    let gateway = Arc::new(GatewayExtension::new());
    let ctx = RequestContext::new("local-extension-validation");
    let mut request = gateway_request(
        "local-extension-validation-request",
        "/Users/djalmajr/secret?token=should-not-leak",
        "203.0.113.41",
        Some("Bearer should-not-leak"),
    );
    assert!(gateway.on_request(&mut request, &ctx).unwrap().is_none());

    let app = build_pipeline(test_state_with_gateway(gateway));
    let (status, body) = app_json_get(app, "/api/admin/extensions", Some("root-secret")).await;

    assert_eq!(status, StatusCode::OK);
    let body_text = body.to_string();
    assert!(!body_text.contains("root-secret"));
    assert!(!body_text.contains("should-not-leak"));
    assert!(!body_text.contains("/Users/djalmajr"));

    let extensions = body["extensions"].as_array().unwrap();
    assert!(extensions.len() >= 4);
    for extension in extensions {
        let name = extension["name"].as_str().unwrap();
        assert_eq!(extension["id"], format!("extension:{name}"));
        assert!(extension["version"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert!(matches!(
            extension["kind"].as_str(),
            Some("middleware" | "authProvider" | "serviceProvider")
        ));
        assert!(matches!(
            extension["status"].as_str(),
            Some("enabled" | "disabled")
        ));
        assert_eq!(extension["configSource"], "staticRegistration");
        assert!(extension["capabilities"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        assert!(extension["dependencies"].as_array().is_some());
        assert!(extension["manifest"]["hooks"].as_array().is_some());
        assert!(extension["manifest"]["menus"].as_array().is_some());
        assert!(extension["manifest"]["provides"].as_array().is_some());
        assert!(extension["manifest"]["requirements"].as_array().is_some());
        assert_eq!(extension["manifest"]["config"]["redacted"], true);
        assert_eq!(
            extension["manifest"]["config"]["source"],
            "staticRegistration"
        );
        assert!(extension["manifest"]["config"]["keys"].as_array().is_some());
    }

    let gateway = extensions
        .iter()
        .find(|extension| extension["name"] == "gateway")
        .unwrap();
    assert_eq!(
        gateway["manifest"]["hooks"],
        serde_json::json!(["onRequest", "onResponse"])
    );
    assert_eq!(
        gateway["manifest"]["provides"],
        serde_json::json!(["middleware"])
    );
    assert_eq!(
        gateway["manifest"]["menus"],
        serde_json::json!([{ "name": "Gateway" }])
    );
    assert_eq!(gateway["diagnostics"]["requests"]["total"], 1);
    assert_eq!(
        gateway["diagnostics"]["recentDecisions"][0]["path"],
        "[redacted]"
    );

    let auth = extensions
        .iter()
        .find(|extension| extension["name"] == "auth")
        .unwrap();
    assert_eq!(
        auth["manifest"]["provides"],
        serde_json::json!(["apiKeys", "authProvider"])
    );

    let keyval = extensions
        .iter()
        .find(|extension| extension["name"] == "keyval")
        .unwrap();
    assert_eq!(
        keyval["manifest"]["provides"],
        serde_json::json!(["provider:keyValue", "provider:queue"])
    );
    assert_eq!(
        keyval["manifest"]["requirements"],
        serde_json::json!(["provider:durableSql"])
    );
}

#[tokio::test]
async fn root_catalog_derives_workers_and_module_menu_contributions() {
    let state = test_state();
    state.index.set_worker_enabled("todos", false).unwrap();
    let app = build_pipeline(state);

    let (unauthorized_status, unauthorized_body) =
        app_json_get(app.clone(), "/api/admin/catalog", None).await;
    assert_eq!(unauthorized_status, StatusCode::UNAUTHORIZED);
    assert_eq!(unauthorized_body["code"], "UNAUTHORIZED");
    assert!(!unauthorized_body.to_string().contains("Gateway"));

    let (non_root_status, non_root_body) = app_json_get(
        app.clone(),
        "/api/admin/catalog",
        Some("super-secret-token"),
    )
    .await;
    assert_eq!(non_root_status, StatusCode::FORBIDDEN);
    assert_eq!(non_root_body["code"], "FORBIDDEN");
    assert!(!non_root_body.to_string().contains("Gateway"));

    let (catalog_status, catalog) =
        app_json_get(app, "/api/admin/catalog", Some("root-secret")).await;
    assert_eq!(catalog_status, StatusCode::OK);
    let items = catalog["items"].as_array().unwrap();

    let todos = items
        .iter()
        .find(|item| item["id"] == "worker:todos")
        .expect("todos worker catalog item");
    assert_eq!(todos["title"], "todos");
    assert_eq!(todos["route"], "/todos");
    assert_eq!(todos["kind"], "worker");
    assert_eq!(todos["status"], "disabled");
    assert_eq!(todos["visibility"], "public");

    let acme = items
        .iter()
        .find(|item| item["id"] == "worker:@acme/api")
        .expect("namespaced worker catalog item");
    assert_eq!(acme["route"], "/@acme/api");
    assert_eq!(acme["visibility"], "protected");

    let gateway = items
        .iter()
        .find(|item| item["id"] == "module:gateway:gateway")
        .expect("gateway menu contribution catalog item");
    assert_eq!(gateway["title"], "Gateway");
    assert_eq!(gateway["kind"], "moduleMenu");
    assert_eq!(gateway["owner"], "gateway");
    assert_eq!(gateway["ownerKind"], "middleware");
    assert_eq!(gateway["route"], "#module-gateway");
    assert_eq!(gateway["status"], "enabled");
    assert_eq!(gateway["visibility"], "root");
}

#[tokio::test]
async fn extension_reconcile_requires_root_before_exposing_plan() {
    let status_dir = tempfile::tempdir().unwrap();
    let status_path = status_dir.path().join("extension-status.json");
    let state = test_state();
    state
        .registry
        .load_extension_status_store(&status_path)
        .unwrap();
    fs::write(
        &status_path,
        serde_json::json!({
            "extensions": {
                "gateway": false,
                "edger-ext-new-crate": true
            }
        })
        .to_string(),
    )
    .unwrap();
    let app = build_pipeline(state);

    let (unauthorized_status, unauthorized_body) = json_post(
        app.clone(),
        "/api/admin/extensions/reconcile",
        None,
        serde_json::json!({ "dryRun": false }),
        None,
    )
    .await;
    assert_eq!(unauthorized_status, StatusCode::UNAUTHORIZED);
    assert_eq!(unauthorized_body["code"], "UNAUTHORIZED");
    assert!(!unauthorized_body.to_string().contains("gateway"));

    let (non_root_status, non_root_body) = json_post(
        app.clone(),
        "/api/admin/extensions/reconcile",
        Some("super-secret-token"),
        serde_json::json!({ "dryRun": false }),
        None,
    )
    .await;
    assert_eq!(non_root_status, StatusCode::FORBIDDEN);
    assert_eq!(non_root_body["code"], "FORBIDDEN");
    let non_root_text = non_root_body.to_string();
    assert!(!non_root_text.contains("gateway"));
    assert!(!non_root_text.contains("edger-ext-new-crate"));

    let (csrf_status, csrf_body) = json_post(
        app.clone(),
        "/api/admin/extensions/reconcile",
        Some("root-secret"),
        serde_json::json!({ "dryRun": false }),
        Some("https://evil.local"),
    )
    .await;
    assert_eq!(csrf_status, StatusCode::FORBIDDEN);
    assert_eq!(csrf_body["code"], "CSRF_DENIED");

    let (inventory_status, inventory) =
        app_json_get(app, "/api/admin/extensions", Some("root-secret")).await;
    assert_eq!(inventory_status, StatusCode::OK);
    let gateway = inventory["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|extension| extension["name"] == "gateway")
        .unwrap();
    assert_eq!(gateway["status"], "enabled");
}

#[tokio::test]
async fn extension_reconcile_dry_run_does_not_change_effective_state() {
    let status_dir = tempfile::tempdir().unwrap();
    let status_path = status_dir.path().join("extension-status.json");
    let state = test_state();
    state
        .registry
        .load_extension_status_store(&status_path)
        .unwrap();
    fs::write(
        &status_path,
        serde_json::json!({
            "extensions": {
                "gateway": false
            }
        })
        .to_string(),
    )
    .unwrap();
    let app = build_pipeline(state);
    let (before_status, before_inventory) =
        app_json_get(app.clone(), "/api/admin/extensions", Some("root-secret")).await;
    assert_eq!(before_status, StatusCode::OK);

    let (reconcile_status, reconcile) = json_post_with_request_id(
        app.clone(),
        "/api/admin/extensions/reconcile",
        Some("root-secret"),
        serde_json::json!({ "dryRun": true }),
        "reconcile-dry-run",
    )
    .await;
    assert_eq!(reconcile_status, StatusCode::OK);
    assert_eq!(reconcile["requestId"], "reconcile-dry-run");
    assert_eq!(
        reconcile["actions"],
        serde_json::json!([{
            "action": "disable",
            "applied": false,
            "classification": "runtime",
            "from": true,
            "name": "gateway",
            "to": false
        }])
    );
    assert_eq!(
        reconcile["summary"],
        serde_json::json!({
            "applied": 0,
            "noop": 0,
            "restartRequired": 0,
            "runtime": 1,
            "total": 1
        })
    );
    assert_eq!(
        reconcile["diagnostics"]["desiredSource"],
        "extensionStatusFile"
    );
    assert_eq!(reconcile["diagnostics"]["dryRun"], true);
    assert_eq!(reconcile["diagnostics"]["dynamicLoading"], false);
    assert_eq!(
        reconcile["diagnostics"]["effectiveSource"],
        "inMemoryRegistry"
    );
    assert_eq!(reconcile["diagnostics"]["mode"], "dryRun");
    assert_eq!(reconcile["diagnostics"]["statusStore"], "configured");
    let reconcile_text = reconcile.to_string();
    assert!(!reconcile_text.contains("root-secret"));
    assert!(!reconcile_text.contains(status_path.to_string_lossy().as_ref()));

    let (after_status, after_inventory) =
        app_json_get(app, "/api/admin/extensions", Some("root-secret")).await;
    assert_eq!(after_status, StatusCode::OK);
    assert_eq!(after_inventory, before_inventory);
}

#[tokio::test]
async fn extension_reconcile_applies_runtime_supported_enable_disable() {
    let status_dir = tempfile::tempdir().unwrap();
    let status_path = status_dir.path().join("extension-status.json");
    let state = test_state();
    state
        .registry
        .load_extension_status_store(&status_path)
        .unwrap();
    fs::write(
        &status_path,
        serde_json::json!({
            "extensions": {
                "gateway": false
            }
        })
        .to_string(),
    )
    .unwrap();
    let app = build_pipeline(state);

    let (reconcile_status, reconcile) = json_post_with_request_id(
        app.clone(),
        "/api/admin/extensions/reconcile",
        Some("root-secret"),
        serde_json::json!({ "dryRun": false }),
        "reconcile-apply-disable",
    )
    .await;
    assert_eq!(reconcile_status, StatusCode::OK);
    assert_eq!(reconcile["requestId"], "reconcile-apply-disable");
    assert_eq!(
        reconcile["actions"][0],
        serde_json::json!({
            "action": "disable",
            "applied": true,
            "classification": "runtime",
            "from": true,
            "name": "gateway",
            "to": false
        })
    );
    assert_eq!(reconcile["summary"]["applied"], 1);
    assert_eq!(reconcile["summary"]["restartRequired"], 0);

    let (inventory_status, inventory) =
        app_json_get(app, "/api/admin/extensions", Some("root-secret")).await;
    assert_eq!(inventory_status, StatusCode::OK);
    let gateway = inventory["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|extension| extension["name"] == "gateway")
        .unwrap();
    assert_eq!(gateway["status"], "disabled");
}

#[tokio::test]
async fn extension_reconcile_marks_unknown_desired_extension_restart_required() {
    let status_dir = tempfile::tempdir().unwrap();
    let status_path = status_dir.path().join("extension-status.json");
    let state = test_state();
    state
        .registry
        .load_extension_status_store(&status_path)
        .unwrap();
    fs::write(
        &status_path,
        serde_json::json!({
            "extensions": {
                "edger-ext-new-crate": true
            }
        })
        .to_string(),
    )
    .unwrap();
    let app = build_pipeline(state);

    let (reconcile_status, reconcile) = json_post_with_request_id(
        app.clone(),
        "/api/admin/extensions/reconcile",
        Some("root-secret"),
        serde_json::json!({ "dryRun": false }),
        "reconcile-restart-required",
    )
    .await;
    assert_eq!(reconcile_status, StatusCode::OK);
    assert_eq!(reconcile["requestId"], "reconcile-restart-required");
    assert_eq!(
        reconcile["actions"],
        serde_json::json!([{
            "action": "enable",
            "applied": false,
            "classification": "restartRequired",
            "from": null,
            "name": "edger-ext-new-crate",
            "to": true
        }])
    );
    assert_eq!(
        reconcile["summary"],
        serde_json::json!({
            "applied": 0,
            "noop": 0,
            "restartRequired": 1,
            "runtime": 0,
            "total": 1
        })
    );
    assert_eq!(reconcile["diagnostics"]["dynamicLoading"], false);

    let (inventory_status, inventory) =
        app_json_get(app, "/api/admin/extensions", Some("root-secret")).await;
    assert_eq!(inventory_status, StatusCode::OK);
    assert!(!inventory["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|extension| extension["name"] == "edger-ext-new-crate"));
}

#[tokio::test]
async fn gateway_admin_readonly_api_exposes_stats_config_and_filtered_logs() {
    let gateway =
        Arc::new(GatewayExtension::new().with_rate_limit(GatewayRateLimitConfig::new(1, 60)));
    let ctx = RequestContext::new("gw-admin-test");
    let mut allowed = gateway_request(
        "gw-allowed",
        "/plain",
        "203.0.113.10",
        Some("Bearer should-not-leak"),
    );
    let mut blocked = gateway_request("gw-blocked", "/plain", "203.0.113.10", None);

    assert!(gateway.on_request(&mut allowed, &ctx).unwrap().is_none());
    assert_eq!(
        gateway
            .on_request(&mut blocked, &ctx)
            .unwrap()
            .unwrap()
            .status,
        429
    );

    let gateway_middleware: Arc<dyn Middleware> = gateway;
    let app = build_pipeline(test_state_with_gateway(gateway_middleware));

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/gateway/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let (stats_status, stats) =
        app_json_get(app.clone(), "/api/admin/gateway/stats", Some("root-secret")).await;
    assert_eq!(stats_status, StatusCode::OK);
    assert_eq!(stats["requests"]["total"], 2);
    assert_eq!(stats["requests"]["rateLimited"], 1);
    assert_eq!(stats["rateLimit"]["enabled"], true);
    assert_eq!(stats["config"]["rateLimit"]["maxRequests"], 1);
    assert!(!stats.to_string().contains("should-not-leak"));

    let (config_status, config) = app_json_get(
        app.clone(),
        "/api/admin/gateway/config",
        Some("root-secret"),
    )
    .await;
    assert_eq!(config_status, StatusCode::OK);
    assert_eq!(config["cors"]["origin"], "*");
    assert_eq!(config["redirectRules"]["count"], 0);
    assert_eq!(config["rateLimit"]["enabled"], true);
    assert_eq!(config["rateLimit"]["windowSeconds"], 60);

    let (logs_status, logs) = app_json_get(
        app,
        "/api/admin/gateway/logs?rateLimited=true&limit=1",
        Some("root-secret"),
    )
    .await;
    assert_eq!(logs_status, StatusCode::OK);
    assert_eq!(logs["total"], 2);
    assert_eq!(logs["returned"], 1);
    assert_eq!(logs["logs"][0]["requestId"], "gw-blocked");
    assert_eq!(logs["logs"][0]["rateLimited"], true);
    assert_eq!(logs["logs"][0]["status"], 429);
    assert!(!logs.to_string().contains("authorization"));
    assert!(!logs.to_string().contains("should-not-leak"));
}

#[tokio::test]
async fn gateway_admin_gateway_log_stats_api_aggregates_recent_decisions() {
    let gateway =
        Arc::new(GatewayExtension::new().with_rate_limit(GatewayRateLimitConfig::new(1, 60)));
    let ctx = RequestContext::new("gw-admin-test");
    let mut allowed = gateway_request(
        "gw-allowed",
        "/plain",
        "203.0.113.20",
        Some("Bearer should-not-leak"),
    );
    let mut preflight = SerializedRequest {
        method: "OPTIONS".into(),
        uri: "/plain".into(),
        headers: vec![
            ("origin".into(), "https://app.example.com".into()),
            ("x-forwarded-for".into(), "203.0.113.20".into()),
        ],
        body: None,
        request_id: "gw-preflight".into(),
        base_href: None,
    };
    let mut blocked = gateway_request("gw-blocked", "/plain", "203.0.113.20", None);

    assert!(gateway.on_request(&mut allowed, &ctx).unwrap().is_none());
    assert_eq!(
        gateway
            .on_request(&mut preflight, &ctx)
            .unwrap()
            .unwrap()
            .status,
        204
    );
    assert_eq!(
        gateway
            .on_request(&mut blocked, &ctx)
            .unwrap()
            .unwrap()
            .status,
        429
    );

    let gateway_middleware: Arc<dyn Middleware> = gateway;
    let app = build_pipeline(test_state_with_gateway(gateway_middleware));

    let unauthorized = app_get_status(app.clone(), "/api/admin/gateway/logs/stats", None).await;
    assert_eq!(unauthorized, StatusCode::UNAUTHORIZED);

    let (status, stats) =
        app_json_get(app, "/api/admin/gateway/logs/stats", Some("root-secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stats["total"], 3);
    assert_eq!(stats["rateLimited"], 1);
    assert_eq!(stats["withoutStatus"], 1);
    assert_eq!(stats["byDecision"]["continue"], 1);
    assert_eq!(stats["byDecision"]["preflight"], 1);
    assert_eq!(stats["byDecision"]["rate_limited"], 1);
    assert_eq!(stats["byStatus"]["204"], 1);
    assert_eq!(stats["byStatus"]["429"], 1);
    assert_eq!(stats["duration"]["tracked"], true);
    assert_eq!(stats["duration"]["samples"], 2);
    assert!(stats["duration"]["avgMs"].is_u64());
    assert!(!stats.to_string().contains("authorization"));
    assert!(!stats.to_string().contains("should-not-leak"));
}

#[tokio::test]
async fn gateway_admin_rate_limit_metrics_api_exposes_local_bucket_summary() {
    let gateway =
        Arc::new(GatewayExtension::new().with_rate_limit(GatewayRateLimitConfig::new(2, 60)));
    let ctx = RequestContext::new("gw-admin-rate-limit-test");
    let mut allowed = gateway_request(
        "gw-allowed",
        "/plain",
        "203.0.113.30",
        Some("Bearer should-not-leak"),
    );

    assert!(gateway.on_request(&mut allowed, &ctx).unwrap().is_none());

    let gateway_middleware: Arc<dyn Middleware> = gateway;
    let app = build_pipeline(test_state_with_gateway(gateway_middleware));

    let unauthorized =
        app_get_status(app.clone(), "/api/admin/gateway/rate-limit/metrics", None).await;
    assert_eq!(unauthorized, StatusCode::UNAUTHORIZED);

    let (status, metrics) = app_json_get(
        app,
        "/api/admin/gateway/rate-limit/metrics",
        Some("root-secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(metrics["enabled"], true);
    assert_eq!(metrics["activeBuckets"], 1);
    assert_eq!(metrics["maxRequests"], 2);
    assert_eq!(metrics["windowSeconds"], 60);
    assert_eq!(metrics["scope"], "local-memory");
    assert!(!metrics.to_string().contains("authorization"));
    assert!(!metrics.to_string().contains("should-not-leak"));
    assert!(!metrics.to_string().contains("203.0.113.30"));
}

#[tokio::test]
async fn root_lists_key_metadata_without_raw_secret() {
    let (status, body) = json_get("/api/admin/keys", Some("root-secret")).await;

    assert_eq!(status, StatusCode::OK);
    let body_text = body.to_string();
    assert!(!body_text.contains("super-secret-token"));
    assert!(!body_text.contains("root-secret"));

    let keys = body["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["name"], "operator");
    assert_eq!(keys[0]["role"], "viewer");
    assert_eq!(keys[0]["keyPrefix"], "super-se");
    assert_eq!(keys[0]["isRoot"], false);
}

#[tokio::test]
async fn non_root_key_can_read_session_but_cannot_list_keys() {
    let (session_status, session) =
        json_get("/api/admin/session", Some("super-secret-token")).await;
    assert_eq!(session_status, StatusCode::OK);
    assert_eq!(session["principal"]["name"], "operator");
    assert_eq!(session["principal"]["isRoot"], false);

    let (keys_status, keys_body) = json_get("/api/admin/keys", Some("super-secret-token")).await;
    assert_eq!(keys_status, StatusCode::FORBIDDEN);
    assert_eq!(keys_body["code"], "FORBIDDEN");
}

#[tokio::test]
async fn root_creates_key_once_without_leaking_raw_secret_in_lists() {
    let app = build_pipeline(test_state());

    let (create_status, created) = json_post(
        app.clone(),
        "/api/admin/keys",
        Some("root-secret"),
        serde_json::json!({
            "name": "release operator",
            "role": "operator",
            "permissions": ["workers:read"],
            "namespaces": ["@acme"]
        }),
        None,
    )
    .await;

    assert_eq!(create_status, StatusCode::CREATED);
    let raw_key = created["rawKey"].as_str().unwrap();
    assert!(raw_key.starts_with("edger_"));
    assert_eq!(created["key"]["name"], "release operator");
    assert_eq!(created["key"]["role"], "operator");
    assert_eq!(
        created["key"]["permissions"],
        serde_json::json!(["workers:read"])
    );
    assert_eq!(created["key"]["namespaces"], serde_json::json!(["@acme"]));
    assert_eq!(created["key"]["isRoot"], false);

    let session = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/session")
                .header("authorization", format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session.status(), StatusCode::OK);

    let keys = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/keys")
                .header("authorization", "Bearer root-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(keys.status(), StatusCode::OK);
    let body = axum::body::to_bytes(keys.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();
    assert!(!body_text.contains(raw_key));
    assert!(body_text.contains("release operator"));
}

#[tokio::test]
async fn key_create_and_revoke_are_root_only_and_csrf_guarded() {
    let app = build_pipeline(test_state());

    let (non_root_status, non_root_body) = json_post(
        app.clone(),
        "/api/admin/keys",
        Some("super-secret-token"),
        serde_json::json!({ "name": "denied" }),
        None,
    )
    .await;
    assert_eq!(non_root_status, StatusCode::FORBIDDEN);
    assert_eq!(non_root_body["code"], "FORBIDDEN");

    let (csrf_status, csrf_body) = json_post(
        app.clone(),
        "/api/admin/keys",
        Some("root-secret"),
        serde_json::json!({ "name": "csrf denied" }),
        Some("https://evil.local"),
    )
    .await;
    assert_eq!(csrf_status, StatusCode::FORBIDDEN);
    assert_eq!(csrf_body["code"], "CSRF_DENIED");

    let (create_status, created) = json_post(
        app.clone(),
        "/api/admin/keys",
        Some("root-secret"),
        serde_json::json!({
            "name": "temporary",
            "permissions": ["workers:read"],
            "namespaces": ["@acme"]
        }),
        None,
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);
    let id = created["key"]["id"].as_u64().unwrap();
    let raw_key = created["rawKey"].as_str().unwrap().to_string();

    let (revoke_denied_status, revoke_denied_body) = json_post(
        app.clone(),
        &format!("/api/admin/keys/{id}/revoke"),
        Some(&raw_key),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(revoke_denied_status, StatusCode::FORBIDDEN);
    assert_eq!(revoke_denied_body["code"], "FORBIDDEN");

    let (revoke_status, revoke_body) = json_post(
        app.clone(),
        &format!("/api/admin/keys/{id}/revoke"),
        Some("root-secret"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(revoke_status, StatusCode::OK);
    assert_eq!(revoke_body["id"], id);
    assert_eq!(revoke_body["revoked"], true);
    assert_eq!(revoke_body["status"], "revoked");

    let revoked_session = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/session")
                .header("authorization", format!("Bearer {raw_key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked_session.status(), StatusCode::UNAUTHORIZED);

    let (second_revoke_status, second_revoke_body) = json_post(
        app,
        &format!("/api/admin/keys/{id}/revoke"),
        Some("root-secret"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(second_revoke_status, StatusCode::OK);
    assert_eq!(second_revoke_body["revoked"], false);
    assert_eq!(second_revoke_body["status"], "not_found");
}

#[tokio::test]
async fn worker_mutation_is_protected_and_changes_runtime_routing() {
    let app = build_pipeline(test_state());

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/workers/todos/disable")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(
        app_get_status(app.clone(), "/todos", None).await,
        StatusCode::OK
    );

    let (disable_status, disabled) = json_post(
        app.clone(),
        "/api/admin/workers/todos/disable",
        Some("root-secret"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(disable_status, StatusCode::OK);
    assert_eq!(disabled["code"], "OK");
    assert_eq!(disabled["status"], "disabled");

    let (inventory_status, inventory) =
        app_json_get(app.clone(), "/api/admin/workers", Some("root-secret")).await;
    assert_eq!(inventory_status, StatusCode::OK);
    let todos = inventory["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|worker| worker["name"] == "todos")
        .unwrap();
    assert_eq!(todos["status"], "disabled");
    assert_eq!(
        app_get_status(app.clone(), "/todos", None).await,
        StatusCode::NOT_FOUND
    );

    let (enable_status, enabled) = json_post(
        app.clone(),
        "/api/admin/workers/todos/enable",
        Some("root-secret"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(enable_status, StatusCode::OK);
    assert_eq!(enabled["code"], "OK");
    assert_eq!(enabled["status"], "loaded");
    assert_eq!(app_get_status(app, "/todos", None).await, StatusCode::OK);
}

#[tokio::test]
async fn worker_mutation_accepts_percent_encoded_namespaced_names() {
    let app = build_pipeline(test_state());

    let (disable_status, disabled) = json_post(
        app.clone(),
        "/api/admin/workers/%40acme%2Fapi/disable",
        Some("root-secret"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(disable_status, StatusCode::OK);
    assert_eq!(disabled["code"], "OK");
    assert_eq!(disabled["status"], "disabled");

    let (inventory_status, inventory) =
        app_json_get(app, "/api/admin/workers", Some("root-secret")).await;
    assert_eq!(inventory_status, StatusCode::OK);
    let acme_api = inventory["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|worker| worker["name"] == "@acme/api")
        .unwrap();
    assert_eq!(acme_api["status"], "disabled");
}

#[tokio::test]
async fn extension_mutation_is_protected_and_updates_inventory_status() {
    let app = build_pipeline(test_state());

    let unauthorized = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/extensions/gateway/disable")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let (csrf_status, csrf_body) = json_post(
        app.clone(),
        "/api/admin/extensions/gateway/disable",
        Some("root-secret"),
        serde_json::json!({}),
        Some("https://evil.local"),
    )
    .await;
    assert_eq!(csrf_status, StatusCode::FORBIDDEN);
    assert_eq!(csrf_body["code"], "CSRF_DENIED");

    let (disable_status, disabled) = json_post(
        app.clone(),
        "/api/admin/extensions/gateway/disable",
        Some("root-secret"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(disable_status, StatusCode::OK);
    assert_eq!(disabled["code"], "OK");
    assert_eq!(disabled["status"], "disabled");

    let (inventory_status, inventory) =
        app_json_get(app.clone(), "/api/admin/extensions", Some("root-secret")).await;
    assert_eq!(inventory_status, StatusCode::OK);
    let gateway = inventory["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|extension| extension["name"] == "gateway")
        .unwrap();
    assert_eq!(gateway["status"], "disabled");

    let (enable_status, enabled) = json_post(
        app.clone(),
        "/api/admin/extensions/gateway/enable",
        Some("root-secret"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(enable_status, StatusCode::OK);
    assert_eq!(enabled["code"], "OK");
    assert_eq!(enabled["status"], "enabled");
}

#[tokio::test]
async fn extension_enable_disable_status_persists_for_rebuilt_registry() {
    let status_dir = tempfile::tempdir().unwrap();
    let status_path = status_dir.path().join("extension-status.json");
    let state = test_state();
    state
        .registry
        .load_extension_status_store(&status_path)
        .unwrap();
    let app = build_pipeline(state);

    let (disable_status, disabled) = json_post(
        app,
        "/api/admin/extensions/gateway/disable",
        Some("root-secret"),
        serde_json::json!({}),
        None,
    )
    .await;

    assert_eq!(disable_status, StatusCode::OK);
    assert_eq!(disabled["status"], "disabled");
    let stored: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&status_path).unwrap()).unwrap();
    assert_eq!(stored["extensions"]["gateway"], false);

    let rebuilt_state = test_state();
    rebuilt_state
        .registry
        .load_extension_status_store(&status_path)
        .unwrap();
    let (inventory_status, inventory) = app_json_get(
        build_pipeline(rebuilt_state),
        "/api/admin/extensions",
        Some("root-secret"),
    )
    .await;

    assert_eq!(inventory_status, StatusCode::OK);
    let gateway = inventory["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|extension| extension["name"] == "gateway")
        .unwrap();
    assert_eq!(gateway["status"], "disabled");
}
