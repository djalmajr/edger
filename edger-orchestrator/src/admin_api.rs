//! Admin API routes for operational inventory and root-only controls.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use edger_core::{
    principal_has_permission, AdminApiKeysResponse, AdminCatalogItem, AdminCatalogResponse,
    AdminCreateApiKeyRequest, AdminCreateApiKeyResponse, AdminErrorResponse, AdminExtensionInfo,
    AdminExtensionReconcileRequest, AdminExtensionsResponse, AdminMutationResponse,
    AdminRevokeApiKeyResponse, AdminSessionResponse, AdminWorkerInfo, AdminWorkersResponse,
    ApiKeyPrincipal, CoreError,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::deploy::{install_worker_from_zip, rescan_workers};
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
        .route("/api/admin/extensions", get(list_extensions))
        .route(
            "/api/admin/extensions/reconcile",
            post(reconcile_extensions),
        )
        .route("/api/admin/keys", get(list_keys).post(create_key))
        .route("/api/admin/keys/{id}/revoke", post(revoke_key))
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
            "/api/admin/extensions/{name}/enable",
            post(enable_extension),
        )
        .route(
            "/api/admin/extensions/{name}/disable",
            post(disable_extension),
        )
}

async fn session(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match authenticate(&state, &headers) {
        Ok(principal) => Json(AdminSessionResponse { principal }).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn catalog(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match require_root(&state, &headers) {
        Ok(_) => Json(AdminCatalogResponse {
            items: build_catalog(
                state.index.admin_workers(),
                state.registry.admin_extensions(),
            ),
        })
        .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn list_workers(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match authenticate(&state, &headers).and_then(|principal| {
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

fn build_catalog(
    workers: Vec<AdminWorkerInfo>,
    extensions: Vec<AdminExtensionInfo>,
) -> Vec<AdminCatalogItem> {
    let mut items = Vec::new();
    for worker in workers {
        items.push(worker_catalog_item(&worker));
    }
    for extension in extensions {
        for menu in &extension.manifest.menus {
            items.push(module_catalog_item(&extension, &menu.name));
        }
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
        visibility: worker.visibility.clone(),
    }
}

fn module_catalog_item(extension: &AdminExtensionInfo, title: &str) -> AdminCatalogItem {
    AdminCatalogItem {
        id: format!("module:{}:{}", extension.name, catalog_slug(title)),
        kind: "moduleMenu".into(),
        owner: extension.name.clone(),
        owner_kind: extension.kind.clone(),
        route: format!("#module-{}", catalog_slug(title)),
        source: "extensionManifest".into(),
        status: extension.status.clone(),
        title: title.into(),
        visibility: "root".into(),
    }
}

fn catalog_slug(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_separator = false;
        } else if !last_was_separator && !slug.is_empty() {
            slug.push('-');
            last_was_separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "module".into()
    } else {
        slug
    }
}

async fn list_extensions(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match require_root(&state, &headers) {
        Ok(_) => Json(AdminExtensionsResponse {
            extensions: state.registry.admin_extensions(),
        })
        .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn reconcile_extensions(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Json(request): Json<AdminExtensionReconcileRequest>,
) -> Response {
    let request_id =
        request_id_from_headers(&headers).unwrap_or_else(|| Uuid::new_v4().to_string());
    match require_root(&state, &headers).and_then(|principal| {
        validate_admin_mutation_security("POST", &headers, &principal)?;
        state
            .registry
            .reconcile_extensions(request_id.clone(), &request)
    }) {
        Ok(response) => Json(response).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn list_keys(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match require_root(&state, &headers).and_then(|_| state.auth.list_api_keys()) {
        Ok(keys) => Json(AdminApiKeysResponse { keys }).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn create_key(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Json(mut request): Json<AdminCreateApiKeyRequest>,
) -> Response {
    match require_root(&state, &headers).and_then(|principal| {
        validate_admin_mutation_security("POST", &headers, &principal)?;
        normalize_create_key_request(&mut request)?;
        let raw_key = generate_api_key();
        let key = state.auth.create_api_key(&raw_key, &request)?;
        Ok((raw_key, key))
    }) {
        Ok((raw_key, key)) => (
            StatusCode::CREATED,
            Json(AdminCreateApiKeyResponse { key, raw_key }),
        )
            .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn revoke_key(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    match require_root(&state, &headers).and_then(|principal| {
        validate_admin_mutation_security("POST", &headers, &principal)?;
        state.auth.revoke_api_key(id)
    }) {
        Ok(revoked) => Json(AdminRevokeApiKeyResponse {
            id,
            revoked,
            status: if revoked { "revoked" } else { "not_found" }.into(),
        })
        .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn install_worker(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match authenticate(&state, &headers).and_then(|principal| {
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
    match authenticate(&state, &headers).and_then(|principal| {
        require_permission(&principal, "workers:install")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        rescan_workers(&state.index, dry_run)
    }) {
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
    match authenticate(&state, &headers).and_then(|principal| {
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
    match authenticate(&state, &headers).and_then(|principal| {
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
    worker_mutation(state, headers, name, query.version, true)
}

async fn disable_worker(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerVersionQuery>,
) -> Response {
    worker_mutation(state, headers, name, query.version, false)
}

async fn enable_extension(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    extension_mutation(state, headers, name, true)
}

async fn disable_extension(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    extension_mutation(state, headers, name, false)
}

fn worker_mutation(
    state: OrchestratorState,
    headers: HeaderMap,
    name: String,
    version: Option<String>,
    enabled: bool,
) -> Response {
    match require_root(&state, &headers).and_then(|principal| {
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

fn extension_mutation(
    state: OrchestratorState,
    headers: HeaderMap,
    name: String,
    enabled: bool,
) -> Response {
    match require_root(&state, &headers).and_then(|principal| {
        validate_admin_mutation_security("POST", &headers, &principal)?;
        state.registry.set_extension_enabled(&name, enabled)
    }) {
        Ok(extension) => Json(AdminMutationResponse {
            code: "OK".into(),
            message: format!("extension {} {}", extension.name, extension.status),
            status: extension.status,
        })
        .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

fn authenticate(
    state: &OrchestratorState,
    headers: &HeaderMap,
) -> Result<ApiKeyPrincipal, CoreError> {
    state
        .auth
        .authenticate_headers(headers)?
        .ok_or_else(|| CoreError::new("UNAUTHORIZED", "missing or invalid API key"))
}

fn require_root(
    state: &OrchestratorState,
    headers: &HeaderMap,
) -> Result<ApiKeyPrincipal, CoreError> {
    let principal = authenticate(state, headers)?;
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

fn normalize_create_key_request(request: &mut AdminCreateApiKeyRequest) -> Result<(), CoreError> {
    request.name = request.name.trim().to_string();
    request.role = request.role.trim().to_string();
    request.permissions = trim_non_empty("permissions", &request.permissions)?;
    request.namespaces = trim_non_empty("namespaces", &request.namespaces)?;

    if request.name.is_empty() {
        return Err(CoreError::new("BAD_REQUEST", "key name is required"));
    }
    if request.role.is_empty() {
        request.role = "viewer".into();
    }
    Ok(())
}

fn trim_non_empty(field: &str, values: &[String]) -> Result<Vec<String>, CoreError> {
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(CoreError::new(
                "BAD_REQUEST",
                format!("{field} cannot contain empty values"),
            ));
        }
        normalized.push(trimmed.to_string());
    }
    Ok(normalized)
}

fn generate_api_key() -> String {
    format!("edger_{}", Uuid::new_v4().simple())
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
