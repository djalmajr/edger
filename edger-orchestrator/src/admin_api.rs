//! Admin API routes for operational inventory and root-only controls.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use edger_core::{
    principal_has_permission, AdminApiKeysResponse, AdminCreateApiKeyRequest,
    AdminCreateApiKeyResponse, AdminErrorResponse, AdminExtensionReconcileRequest,
    AdminExtensionsResponse, AdminMutationResponse, AdminRevokeApiKeyResponse,
    AdminSessionResponse, AdminWorkersResponse, ApiKeyPrincipal, CoreError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::operational_log::log_operational_error;
use crate::pipeline::OrchestratorState;
use crate::security::validate_admin_mutation_security;
use crate::server::request_id_from_headers;

pub fn router() -> Router<OrchestratorState> {
    Router::new()
        .route("/api/admin/session", get(session))
        .route("/api/admin/workers", get(list_workers))
        .route("/api/admin/extensions", get(list_extensions))
        .route(
            "/api/admin/extensions/reconcile",
            post(reconcile_extensions),
        )
        .route("/api/admin/gateway/stats", get(gateway_stats))
        .route(
            "/api/admin/gateway/rate-limit/metrics",
            get(gateway_rate_limit_metrics),
        )
        .route("/api/admin/gateway/config", get(gateway_config))
        .route("/api/admin/gateway/logs/stats", get(gateway_logs_stats))
        .route("/api/admin/gateway/logs", get(gateway_logs))
        .route("/api/admin/keys", get(list_keys).post(create_key))
        .route("/api/admin/keys/{id}/revoke", post(revoke_key))
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

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GatewayLogsQuery {
    decision: Option<String>,
    limit: Option<usize>,
    rate_limited: Option<bool>,
    status: Option<u16>,
}

async fn session(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match authenticate(&state, &headers) {
        Ok(principal) => Json(AdminSessionResponse { principal }).into_response(),
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

async fn gateway_stats(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match require_root(&state, &headers).and_then(|_| gateway_diagnostics(&state)) {
        Ok(diagnostics) => Json(diagnostics).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn gateway_config(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match require_root(&state, &headers).and_then(|_| {
        gateway_diagnostics(&state).and_then(|diagnostics| {
            diagnostics
                .get("config")
                .cloned()
                .ok_or_else(|| CoreError::new("NOT_FOUND", "gateway config unavailable"))
        })
    }) {
        Ok(config) => Json(config).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn gateway_rate_limit_metrics(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
) -> Response {
    match require_root(&state, &headers)
        .and_then(|_| gateway_diagnostics(&state))
        .and_then(|diagnostics| gateway_rate_limit_metrics_response(&diagnostics))
    {
        Ok(metrics) => Json(metrics).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn gateway_logs(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Query(query): Query<GatewayLogsQuery>,
) -> Response {
    match require_root(&state, &headers)
        .and_then(|_| gateway_diagnostics(&state))
        .and_then(|diagnostics| gateway_logs_response(&diagnostics, &query))
    {
        Ok(logs) => Json(logs).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn gateway_logs_stats(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
) -> Response {
    match require_root(&state, &headers)
        .and_then(|_| gateway_diagnostics(&state))
        .and_then(|diagnostics| gateway_log_stats_response(&diagnostics))
    {
        Ok(stats) => Json(stats).into_response(),
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

async fn enable_worker(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    worker_mutation(state, headers, name, true)
}

async fn disable_worker(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    worker_mutation(state, headers, name, false)
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
    enabled: bool,
) -> Response {
    match require_root(&state, &headers).and_then(|principal| {
        validate_admin_mutation_security("POST", &headers, &principal)?;
        state.index.set_worker_enabled(&name, enabled)
    }) {
        Ok(worker) => Json(AdminMutationResponse {
            code: "OK".into(),
            message: format!("worker {} {}", worker.name, worker.status),
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

fn gateway_diagnostics(state: &OrchestratorState) -> Result<Value, CoreError> {
    state
        .registry
        .admin_extension("gateway")
        .and_then(|extension| extension.diagnostics)
        .ok_or_else(|| CoreError::new("NOT_FOUND", "gateway diagnostics unavailable"))
}

fn gateway_logs_response(
    diagnostics: &Value,
    query: &GatewayLogsQuery,
) -> Result<Value, CoreError> {
    let decisions = diagnostics
        .get("recentDecisions")
        .and_then(Value::as_array)
        .ok_or_else(|| CoreError::new("NOT_FOUND", "gateway logs unavailable"))?;
    let total = decisions.len();
    let limit = query.limit.unwrap_or(50).min(100);
    let logs = decisions
        .iter()
        .filter(|entry| gateway_log_matches(entry, query))
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let returned = logs.len();
    Ok(json!({
        "filters": {
            "decision": &query.decision,
            "limit": limit,
            "rateLimited": query.rate_limited,
            "status": query.status,
        },
        "logs": logs,
        "returned": returned,
        "total": total,
    }))
}

fn gateway_rate_limit_metrics_response(diagnostics: &Value) -> Result<Value, CoreError> {
    let rate_limit = diagnostics
        .get("rateLimit")
        .cloned()
        .ok_or_else(|| CoreError::new("NOT_FOUND", "gateway rate limit metrics unavailable"))?;
    let Value::Object(mut metrics) = rate_limit else {
        return Err(CoreError::new(
            "NOT_FOUND",
            "gateway rate limit metrics unavailable",
        ));
    };
    metrics.insert("scope".into(), Value::String("local-memory".into()));
    Ok(Value::Object(metrics))
}

fn gateway_log_stats_response(diagnostics: &Value) -> Result<Value, CoreError> {
    let decisions = diagnostics
        .get("recentDecisions")
        .and_then(Value::as_array)
        .ok_or_else(|| CoreError::new("NOT_FOUND", "gateway logs unavailable"))?;
    let mut by_decision = BTreeMap::<String, u64>::new();
    let mut by_status = BTreeMap::<String, u64>::new();
    let mut duration_samples = 0_u64;
    let mut duration_sum = 0_u64;
    let mut rate_limited = 0_u64;
    let mut without_status = 0_u64;

    for entry in decisions {
        if entry
            .get("rateLimited")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            rate_limited = rate_limited.saturating_add(1);
        }
        if let Some(decision) = entry.get("decision").and_then(Value::as_str) {
            *by_decision.entry(decision.to_string()).or_default() += 1;
        }
        if let Some(status) = entry.get("status").and_then(Value::as_u64) {
            *by_status.entry(status.to_string()).or_default() += 1;
        } else {
            without_status = without_status.saturating_add(1);
        }
        if let Some(duration_ms) = entry.get("durationMs").and_then(Value::as_u64) {
            duration_samples = duration_samples.saturating_add(1);
            duration_sum = duration_sum.saturating_add(duration_ms);
        }
    }
    let avg_ms = (duration_samples > 0).then_some(duration_sum / duration_samples);

    Ok(json!({
        "byDecision": by_decision,
        "byStatus": by_status,
        "duration": {
            "avgMs": avg_ms,
            "samples": duration_samples,
            "tracked": duration_samples > 0,
        },
        "rateLimited": rate_limited,
        "total": decisions.len(),
        "withoutStatus": without_status,
    }))
}

fn gateway_log_matches(entry: &Value, query: &GatewayLogsQuery) -> bool {
    if let Some(expected) = query.rate_limited {
        if entry.get("rateLimited").and_then(Value::as_bool) != Some(expected) {
            return false;
        }
    }
    if let Some(expected) = query.status {
        if entry.get("status").and_then(Value::as_u64) != Some(u64::from(expected)) {
            return false;
        }
    }
    if let Some(expected) = query.decision.as_deref() {
        if entry.get("decision").and_then(Value::as_str) != Some(expected) {
            return false;
        }
    }
    true
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
        "BAD_REQUEST" => StatusCode::BAD_REQUEST,
        "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
        "NOT_FOUND" => StatusCode::NOT_FOUND,
        "CSRF_DENIED" | "FORBIDDEN" => StatusCode::FORBIDDEN,
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
