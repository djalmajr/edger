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

use crate::deploy::{install_worker_from_zip, rescan_workers_and_prewarm};
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
