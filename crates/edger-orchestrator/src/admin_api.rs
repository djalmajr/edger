//! Admin API routes for operational inventory and root-only controls.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use edger_core::{
    principal_has_permission, root_principal, AdminCatalogItem, AdminCatalogResponse,
    AdminErrorResponse, AdminMutationResponse, AdminSessionResponse, AdminWorkerInfo,
    AdminWorkersResponse, ApiKeyPrincipal, CoreError, SerializedRequest, WorkerOrigin,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::deploy::{extract_zip, install_worker_from_zip, run_worker_release_with_events};
use crate::operational_log::log_operational_error;
use crate::pipeline::OrchestratorState;
use crate::security::validate_admin_mutation_security;
use crate::server::request_id_from_headers;
use crate::wire::MAX_BODY_BYTES;

pub fn router() -> Router<OrchestratorState> {
    Router::new()
        .route("/api/admin/session", get(session))
        .route("/api/admin/catalog", get(catalog))
        .route("/api/admin/workers", get(list_workers))
        .route(
            "/api/admin/workers/install",
            post(install_worker).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route("/api/admin/workers/rescan", post(rescan_workers_route))
        .route(
            "/api/admin/workers/error-summary",
            get(worker_error_summary),
        )
        .route("/api/admin/workers/{name}/errors", get(worker_errors))
        .route("/api/admin/observability/events", get(observability_events))
        .route("/api/admin/observability/series", get(observability_series))
        .route(
            "/api/admin/observability/events/stream",
            get(observability_events_stream),
        )
        .route("/api/admin/workers/{name}/enable", post(enable_worker))
        .route("/api/admin/workers/{name}/disable", post(disable_worker))
        .route(
            "/api/admin/workers/{name}/health-check",
            post(run_health_check),
        )
        .route(
            "/api/admin/workers/{name}/files",
            get(worker_files)
                .post(upload_worker_files)
                .layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
        )
        .route(
            "/api/admin/workers/{name}/files/download",
            get(download_worker_files),
        )
}

async fn session(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match authenticate(&state, &headers).await {
        Ok(principal) => Json(AdminSessionResponse { principal }).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn catalog(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match require_root(&state, &headers).await {
        Ok(_) => Json(AdminCatalogResponse {
            items: build_catalog(state.index.admin_workers()),
        })
        .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn list_workers(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "workers:read")?;
        Ok(principal)
    }) {
        Ok(principal) => Json(AdminWorkersResponse {
            workers: state.index.admin_workers_for_principal(&principal),
        })
        .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

fn build_catalog(workers: Vec<AdminWorkerInfo>) -> Vec<AdminCatalogItem> {
    let mut items = Vec::new();
    for worker in workers {
        items.push(worker_catalog_item(&worker));
    }
    items.sort_by(|a, b| {
        a.title
            .cmp(&b.title)
            .then_with(|| a.owner.cmp(&b.owner))
            .then_with(|| a.id.cmp(&b.id))
    });
    items
}

fn worker_catalog_item(worker: &AdminWorkerInfo) -> AdminCatalogItem {
    AdminCatalogItem {
        id: format!("worker:{}", worker.name),
        kind: "worker".into(),
        owner: worker.name.clone(),
        owner_kind: "worker".into(),
        route: worker
            .plugin_base
            .clone()
            .unwrap_or_else(|| format!("/{}", worker.name)),
        source: worker.source.clone(),
        status: worker.status.clone(),
        title: worker.name.clone(),
    }
}

async fn install_worker(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "workers:install")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        let mut installed = install_worker_from_zip(&state.index, &principal, &body)?;
        let candidate = state
            .index
            .worker_refs()
            .into_iter()
            .find(|worker| worker.name == installed.name && worker.version == installed.version)
            .ok_or_else(|| CoreError::new("DEPLOY_INTERNAL", "installed worker was not indexed"))?;
        if let Err(error) =
            run_worker_release_with_events(&candidate, &state.server.operational_events()).await
        {
            rollback_failed_install(&state, &installed)?;
            return Err(error);
        }
        installed.release = if candidate.config.release_command.is_some() {
            "completed"
        } else {
            "not_configured"
        }
        .into();
        if candidate
            .config
            .health_check
            .as_ref()
            .is_some_and(|check| check.mode == edger_core::WorkerHealthCheckMode::OnDeploy)
        {
            let check = match execute_worker_health_check(
                &state,
                &installed.name,
                &installed.version,
                "on-deploy",
            )
            .await
            {
                Ok(check) => check,
                Err(error) => {
                    rollback_failed_install(&state, &installed)?;
                    return Err(error);
                }
            };
            if !check.healthy {
                rollback_failed_install(&state, &installed)?;
                return Err(CoreError::new(
                    "DEPLOY_HEALTH_CHECK_FAILED",
                    format!(
                        "health check failed for {}@{}: {}",
                        installed.name, installed.version, check.message
                    ),
                ));
            }
            installed.health = "passed".into();
        } else {
            installed.health = "not_configured".into();
        }
        if let Err(error) =
            state
                .index
                .set_worker_enabled(&installed.name, Some(&installed.version), true)
        {
            rollback_failed_install(&state, &installed)?;
            return Err(error);
        }
        installed.activation = "active".into();
        installed.default_version = state.index.resolve_worker(&installed.name, None)?.version;
        Ok(installed)
    }
    .await;
    match result {
        Ok(installed) => (StatusCode::CREATED, Json(installed)).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

fn rollback_failed_install(
    state: &OrchestratorState,
    installed: &crate::deploy::InstalledWorker,
) -> Result<(), CoreError> {
    state
        .index
        .remove_worker(&installed.name, &installed.version)?;
    let source = PathBuf::from(&installed.source);
    fs::remove_dir_all(&source).map_err(|error| {
        CoreError::new(
            "DEPLOY_ROLLBACK_FAILED",
            format!(
                "failed to remove rejected candidate {}@{} from {}: {error}",
                installed.name,
                installed.version,
                source.display()
            ),
        )
    })
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RescanRequest {
    dry_run: Option<bool>,
}

async fn rescan_workers_route(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let dry_run = serde_json::from_slice::<RescanRequest>(&body)
        .ok()
        .and_then(|request| request.dry_run)
        .unwrap_or(true);
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "workers:install")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        crate::deploy::rescan_workers_and_prewarm_with_events(
            &state.index,
            &state.pool,
            &state.server.operational_events(),
            dry_run,
        )
        .await
    }
    .await;
    match result {
        Ok(report) => Json(report).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerVersionQuery {
    version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkerHealthCheckResult {
    worker: String,
    version: String,
    path: String,
    method: String,
    trigger: &'static str,
    healthy: bool,
    status: Option<u16>,
    duration_ms: u64,
    code: Option<String>,
    message: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerErrorsQuery {
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObservabilityEventsQuery {
    before: Option<u64>,
    limit: Option<usize>,
    since_ms: Option<u128>,
    until_ms: Option<u128>,
    namespace: Option<String>,
    worker: Option<String>,
    version: Option<String>,
    process_id: Option<String>,
    source: Option<String>,
    kind: Option<String>,
    level: Option<String>,
    outcome: Option<String>,
    status: Option<u16>,
    request_id: Option<String>,
    trace_id: Option<String>,
    cursor: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObservabilitySeriesQuery {
    namespace: Option<String>,
    worker: Option<String>,
    version: Option<String>,
    window_ms: Option<u64>,
    bucket_ms: Option<u64>,
}

struct EventTailState {
    store: crate::observability::OperationalStore,
    receiver: tokio::sync::broadcast::Receiver<u64>,
    query: crate::observability::OperationalEventQuery,
    cursor: u64,
    pending: VecDeque<crate::observability::OperationalEvent>,
    gap_pending: Option<(Option<u64>, Option<u64>)>,
}

impl From<ObservabilityEventsQuery> for crate::observability::OperationalEventQuery {
    fn from(query: ObservabilityEventsQuery) -> Self {
        Self {
            before: query.before,
            limit: query.limit,
            since_ms: query.since_ms,
            until_ms: query.until_ms,
            namespace: query.namespace,
            worker: query.worker,
            version: query.version,
            process_id: query.process_id,
            source: query.source,
            kind: query.kind,
            level: query.level,
            outcome: query.outcome,
            status: query.status,
            request_id: query.request_id,
            trace_id: query.trace_id,
        }
    }
}

async fn worker_error_summary(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
) -> Response {
    match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "workers:read")?;
        Ok(principal)
    }) {
        Ok(_) => Json(json!({ "summary": state.server.worker_errors().summary() })).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn worker_errors(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerErrorsQuery>,
) -> Response {
    match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "workers:read")?;
        Ok(principal)
    }) {
        Ok(_) => {
            let limit = query.limit.unwrap_or(10).min(20);
            let errors = state.server.worker_errors().recent(&name, limit);
            Json(json!({ "worker": name, "errors": errors })).into_response()
        }
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn observability_events(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Query(query): Query<ObservabilityEventsQuery>,
) -> Response {
    match require_root(&state, &headers).await {
        Ok(_) => Json(state.server.operational_events().query(query.into())).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn observability_series(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Query(query): Query<ObservabilitySeriesQuery>,
) -> Response {
    if let Err(err) = require_root(&state, &headers).await {
        return admin_error(map_error_status(&err), &err, &headers);
    }
    let event_query = crate::observability::OperationalEventQuery {
        namespace: query.namespace,
        worker: query.worker,
        version: query.version,
        ..Default::default()
    };
    Json(state.server.operational_events().series(
        event_query,
        query.window_ms.unwrap_or(5 * 60_000),
        query.bucket_ms.unwrap_or(15_000),
    ))
    .into_response()
}

async fn observability_events_stream(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Query(query): Query<ObservabilityEventsQuery>,
) -> Response {
    if let Err(err) = require_root(&state, &headers).await {
        return admin_error(map_error_status(&err), &err, &headers);
    }
    let cursor = query.cursor.or_else(|| {
        headers
            .get("last-event-id")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
    });
    let store = state.server.operational_events();
    let receiver = store.subscribe();
    let stream = futures_util::stream::unfold(
        EventTailState {
            store,
            receiver,
            query: query.into(),
            cursor: cursor.unwrap_or_default(),
            pending: VecDeque::new(),
            gap_pending: None,
        },
        |mut state| async move {
            loop {
                if let Some((oldest, newest)) = state.gap_pending.take() {
                    let payload = json!({
                        "gap": true,
                        "oldestAvailable": oldest,
                        "newestAvailable": newest,
                    });
                    return Some((
                        Ok::<_, Infallible>(
                            Event::default().event("gap").data(payload.to_string()),
                        ),
                        state,
                    ));
                }
                if let Some(event) = state.pending.pop_front() {
                    state.cursor = event.id;
                    let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".into());
                    return Some((
                        Ok::<_, Infallible>(
                            Event::default()
                                .id(event.id.to_string())
                                .event("operational_event")
                                .data(payload),
                        ),
                        state,
                    ));
                }

                let tail = state.store.tail(state.query.clone(), state.cursor);
                if tail.gap {
                    state.gap_pending = Some((tail.oldest_available, tail.newest_available));
                    if let Some(oldest) = tail.oldest_available {
                        state.cursor = oldest.saturating_sub(1);
                    }
                }
                state.pending = tail.events.into();
                if state.gap_pending.is_some() || !state.pending.is_empty() {
                    continue;
                }
                match state.receiver.recv().await {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

async fn enable_worker(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerVersionQuery>,
) -> Response {
    worker_mutation(state, headers, name, query.version, true).await
}

async fn disable_worker(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerVersionQuery>,
) -> Response {
    worker_mutation(state, headers, name, query.version, false).await
}

async fn run_health_check(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerVersionQuery>,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "workers:read")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        if principal.role != "admin" {
            return Err(CoreError::new("FORBIDDEN", "admin role required"));
        }
        let version = query
            .version
            .as_deref()
            .ok_or_else(|| CoreError::validation("version", "version is required"))?;
        execute_worker_health_check(&state, &name, version, "manual").await
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn execute_worker_health_check(
    state: &OrchestratorState,
    name: &str,
    version: &str,
    trigger: &'static str,
) -> Result<WorkerHealthCheckResult, CoreError> {
    let worker = state
        .index
        .worker_refs()
        .into_iter()
        .find(|worker| worker.name == name && worker.version == version)
        .ok_or_else(|| CoreError::new("NOT_FOUND", format!("worker {name}@{version} not found")))?;
    let check = worker.config.health_check.clone().ok_or_else(|| {
        CoreError::new(
            "HEALTH_CHECK_NOT_CONFIGURED",
            format!("worker {name}@{version} has no healthCheck configuration"),
        )
    })?;
    let request_id = format!("health-check-{}", uuid::Uuid::new_v4());
    let request = SerializedRequest {
        method: check.method.clone(),
        uri: check.path.clone(),
        headers: vec![
            ("x-request-id".into(), request_id.clone()),
            ("x-edger-health-check".into(), trigger.into()),
        ],
        body: None,
        request_id: request_id.clone(),
        base_href: Some(format!("/{name}/")),
    };
    let started = std::time::Instant::now();
    let dispatch = tokio::time::timeout(
        Duration::from_millis(check.timeout_ms),
        state
            .pool
            .fetch_worker(&worker, request, Some(worker.kind.clone())),
    )
    .await;
    let duration_ms = started.elapsed().as_millis().max(1) as u64;
    let (healthy, status, code, message) = match dispatch {
        Ok(Ok(response)) if (200..400).contains(&response.status) => (
            true,
            Some(response.status),
            None,
            "Health check completed successfully".into(),
        ),
        Ok(Ok(response)) => (
            false,
            Some(response.status),
            Some("HEALTH_CHECK_STATUS".into()),
            format!("Health check returned status {}", response.status),
        ),
        Ok(Err(error)) => (
            false,
            None,
            Some("HEALTH_CHECK_DISPATCH".into()),
            format!("Health check dispatch failed: {error}"),
        ),
        Err(_) => (
            false,
            None,
            Some("HEALTH_CHECK_TIMEOUT".into()),
            format!("Health check exceeded {}ms", check.timeout_ms),
        ),
    };
    state
        .server
        .operational_events()
        .record(crate::observability::OperationalEventInput {
            source: crate::observability::OperationalEventSource::Runtime,
            kind: "health_check".into(),
            level: if healthy {
                crate::observability::OperationalEventLevel::Info
            } else {
                crate::observability::OperationalEventLevel::Error
            },
            namespace: worker.namespace.clone(),
            worker: Some(worker.name.clone()),
            version: Some(worker.version.clone()),
            process_id: None,
            request_id: Some(request_id),
            trace_id: None,
            outcome: Some(if healthy { "healthy" } else { "failed" }.into()),
            status,
            duration_ms: Some(duration_ms),
            code: code.clone(),
            message: Some(message.clone()),
            truncated: None,
            dropped_count: None,
        });
    Ok(WorkerHealthCheckResult {
        worker: worker.name,
        version: worker.version,
        path: check.path,
        method: check.method,
        trigger,
        healthy,
        status,
        duration_ms,
        code,
        message,
    })
}

async fn worker_mutation(
    state: OrchestratorState,
    headers: HeaderMap,
    name: String,
    version: Option<String>,
    enabled: bool,
) -> Response {
    match require_root(&state, &headers).await.and_then(|principal| {
        validate_admin_mutation_security("POST", &headers, &principal)?;
        state
            .index
            .set_worker_enabled(&name, version.as_deref(), enabled)
    }) {
        Ok(worker) => Json(AdminMutationResponse {
            code: "OK".into(),
            message: format!(
                "worker {}@{} {}",
                worker.name, worker.version, worker.status
            ),
            status: worker.status,
        })
        .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerFilesQuery {
    version: Option<String>,
    path: Option<String>,
}

async fn worker_files(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerFilesQuery>,
) -> Response {
    match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "workers:read")?;
        list_worker_files(
            &state,
            &principal,
            &name,
            query.version.as_deref(),
            query.path.as_deref(),
        )
    }) {
        Ok(payload) => Json(payload).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

struct WorkerFileDownload {
    bytes: Vec<u8>,
    content_type: &'static str,
    filename: String,
}

const MAX_DOWNLOAD_ENTRIES: usize = 10_000;

async fn download_worker_files(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerFilesQuery>,
) -> Response {
    match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "workers:read")?;
        build_worker_file_download(
            &state,
            &principal,
            &name,
            query.version.as_deref(),
            query.path.as_deref(),
        )
    }) {
        Ok(download) => {
            let mut response_headers = HeaderMap::new();
            response_headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_static(download.content_type),
            );
            response_headers.insert(
                CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!("attachment; filename=\"{}\"", download.filename))
                    .expect("download filename is sanitized"),
            );
            (response_headers, download.bytes).into_response()
        }
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

// Drop-to-publish: extracts an uploaded zip (client-zipped files/folders) into
// the version's directory at `path`, overwriting in place. Same gate as
// install; `extract_zip` rejects zip-slip on every entry name.
async fn upload_worker_files(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerFilesQuery>,
    body: Bytes,
) -> Response {
    match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "workers:install")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        write_worker_files(
            &state,
            &principal,
            &name,
            query.version.as_deref(),
            query.path.as_deref(),
            &body,
        )
    }) {
        Ok(payload) => Json(payload).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

// Canonical on-disk directory for a worker@version the principal can see.
fn resolve_worker_dir(
    state: &OrchestratorState,
    principal: &ApiKeyPrincipal,
    name: &str,
    version: Option<&str>,
) -> Result<(std::path::PathBuf, AdminWorkerInfo), CoreError> {
    let workers = state.index.admin_workers_for_principal(principal);
    let worker = workers
        .iter()
        .find(|worker| worker.name == name && version.is_none_or(|value| worker.version == value))
        .ok_or_else(|| CoreError::new("NOT_FOUND", format!("worker {name} not found")))?;
    let base = std::fs::canonicalize(&worker.source).map_err(|err| {
        CoreError::new("NOT_FOUND", format!("worker directory unavailable: {err}"))
    })?;
    Ok((base, worker.clone()))
}

// Canonicalize base + sub_path and confirm the result stays within base.
fn resolve_within(base: &std::path::Path, sub_path: &str) -> Result<std::path::PathBuf, CoreError> {
    let requested = if sub_path.is_empty() {
        base.to_path_buf()
    } else {
        base.join(sub_path)
    };
    let target = std::fs::canonicalize(&requested)
        .map_err(|err| CoreError::new("NOT_FOUND", format!("path not found: {err}")))?;
    if !target.starts_with(base) {
        return Err(CoreError::new(
            "FORBIDDEN",
            "path escapes the worker directory",
        ));
    }
    Ok(target)
}

fn write_worker_files(
    state: &OrchestratorState,
    principal: &ApiKeyPrincipal,
    name: &str,
    version: Option<&str>,
    sub_path: Option<&str>,
    zip_bytes: &[u8],
) -> Result<serde_json::Value, CoreError> {
    let (base, worker) = resolve_worker_dir(state, principal, name, version)?;
    if worker.origin != WorkerOrigin::User {
        return Err(CoreError::new(
            "FORBIDDEN",
            "core worker files are read-only; publish a new core overlay version instead",
        ));
    }
    let rel = sub_path.unwrap_or("").trim_matches('/');
    if rel.split('/').any(|segment| segment == "..") {
        return Err(CoreError::new(
            "FORBIDDEN",
            "path escapes the worker directory",
        ));
    }
    let requested = if rel.is_empty() {
        base.clone()
    } else {
        base.join(rel)
    };
    std::fs::create_dir_all(&requested)
        .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot create path: {err}")))?;
    let dest = resolve_within(&base, rel)?;
    extract_zip(zip_bytes, &dest)?;
    list_worker_files(state, principal, name, version, sub_path)
}

// Read-only browse of a deployed version's directory. The dir comes from the
// admin index (`source`), scoped to what the principal can see; the requested
// subpath is canonicalized and checked to stay within that dir (no traversal).
fn list_worker_files(
    state: &OrchestratorState,
    principal: &ApiKeyPrincipal,
    name: &str,
    version: Option<&str>,
    sub_path: Option<&str>,
) -> Result<serde_json::Value, CoreError> {
    let (base, worker) = resolve_worker_dir(state, principal, name, version)?;
    let rel = sub_path.unwrap_or("").trim_matches('/');
    let target = resolve_within(&base, rel)?;

    let read = std::fs::read_dir(&target)
        .map_err(|err| CoreError::new("BAD_REQUEST", format!("not a directory: {err}")))?;
    let mut entries = Vec::new();
    for entry in read.flatten() {
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().is_some_and(|meta| meta.is_dir());
        let size = meta.as_ref().map(|meta| meta.len()).unwrap_or(0);
        entries.push(json!({
            "kind": if is_dir { "dir" } else { "file" },
            "name": entry.file_name().to_string_lossy(),
            "size": size,
        }));
    }
    entries.sort_by(|a, b| {
        let a_dir = a["kind"] == "dir";
        let b_dir = b["kind"] == "dir";
        b_dir.cmp(&a_dir).then_with(|| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        })
    });

    Ok(json!({
        "entries": entries,
        "name": name,
        "path": rel,
        "version": worker.version,
    }))
}

fn build_worker_file_download(
    state: &OrchestratorState,
    principal: &ApiKeyPrincipal,
    name: &str,
    version: Option<&str>,
    sub_path: Option<&str>,
) -> Result<WorkerFileDownload, CoreError> {
    let (base, worker) = resolve_worker_dir(state, principal, name, version)?;
    let rel = sub_path.unwrap_or("").trim_matches('/');
    let target = resolve_within(&base, rel)?;
    let metadata = std::fs::symlink_metadata(&target)
        .map_err(|err| CoreError::new("NOT_FOUND", format!("path not found: {err}")))?;
    if metadata.file_type().is_symlink() {
        return Err(CoreError::new(
            "FORBIDDEN",
            "symlink downloads are not allowed",
        ));
    }

    let fallback = format!("{}-{}", worker.name, worker.version);
    let target_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&fallback);
    if metadata.is_file() {
        if metadata.len() > MAX_BODY_BYTES as u64 {
            return Err(download_too_large());
        }
        let bytes = std::fs::read(&target)
            .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot read file: {err}")))?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(download_too_large());
        }
        return Ok(WorkerFileDownload {
            bytes,
            content_type: "application/octet-stream",
            filename: sanitize_download_filename(target_name),
        });
    }
    if !metadata.is_dir() {
        return Err(CoreError::new(
            "BAD_REQUEST",
            "path is not a file or directory",
        ));
    }

    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let mut total_bytes = 0_u64;
    let mut total_entries = 0_usize;
    append_directory_to_zip(
        &mut writer,
        &target,
        &target,
        &mut total_bytes,
        &mut total_entries,
    )?;
    let archive = writer
        .finish()
        .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot finish archive: {err}")))?
        .into_inner();
    if archive.len() > MAX_BODY_BYTES {
        return Err(download_too_large());
    }
    Ok(WorkerFileDownload {
        bytes: archive,
        content_type: "application/zip",
        filename: format!("{}.zip", sanitize_download_filename(target_name)),
    })
}

fn append_directory_to_zip<W: Write + Seek>(
    writer: &mut zip::ZipWriter<W>,
    root: &std::path::Path,
    directory: &std::path::Path,
    total_bytes: &mut u64,
    total_entries: &mut usize,
) -> Result<(), CoreError> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot read directory: {err}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot read directory: {err}")))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        *total_entries = total_entries
            .checked_add(1)
            .ok_or_else(download_too_large)?;
        if *total_entries > MAX_DOWNLOAD_ENTRIES {
            return Err(download_too_large());
        }
        let file_type = entry
            .file_type()
            .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot inspect entry: {err}")))?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| CoreError::new("FORBIDDEN", "path escapes the download directory"))?;
        let zip_name = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if file_type.is_dir() {
            writer
                .add_directory(
                    format!("{zip_name}/"),
                    zip::write::SimpleFileOptions::default(),
                )
                .map_err(|err| {
                    CoreError::new("BAD_REQUEST", format!("cannot archive directory: {err}"))
                })?;
            append_directory_to_zip(writer, root, &path, total_bytes, total_entries)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let length = entry
            .metadata()
            .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot inspect file: {err}")))?
            .len();
        *total_bytes = total_bytes
            .checked_add(length)
            .ok_or_else(download_too_large)?;
        if *total_bytes > MAX_BODY_BYTES as u64 {
            return Err(download_too_large());
        }
        writer
            .start_file(zip_name, zip::write::SimpleFileOptions::default())
            .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot archive file: {err}")))?;
        let file = std::fs::File::open(&path)
            .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot read file: {err}")))?;
        std::io::copy(&mut file.take(length), writer)
            .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot archive file: {err}")))?;
    }
    Ok(())
}

fn sanitize_download_filename(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "download".into()
    } else {
        sanitized
    }
}

fn download_too_large() -> CoreError {
    CoreError::new(
        "DOWNLOAD_TOO_LARGE",
        format!("download exceeds the {} byte limit", MAX_BODY_BYTES),
    )
}

async fn authenticate(
    state: &OrchestratorState,
    headers: &HeaderMap,
) -> Result<ApiKeyPrincipal, CoreError> {
    if state.auth.is_open() {
        return Ok(root_principal());
    }

    state
        .auth
        .authenticate_headers(headers)
        .await
        .ok_or_else(|| CoreError::new("UNAUTHORIZED", "missing or invalid API key"))
}

async fn require_root(
    state: &OrchestratorState,
    headers: &HeaderMap,
) -> Result<ApiKeyPrincipal, CoreError> {
    let principal = authenticate(state, headers).await?;
    if principal.is_root {
        Ok(principal)
    } else {
        Err(CoreError::new(
            "FORBIDDEN",
            "admin API requires root credentials",
        ))
    }
}

fn require_permission(principal: &ApiKeyPrincipal, permission: &str) -> Result<(), CoreError> {
    if principal_has_permission(principal, permission) {
        Ok(())
    } else {
        Err(CoreError::new(
            "FORBIDDEN",
            format!("permission required: {permission}"),
        ))
    }
}

fn map_error_status(err: &CoreError) -> StatusCode {
    match err.code.as_str() {
        "BAD_REQUEST" | "VALIDATION_ERROR" | "DEPLOY_INVALID_PACKAGE" | "DEPLOY_PATH_DENIED" => {
            StatusCode::BAD_REQUEST
        }
        "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
        "NOT_FOUND" => StatusCode::NOT_FOUND,
        "CSRF_DENIED" | "FORBIDDEN" => StatusCode::FORBIDDEN,
        "DOWNLOAD_TOO_LARGE" => StatusCode::PAYLOAD_TOO_LARGE,
        "COLLISION"
        | "CORE_DEFAULT_REQUIRED"
        | "DEPLOY_TARGET_EXISTS"
        | "DEPLOY_HEALTH_CHECK_FAILED"
        | "HEALTH_CHECK_NOT_CONFIGURED" => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn admin_error(status: StatusCode, err: &CoreError, headers: &HeaderMap) -> Response {
    let request_id = request_id_from_headers(headers);
    log_operational_error("admin_api", request_id.as_deref(), status, err);
    (
        status,
        Json(AdminErrorResponse {
            code: err.code.clone(),
            message: err.message.clone(),
        }),
    )
        .into_response()
}
