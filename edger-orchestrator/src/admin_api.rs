//! Admin API routes for operational inventory and root-only controls.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use edger_core::{
    principal_has_permission, root_principal, AdminCatalogItem, AdminCatalogResponse,
    AdminErrorResponse, AdminMutationResponse, AdminSessionResponse, AdminWorkerInfo,
    AdminWorkersResponse, ApiKeyPrincipal, CoreError,
};
use serde::Deserialize;
use serde_json::json;

use crate::deploy::{extract_zip, install_worker_from_zip, rescan_workers_and_prewarm};
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
        .route("/api/admin/workers/{name}/enable", post(enable_worker))
        .route("/api/admin/workers/{name}/disable", post(disable_worker))
        .route(
            "/api/admin/workers/{name}/files",
            get(worker_files)
                .post(upload_worker_files)
                .layer(DefaultBodyLimit::max(MAX_BODY_BYTES)),
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
    match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "workers:install")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        install_worker_from_zip(&state.index, &principal, &body)
    }) {
        Ok(installed) => (StatusCode::CREATED, Json(installed)).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

#[derive(Debug, Default, Deserialize)]
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
        rescan_workers_and_prewarm(&state.index, &state.pool, dry_run).await
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

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerErrorsQuery {
    limit: Option<usize>,
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
) -> Result<(std::path::PathBuf, String), CoreError> {
    let workers = state.index.admin_workers_for_principal(principal);
    let worker = workers
        .iter()
        .find(|worker| worker.name == name && version.is_none_or(|value| worker.version == value))
        .ok_or_else(|| CoreError::new("NOT_FOUND", format!("worker {name} not found")))?;
    let base = std::fs::canonicalize(&worker.source).map_err(|err| {
        CoreError::new("NOT_FOUND", format!("worker directory unavailable: {err}"))
    })?;
    Ok((base, worker.version.clone()))
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
    let (base, _) = resolve_worker_dir(state, principal, name, version)?;
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
    let (base, resolved_version) = resolve_worker_dir(state, principal, name, version)?;
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
        "version": resolved_version,
    }))
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
        "BAD_REQUEST" | "DEPLOY_INVALID_PACKAGE" | "DEPLOY_PATH_DENIED" => StatusCode::BAD_REQUEST,
        "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
        "NOT_FOUND" => StatusCode::NOT_FOUND,
        "CSRF_DENIED" | "FORBIDDEN" => StatusCode::FORBIDDEN,
        "COLLISION" | "DEPLOY_TARGET_EXISTS" => StatusCode::CONFLICT,
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
