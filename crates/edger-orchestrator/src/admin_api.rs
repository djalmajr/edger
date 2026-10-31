//! Admin API routes for operational inventory and root-only controls.

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, RETRY_AFTER,
};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, patch, post, put};
use axum::{Json, Router};
use edger_core::admin::UpdateApiKeyPermissionsRequest;
use edger_core::{
    principal_can_access_worker, principal_has_permission, require_same_origin, root_principal,
    validate_app_secret_name, AdminApiKeysResponse, AdminCatalogItem, AdminCatalogResponse,
    AdminErrorResponse, AdminSessionResponse, AdminWorkerInfo, AdminWorkersResponse,
    ApiKeyPrincipal, CoreError, CreateApiKeyRequest, SerializedRequest, WorkerManifest,
    WorkerOrigin, WorkerVisibility,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::io::{Read, Seek, Write};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use crate::deploy::{
    commit_install, delete_worker as delete_worker_deployment, extract_zip_entries,
    install_worker_from_zip, rollback_install, run_worker_release_with_events,
    MAX_DEPLOY_PACKAGE_BYTES,
};
use crate::manifest_loader::{
    clear_persisted_routing_policy, manifest_yaml_options, persist_routing_policy,
};
use crate::manifest_loader::{persist_default_version, persist_worker_enabled};
use crate::operational_log::log_operational_error;
use crate::pipeline::{OrchestratorState, ADMIN_CONTROL_AUTH_HEADER, ADMIN_WORKER_VERSION_HEADER};
use crate::routing_policy::parse_routing_policy;
use crate::security::validate_admin_mutation_security;
use crate::server::request_id_from_headers;
use crate::{
    auth::extract_api_key,
    console_auth::{
        ConsoleActor, ConsoleAuthError, ConsoleUserCreate, ConsoleUserInfo, ConsoleUserPatch,
        RevokeSessionOutcome, LOGIN_MAX_FAILURES, LOGIN_WINDOW, PASSWORD_MAX_LEN, PASSWORD_MIN_LEN,
        SESSION_PREFIX, SESSION_TTL,
    },
};

const MAX_ROUTING_POLICY_BYTES: usize = 16 * 1024;

/// Limite de corpo dos endpoints de console (login/troca de senha): JSON
/// pequeno e delimitado — payload maior não é decodificado.
const CONSOLE_BODY_LIMIT: usize = 4 * 1024;

pub fn router() -> Router<OrchestratorState> {
    Router::new()
        .route("/api/admin/session", get(session))
        .route("/api/admin/catalog", get(catalog))
        .route("/api/admin/workers", get(list_workers))
        .route(
            "/api/admin/routing-policy",
            get(get_routing_policy)
                .put(put_routing_policy)
                .delete(delete_routing_policy)
                .layer(DefaultBodyLimit::max(MAX_ROUTING_POLICY_BYTES)),
        )
        .route("/api/admin/workers/{name}", delete(delete_worker_route))
        .route(
            "/api/admin/workers/install",
            post(install_worker).layer(DefaultBodyLimit::max(MAX_DEPLOY_PACKAGE_BYTES)),
        )
        .route("/api/admin/workers/rescan", post(rescan_workers_route))
        .route(
            "/api/admin/workers/error-summary",
            get(worker_error_summary),
        )
        .route("/api/admin/workers/{name}/errors", get(worker_errors))
        .route("/api/admin/observability/events", get(observability_events))
        .route("/api/admin/observability/series", get(observability_series))
        .route("/api/admin/cron/jobs", get(list_cron_jobs))
        .route(
            "/api/admin/observability/events/stream",
            get(observability_events_stream),
        )
        .route("/api/admin/workers/{name}/enable", post(enable_worker))
        .route("/api/admin/workers/{name}/disable", post(disable_worker))
        .route("/api/admin/workers/{name}/promote", post(promote_worker))
        .route(
            "/api/admin/workers/{name}/recycle",
            post(recycle_worker_route),
        )
        .route("/api/admin/workers/{name}/invoke", any(invoke_worker_root))
        .route(
            "/api/admin/workers/{name}/invoke/{*path}",
            any(invoke_worker_path),
        )
        .route(
            "/api/admin/workers/{name}/health-check",
            post(run_health_check),
        )
        // (EDG-38) Cofre de segredos de app: a listagem NUNCA devolve o
        // valor (só nomes e datas); o valor é injetado no env do processo
        // do worker que declara o nome no manifest.
        .route("/api/admin/workers/{name}/secrets", get(list_app_secrets))
        .route(
            "/api/admin/workers/{name}/secrets/{secret}",
            put(write_app_secret).delete(delete_app_secret),
        )
        .route(
            "/api/admin/workers/{name}/files",
            get(worker_files)
                .post(upload_worker_files)
                .layer(DefaultBodyLimit::max(MAX_DEPLOY_PACKAGE_BYTES)),
        )
        .route(
            "/api/admin/workers/{name}/files/delete",
            post(delete_worker_files),
        )
        .route(
            "/api/admin/workers/{name}/files/download",
            get(download_worker_files),
        )
        .route("/api/admin/state/export", get(state_export_route))
        .route("/api/admin/keys", get(list_api_keys).post(create_api_key))
        .route("/api/admin/keys/{id}/revoke", post(revoke_api_key))
        .route(
            "/api/admin/keys/{id}",
            patch(update_api_key_permissions).delete(delete_api_key),
        )
        .route(
            "/api/admin/login",
            post(login_route).layer(DefaultBodyLimit::max(CONSOLE_BODY_LIMIT)),
        )
        .route("/api/admin/login-options", get(login_options))
        .route("/api/admin/logout", post(logout_route))
        .route(
            "/api/admin/me/password",
            post(me_password).layer(DefaultBodyLimit::max(CONSOLE_BODY_LIMIT)),
        )
        .route(
            "/api/admin/users",
            get(list_users_route)
                .post(create_user_route)
                .layer(DefaultBodyLimit::max(CONSOLE_BODY_LIMIT)),
        )
        .route(
            "/api/admin/users/{id}",
            patch(update_user_route)
                .delete(delete_user_route)
                .layer(DefaultBodyLimit::max(CONSOLE_BODY_LIMIT)),
        )
        .route(
            "/api/admin/users/{id}/reset-password",
            post(reset_user_password_route).layer(DefaultBodyLimit::max(CONSOLE_BODY_LIMIT)),
        )
        .route("/api/admin/sessions", get(list_sessions_route))
        .route("/api/admin/sessions/{id}", delete(revoke_session_route))
        .route("/api/admin/settings", get(settings_route))
}

#[derive(Deserialize)]
struct RoutingPolicyQuery {
    name: String,
}

async fn get_routing_policy(
    State(state): State<OrchestratorState>,
    Query(query): Query<RoutingPolicyQuery>,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "workers:read")?;
        let policy = state.index.routing_policy(&query.name)?;
        if policy.is_none() || !principal.is_root {
            require_visible_worker(&state, &principal, &query.name, None)?;
        }
        Ok::<_, CoreError>(policy)
    }
    .await;
    match result {
        Ok(policy) => Json(json!({ "policy": policy })).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn put_routing_policy(
    State(state): State<OrchestratorState>,
    Query(query): Query<RoutingPolicyQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = async {
        let principal = require_root(&state, &headers).await?;
        validate_admin_mutation_security("PUT", &headers, &principal)?;
        let policy = parse_routing_policy(&body)?;
        if policy.name != query.name {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                "routing policy name does not match the query name",
            ));
        }
        persist_routing_policy(&state.index, &policy)?;
        Ok::<_, CoreError>(policy)
    }
    .await;
    match result {
        Ok(policy) => Json(json!({ "policy": policy })).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn delete_routing_policy(
    State(state): State<OrchestratorState>,
    Query(query): Query<RoutingPolicyQuery>,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let principal = require_root(&state, &headers).await?;
        validate_admin_mutation_security("DELETE", &headers, &principal)?;
        clear_persisted_routing_policy(&state.index, &query.name)?;
        Ok::<_, CoreError>(())
    }
    .await;
    match result {
        Ok(()) => Json(json!({ "deleted": true })).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

/// O store de keys — 503 quando a instância subiu sem ele (open mode, ou
/// EDGER_API_KEYS_DB inutilizável): a gestão indisponível é um estado
/// operacional explícito, não um 404 mentiroso.
fn key_service(
    state: &OrchestratorState,
) -> Result<&std::sync::Arc<crate::api_keys::ApiKeyService>, CoreError> {
    state.auth.key_service().ok_or_else(|| {
        CoreError::new(
            "KEYS_STORE_UNAVAILABLE",
            "api key store is not configured on this instance",
        )
    })
}

async fn list_api_keys(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "keys:manage")?;
        key_service(&state)?.list()
    }
    .await;
    match result {
        Ok(keys) => Json(AdminApiKeysResponse { keys }).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn create_api_key(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "keys:manage")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        // Store antes do body: sem ele o endpoint é inutilizável qualquer
        // que seja o payload — 503 domina 400.
        let service = key_service(&state)?;
        let request: CreateApiKeyRequest = serde_json::from_slice(&body)
            .map_err(|err| CoreError::new("VALIDATION_ERROR", format!("invalid body: {err}")))?;
        // A anti-escalada mora no validate_key_grant do create: o principal
        // criador só concede subconjunto do que TEM.
        service.create(&principal, request)
    }
    .await;
    match result {
        Ok(created) => (StatusCode::CREATED, Json(created)).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn revoke_api_key(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "keys:manage")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        if !key_service(&state)?.revoke(id)? {
            return Err(CoreError::new(
                "NOT_FOUND",
                format!("api key {id} not found"),
            ));
        }
        Ok(json!({ "id": id, "revoked": true }))
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn update_api_key_permissions(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    body: Bytes,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "keys:manage")?;
        validate_admin_mutation_security("PATCH", &headers, &principal)?;
        let service = key_service(&state)?;
        let request: UpdateApiKeyPermissionsRequest = serde_json::from_slice(&body)
            .map_err(|err| CoreError::new("VALIDATION_ERROR", format!("invalid body: {err}")))?;
        service.update_permissions(&principal, id, request.permissions)
    }
    .await;
    match result {
        Ok(key) => Json(key).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn delete_api_key(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(id): Path<u64>,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "keys:manage")?;
        validate_admin_mutation_security("DELETE", &headers, &principal)?;
        if !key_service(&state)?.delete(id)? {
            return Err(CoreError::new(
                "NOT_FOUND",
                format!("api key {id} not found"),
            ));
        }
        Ok(json!({ "id": id, "deleted": true }))
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn session(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    let result = async {
        let principal = authenticate_unrestricted(&state, &headers).await?;
        Ok::<_, CoreError>(AdminSessionResponse {
            principal,
            must_change_password: state.auth.must_change_password(&headers)?,
            tenant_routing_enabled: state.server.tenant_routing_enabled(),
            weighted_routing_enabled: state.server.weighted_routing_enabled(),
        })
    }
    .await;
    match result {
        Ok(response) => Json(response).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

// --------------------------------------------------------------------------
// Console por senha (root user + sessões `ses-` persistentes). O login é
// PÚBLICO (é a porta de entrada da credencial); o logout e a troca de senha
// só aceitam credencial válida. Senha/token nunca entram em log ou em
// mensagem de erro.
// --------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangePasswordRequest {
    current: String,
    new: String,
}

/// Requisição com cara de browser (origin/sec-fetch-*) exige origem == host;
/// cliente sem esses headers (curl/API) não é alvo do CSRF check.
fn browser_origin_check(headers: &HeaderMap) -> Result<(), CoreError> {
    let browser_originated = headers.get("origin").is_some()
        || headers.get("sec-fetch-mode").is_some()
        || headers.get("sec-fetch-site").is_some();
    if !browser_originated {
        return Ok(());
    }
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    let host = headers.get("host").and_then(|value| value.to_str().ok());
    require_same_origin(origin, host)
}

fn console_unavailable_error() -> CoreError {
    CoreError::new(
        "CONSOLE_STORE_UNAVAILABLE",
        "console password auth is not configured on this instance",
    )
}

fn rate_limited_response(retry: Duration) -> Response {
    let retry_after = retry.as_secs().max(1).to_string();
    (
        StatusCode::TOO_MANY_REQUESTS,
        [
            (RETRY_AFTER, retry_after.as_str()),
            (CACHE_CONTROL, "no-store"),
        ],
        Json(AdminErrorResponse {
            code: "RATE_LIMITED".into(),
            message: "too many failed attempts".into(),
        }),
    )
        .into_response()
}

/// Limite de cálculos de senha simultâneos esgotado (sem slot no tempo do
/// timeout): o cálculo Argon2 não anda — negação honesta (503), nunca
/// autenticação.
fn console_busy_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(RETRY_AFTER, "1"), (CACHE_CONTROL, "no-store")],
        Json(AdminErrorResponse {
            code: "CONSOLE_BUSY".into(),
            message: "too many concurrent password calculations".into(),
        }),
    )
        .into_response()
}

/// Disponibilidade do login por senha, sem segredos: `passwordEnabled` =
/// root ativo no store; `rootSeeded` = linha de root no store (reflete o
/// banco, não apenas a presença do env de semente).
async fn login_options(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match state.auth.console_service() {
        Some(console) => match (console.password_enabled(), console.has_root_user()) {
            (Ok(password_enabled), Ok(root_seeded)) => Json(json!({
                "passwordEnabled": password_enabled,
                "rootSeeded": root_seeded,
            }))
            .into_response(),
            (Err(err), _) | (_, Err(err)) => {
                admin_error(StatusCode::SERVICE_UNAVAILABLE, &err, &headers)
            }
        },
        None => Json(json!({ "passwordEnabled": false, "rootSeeded": false })).into_response(),
    }
}

/// Login público: JSON limitado `{username,password}`, origem igual para
/// browser, 401 genérica em falha (não revela se `root` existe), 429 +
/// Retry-After no budget POR IP REAL (`ConnectInfo` do listener — `XFF`
/// forjado não rotaciona o bucket), 503 sem store. Sucesso:
/// `{"token":"ses-…"}` com `Cache-Control: no-store`. Sem `ConnectInfo`
/// (teste isolado) o extractor falha: 400, login não anda (falha fechado).
async fn login_route(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let console = match state.auth.console_service() {
        Some(console) => console,
        None => {
            return admin_error(
                StatusCode::SERVICE_UNAVAILABLE,
                &console_unavailable_error(),
                &headers,
            )
        }
    };
    if let Err(err) = browser_origin_check(&headers) {
        return admin_error(map_error_status(&err), &err, &headers);
    }
    let request: LoginRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(err) => {
            let error = CoreError::new("VALIDATION_ERROR", format!("invalid body: {err}"));
            return admin_error(map_error_status(&error), &error, &headers);
        }
    };
    let username = request.username.trim().to_string();
    let client_ip: IpAddr = peer.ip();
    match console
        .login_async(client_ip, &username, &request.password)
        .await
    {
        Ok(token) => {
            let pending = match console.session_must_change_password(&token) {
                Ok(pending) => pending,
                Err(err) => return admin_error(map_error_status(&err), &err, &headers),
            };
            tracing::info!(username, "console login issued a session");
            (
                StatusCode::OK,
                [(CACHE_CONTROL, "no-store")],
                Json(json!({ "token": token, "mustChangePassword": pending })),
            )
                .into_response()
        }
        Err(ConsoleAuthError::InvalidCredentials) => {
            // Mesma resposta para usuário inexistente, senha errada, usuário
            // inativo ou root ausente — o corpo não revela o motivo.
            tracing::warn!(username, "console login rejected");
            (
                StatusCode::UNAUTHORIZED,
                Json(AdminErrorResponse {
                    code: "UNAUTHORIZED".into(),
                    message: "invalid credentials".into(),
                }),
            )
                .into_response()
        }
        Err(ConsoleAuthError::RateLimited(retry)) => rate_limited_response(retry),
        Err(ConsoleAuthError::Busy) => console_busy_response(),
        Err(ConsoleAuthError::InvalidRequest(message)) => {
            let error = CoreError::new("VALIDATION_ERROR", message);
            admin_error(map_error_status(&error), &error, &headers)
        }
        Err(ConsoleAuthError::Store(err)) => {
            // Falha fechada: erro de store nunca autentica; 503 honesto.
            let error = CoreError::new("CONSOLE_STORE_UNAVAILABLE", err.message.clone());
            admin_error(StatusCode::SERVICE_UNAVAILABLE, &error, &headers)
        }
    }
}

/// Logout: revoga SOMENTE token de sessão (`ses-`). Root key/`egk_`/OIDC
/// passam autenticados e recebem 204 sem revogação (o logout é local para
/// eles); em open mode o principal sintético root também recebe 204.
async fn logout_route(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    let principal = match authenticate_unrestricted(&state, &headers).await {
        Ok(principal) => principal,
        Err(err) => return admin_error(map_error_status(&err), &err, &headers),
    };
    let Some(credential) =
        extract_api_key(&headers).filter(|credential| credential.starts_with(SESSION_PREFIX))
    else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let Some(console) = state.auth.console_service() else {
        // Open mode accepts the synthetic root principal, including when a
        // caller sends a session-shaped credential. There is no session store
        // to revoke, so logout remains an idempotent no-op.
        return StatusCode::NO_CONTENT.into_response();
    };
    match console.logout(&credential) {
        Ok(_) => {
            tracing::info!(username = %principal.name, "console session revoked (logout)");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            // Revogação falhou: a sessão segue viva — 503 para o cliente
            // não descartar o token em falso.
            let error = CoreError::new("CONSOLE_STORE_UNAVAILABLE", err.message.clone());
            admin_error(StatusCode::SERVICE_UNAVAILABLE, &error, &headers)
        }
    }
}

/// Troca a senha do dono da sessão A PARTIR DA SESSÃO (não da root key,
/// `egk_` ou OIDC) — funciona para qualquer usuário de sessão, sem depender
/// de nome root. Valida a senha atual e a nova (política forte), rotaciona
/// TODAS as sessões do usuário (as antigas — a corrente inclusive — morrem)
/// e devolve o NOVO token: `{"token":"ses-…"}` com `Cache-Control: no-store`.
/// A prova e o hash da nova senha rodam na pool de blocking sob o limite
/// compartilhado de slots de hash (`spawn_blocking` + `Busy` => 503);
/// a transação revalida a sessão solicitante no commit; a troca usa o mesmo
/// budget por IP real do login.
async fn me_password(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let console = match state.auth.console_service() {
        Some(console) => console,
        None => {
            return admin_error(
                StatusCode::SERVICE_UNAVAILABLE,
                &console_unavailable_error(),
                &headers,
            )
        }
    };
    if let Err(err) = browser_origin_check(&headers) {
        return admin_error(map_error_status(&err), &err, &headers);
    }
    // Só sessão muda a senha: credencial não-`ses-` (root key/`egk_`/OIDC)
    // não é aceita mesmo autenticando.
    let Some(credential) = extract_api_key(&headers) else {
        let error = CoreError::new("UNAUTHORIZED", "missing or invalid API key");
        return admin_error(map_error_status(&error), &error, &headers);
    };
    if !credential.starts_with(SESSION_PREFIX) {
        let error = CoreError::new("FORBIDDEN", "password change requires a console session");
        return admin_error(map_error_status(&error), &error, &headers);
    }
    let principal = match console.authenticate_session(&credential) {
        Some(principal) => principal,
        None => {
            let error = CoreError::new("UNAUTHORIZED", "missing or invalid API key");
            return admin_error(map_error_status(&error), &error, &headers);
        }
    };
    let request: ChangePasswordRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(err) => {
            let error = CoreError::new("VALIDATION_ERROR", format!("invalid body: {err}"));
            return admin_error(map_error_status(&error), &error, &headers);
        }
    };
    match console
        .change_password_async(peer.ip(), &credential, &request.current, &request.new)
        .await
    {
        Ok(token) => {
            tracing::info!(username = %principal.name, "console password changed; sessions rotated");
            (
                StatusCode::OK,
                [(CACHE_CONTROL, "no-store")],
                Json(json!({ "token": token })),
            )
                .into_response()
        }
        Err(ConsoleAuthError::InvalidCredentials) => {
            let error = CoreError::new("UNAUTHORIZED", "invalid credentials");
            admin_error(map_error_status(&error), &error, &headers)
        }
        Err(ConsoleAuthError::RateLimited(retry)) => rate_limited_response(retry),
        Err(ConsoleAuthError::Busy) => console_busy_response(),
        Err(ConsoleAuthError::InvalidRequest(message)) => {
            let error = CoreError::new("VALIDATION_ERROR", message);
            admin_error(map_error_status(&error), &error, &headers)
        }
        Err(ConsoleAuthError::Store(err)) => {
            let error = CoreError::new("CONSOLE_STORE_UNAVAILABLE", err.message.clone());
            admin_error(StatusCode::SERVICE_UNAVAILABLE, &error, &headers)
        }
    }
}

// --------------------------------------------------------------------------
// Gestão de usuários da console por root ou sessão admin com cobertura.
// `egk_` com papel admin não recebe autoridade de gestão.
// Metadados SEM hash/senha; mutações com `validate_admin_mutation_security`,
// corpo limitado e revogação de sessões na mesma transação (desativação,
// redução de permissões/escopos, reset e exclusão).
// --------------------------------------------------------------------------

/// Store de console quando a instância tem a feature (senão 503 honesto —
/// gestão indisponível é estado operacional, não 404 mentiroso).
fn console_store(
    state: &OrchestratorState,
) -> Result<&std::sync::Arc<crate::console_auth::ConsoleAuthService>, CoreError> {
    state
        .auth
        .console_service()
        .ok_or_else(console_unavailable_error)
}

/// Registro da resposta: o MESMO shape que o cPanel parseia inteiro
/// (`parseAdminUser` de `workers/core/cpanel/src/lib/api.ts`) — `id`,
/// `username`, `role`, `isRoot`, `disabled`, `createdAt` (epoch segundos),
/// arrays `permissions`/`namespaces`/`workers`. Nunca senha/hash.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminConsoleUser {
    id: i64,
    username: String,
    role: String,
    is_root: bool,
    disabled: bool,
    must_change_password: bool,
    manageable: bool,
    permissions: Vec<String>,
    namespaces: Vec<String>,
    workers: Vec<String>,
    created_at: i64,
    updated_at: i64,
}

fn admin_console_user(user: &ConsoleUserInfo) -> AdminConsoleUser {
    AdminConsoleUser {
        id: user.id,
        username: user.username.clone(),
        role: user.role.clone(),
        is_root: user.is_root,
        disabled: !user.active,
        must_change_password: user.must_change_password,
        manageable: user.manageable,
        permissions: user.permissions.clone(),
        namespaces: user.namespaces.clone(),
        workers: user.workers.clone(),
        created_at: user.created_at,
        updated_at: user.updated_at,
    }
}

/// `GET /api/admin/users` — root ou console admin; lista metadados (sem hash/senha/
/// sessões). A linha do root entra com `isRoot=true` (a UI a marca como
/// imutável; as mutações a rejeitam com 403).
async fn list_users_route(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    let result = async {
        let (_, actor) = require_user_manager(&state, &headers).await?;
        let users = console_store(&state)?.list_users(&actor)?;
        Ok::<_, CoreError>(
            users
                .into_iter()
                .map(|user| admin_console_user(&user))
                .collect::<Vec<_>>(),
        )
    }
    .await;
    match result {
        Ok(users) => Json(json!({ "users": users })).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

/// `POST /api/admin/users` — root ou console admin; cria com username normalizado,
/// senha forte e permissões/escopos validados pelo catálogo. 409 no
/// username duplicado; NUNCA devolve a senha.
async fn create_user_route(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let result = async {
        let (principal, actor) = require_user_manager(&state, &headers).await?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        let service = console_store(&state)?;
        let request: ConsoleUserCreate = serde_json::from_slice(&body)
            .map_err(|err| CoreError::new("VALIDATION_ERROR", format!("invalid body: {err}")))?;
        service.create_user(&actor, &request).await
    }
    .await;
    match result {
        Ok(user) => (
            StatusCode::CREATED,
            Json(json!({ "user": admin_console_user(&user) })),
        )
            .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateUserRequest {
    role: Option<String>,
    disabled: Option<bool>,
    permissions: Option<Vec<String>>,
    namespaces: Option<Vec<String>>,
    workers: Option<Vec<String>>,
}

/// `PATCH /api/admin/users/{id}` — root ou console admin; altera `disabled`,
/// permissões e escopos de usuário NÃO-ROOT (root => 403). Desativação ou
/// redução de permissões/escopos revoga as sessões na mesma transação.
async fn update_user_route(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    body: Bytes,
) -> Response {
    let result = async {
        let (principal, actor) = require_user_manager(&state, &headers).await?;
        validate_admin_mutation_security("PATCH", &headers, &principal)?;
        let service = console_store(&state)?;
        let raw: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|err| CoreError::new("VALIDATION_ERROR", format!("invalid body: {err}")))?;
        if !principal.is_root && raw.get("role").is_some() {
            return Err(CoreError::new("FORBIDDEN", "forbidden"));
        }
        let request: UpdateUserRequest = serde_json::from_value(raw)
            .map_err(|err| CoreError::new("VALIDATION_ERROR", format!("invalid body: {err}")))?;
        service.update_user(
            &actor,
            id,
            &ConsoleUserPatch {
                role: request.role,
                disabled: request.disabled,
                permissions: request.permissions,
                namespaces: request.namespaces,
                workers: request.workers,
            },
        )
    }
    .await;
    match result {
        Ok(user) => Json(json!({ "user": admin_console_user(&user) })).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetUserPasswordRequest {
    password: String,
}

/// `POST /api/admin/users/{id}/reset-password` — root ou console admin; define nova
/// senha forte e revoga TODAS as sessões do alvo na mesma transação.
async fn reset_user_password_route(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    body: Bytes,
) -> Response {
    let result = async {
        let (principal, actor) = require_user_manager(&state, &headers).await?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        let service = console_store(&state)?;
        let request: ResetUserPasswordRequest = serde_json::from_slice(&body)
            .map_err(|err| CoreError::new("VALIDATION_ERROR", format!("invalid body: {err}")))?;
        service
            .reset_user_password(&actor, id, &request.password)
            .await
    }
    .await;
    match result {
        Ok(user) => Json(json!({ "user": admin_console_user(&user) })).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

/// `DELETE /api/admin/users/{id}` — root ou console admin; remove usuário não-root e
/// sessões na mesma transação.
async fn delete_user_route(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let result = async {
        let (principal, actor) = require_user_manager(&state, &headers).await?;
        validate_admin_mutation_security("DELETE", &headers, &principal)?;
        console_store(&state)?.delete_user(&actor, id)?;
        Ok::<_, CoreError>(json!({ "id": id, "deleted": true }))
    }
    .await;
    match result {
        Ok(payload) => Json(payload).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

// --------------------------------------------------------------------------
// Sessões do console (root/admin) e configurações (somente root). Sessões:
// metadados SEM token/hash, `current` pela credencial do chamador; a
// revogação por id é a única mutação (self-revoke negado — logout).
// Configurações: allowlist estrita — sem URL/caminho/senha/chave/env raw e
// sem mutação (nenhum PATCH nesta fatia).
// --------------------------------------------------------------------------

/// `GET /api/admin/sessions` — root ou console admin; sessões vivas (usuário ativo e
/// não expiradas) sem segredos, `createdAt desc/id`, limite 1000 com
/// `truncated`. `current` marca a sessão da credencial quando ela é `ses-…`
/// (root token estático não tem sessão corrente). `no-store`; sem console é
/// 503 honesto.
async fn list_sessions_route(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let (_, actor) = require_user_manager(&state, &headers).await?;
        let service = console_store(&state)?;
        service.list_sessions(&actor)
    }
    .await;
    match result {
        Ok((sessions, truncated)) => (
            [(CACHE_CONTROL, "no-store")],
            Json(json!({ "sessions": sessions, "truncated": truncated })),
        )
            .into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

/// Id de sessão no formato existente (Uuid simple): 32 hex ASCII
/// MINÚSCULOS — id fora disso (hostil, maiúsculo, curto, injeção) nunca
/// chega ao SQL (o statement é parametrizado de qualquer forma).
fn valid_session_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `DELETE /api/admin/sessions/{id}` — root ou console admin + validação de mutação;
/// id estrito (400 `VALIDATION_ERROR` se hostil), SQL parametrizado. Sessão
/// alheia viva => 200 `{ok:true}` e a autenticação daquele token morre na
/// hora; inexistente => 404; a PRÓPRIA sessão do chamador => 403 (sai pelo
/// logout) — a proteção contra self-revoke vive no predicado SQL atômico
/// por `token_hash`, não num pré-check.
async fn revoke_session_route(
    State(state): State<OrchestratorState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let (principal, actor) = require_user_manager(&state, &headers).await?;
        validate_admin_mutation_security("DELETE", &headers, &principal)?;
        if !valid_session_id(&id) {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                "session id must be 32 lowercase hex characters",
            ));
        }
        let service = console_store(&state)?;
        service.revoke_session_by_id(&actor, &id)
    }
    .await;
    match result {
        Ok(RevokeSessionOutcome::Revoked) => Json(json!({ "ok": true })).into_response(),
        Ok(RevokeSessionOutcome::NotFound) => {
            let error = CoreError::new("NOT_FOUND", "session not found");
            admin_error(map_error_status(&error), &error, &headers)
        }
        Ok(RevokeSessionOutcome::OwnSession) => {
            let error = CoreError::new(
                "FORBIDDEN",
                "cannot revoke your own session; use the logout endpoint",
            );
            admin_error(map_error_status(&error), &error, &headers)
        }
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

/// `GET /api/admin/settings` — root apenas; allowlist estrita da
/// configuração ATUAL (somente leitura): versão do runtime, flags de
/// roteamento, disponibilidade do login por senha (reflete `login-options`:
/// `false` sem console; falha de store vira 503) e as políticas FIXAS já
/// implementadas (TTL de sessão, política de senha, janela/limite de
/// login). Nada de URL, caminho, senha, chave, env raw ou configuração
/// mutável; `no-store`.
async fn settings_route(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    if let Err(err) = require_root(&state, &headers).await {
        return admin_error(map_error_status(&err), &err, &headers);
    }
    let password_enabled = match state.auth.console_service() {
        Some(console) => match console.password_enabled() {
            Ok(enabled) => enabled,
            Err(err) => return admin_error(StatusCode::SERVICE_UNAVAILABLE, &err, &headers),
        },
        None => false,
    };
    (
        [(CACHE_CONTROL, "no-store")],
        Json(json!({
            "runtimeVersion": env!("CARGO_PKG_VERSION"),
            "tenantRoutingEnabled": state.server.tenant_routing_enabled(),
            "weightedRoutingEnabled": state.server.weighted_routing_enabled(),
            "passwordEnabled": password_enabled,
            "sessionTtlSeconds": SESSION_TTL.as_secs(),
            "passwordMinLength": PASSWORD_MIN_LEN,
            "passwordMaxLength": PASSWORD_MAX_LEN,
            "loginWindowSeconds": LOGIN_WINDOW.as_secs(),
            "loginMaxFailures": LOGIN_MAX_FAILURES,
        })),
    )
        .into_response()
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

#[derive(Debug, Default, Deserialize)]
struct InstallWorkerQuery {
    #[serde(default)]
    force: bool,
    #[serde(default)]
    staged: bool,
}

/// (EDG-12) Queue the background `minProcesses` prewarm for a version that
/// install/promote/enable just left active. The API response never waits for
/// the prewarm; its outcome lands in the operational events in the same
/// envelope as the rescan prewarm, with the worker, version and how many
/// instances were created. A failed attempt records a `warn` event and does
/// not retry (EDG-10 floor replenishment semantics). Returns the `prewarm`
/// response field: `"scheduled"` when the prewarm was queued,
/// `"not_configured"` when the version is inactive, has `minProcesses == 0`,
/// or uses a backend without a process.
fn schedule_min_process_prewarm(
    state: &OrchestratorState,
    name: &str,
    version: &str,
) -> &'static str {
    let Some(worker) = state
        .index
        .worker_refs()
        .into_iter()
        .find(|worker| worker.name == name && worker.version == version)
    else {
        return "not_configured";
    };
    if !worker.config.enabled
        || worker.config.min_processes == 0
        || !worker.kind.uses_process_backend()
    {
        return "not_configured";
    }
    let pool = state.pool.clone();
    let events = state.server.operational_events();
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        match pool.prewarm_worker(&worker).await {
            Ok(created) => {
                events.record(prewarm_event_input(
                    &worker,
                    crate::observability::OperationalEventLevel::Info,
                    "completed",
                    format!("prewarmed {created} process(es) for the min-processes floor"),
                    Some(started.elapsed().as_millis().max(1) as u64),
                ));
            }
            Err(error) => {
                events.record(prewarm_event_input(
                    &worker,
                    crate::observability::OperationalEventLevel::Warn,
                    "failed",
                    format!("min-processes prewarm failed: {error}"),
                    Some(started.elapsed().as_millis().max(1) as u64),
                ));
            }
        }
    });
    "scheduled"
}

/// Operational event for a post-activation `minProcesses` prewarm (EDG-12),
/// in the same envelope as the rescan prewarm/release events: source, kind,
/// worker, version, outcome and duration; the created count rides in the
/// message.
fn prewarm_event_input(
    worker: &edger_core::WorkerRef,
    level: crate::observability::OperationalEventLevel,
    outcome: &str,
    message: String,
    duration_ms: Option<u64>,
) -> crate::observability::OperationalEventInput {
    crate::observability::OperationalEventInput {
        source: crate::observability::OperationalEventSource::Orchestrator,
        kind: "worker.prewarm".into(),
        level,
        namespace: worker.namespace.clone(),
        worker: Some(worker.name.clone()),
        version: Some(worker.version.clone()),
        process_id: None,
        request_id: None,
        trace_id: None,
        outcome: Some(outcome.into()),
        status: None,
        duration_ms,
        timeout_ms: None,
        code: None,
        message: Some(message),
        truncated: None,
        dropped_count: None,
        method: None,
        path: None,
        content_type: None,
    }
}

async fn install_worker(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Query(query): Query<InstallWorkerQuery>,
    body: Bytes,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "workers:install")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        let package_name_hint = headers
            .get("x-edger-package-name")
            .and_then(|value| value.to_str().ok());
        // CAS do canal draft: o autosave manda a revisão da qual partiu.
        let expected_revision = headers
            .get("x-edger-expected-revision")
            .and_then(|value| value.to_str().ok());
        let mut transaction = install_worker_from_zip(
            &state.index,
            &principal,
            &body,
            package_name_hint,
            query.force,
            query.staged,
            expected_revision,
        )?;
        let replaced_existing = transaction.replaced_existing();
        let candidate = state
            .index
            .worker_refs()
            .into_iter()
            .find(|worker| {
                worker.name == transaction.installed.name
                    && worker.version == transaction.installed.version
            })
            .ok_or_else(|| CoreError::new("DEPLOY_INTERNAL", "installed worker was not indexed"))?;
        if let Err(error) =
            run_worker_release_with_events(&candidate, &state.server.operational_events()).await
        {
            rollback_failed_install(&state, &transaction).await?;
            return Err(error);
        }
        transaction.installed.release = if candidate.config.release_command.is_some() {
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
                &transaction.installed.name,
                &transaction.installed.version,
                "on-deploy",
            )
            .await
            {
                Ok(check) => check,
                Err(error) => {
                    rollback_failed_install(&state, &transaction).await?;
                    return Err(error);
                }
            };
            if !check.healthy {
                rollback_failed_install(&state, &transaction).await?;
                return Err(CoreError::new(
                    "DEPLOY_HEALTH_CHECK_FAILED",
                    format!(
                        "health check failed for {}@{}: {}",
                        transaction.installed.name, transaction.installed.version, check.message
                    ),
                ));
            }
            transaction.installed.health = "passed".into();
        } else {
            transaction.installed.health = "not_configured".into();
        }
        if state.index.cpanel_version_is_older_than_active(
            &transaction.installed.name,
            &transaction.installed.version,
        ) {
            // D19: um cPanel mais antigo que o ativo não é reativado no
            // install — fica inativo até uma promoção explícita.
            transaction.installed.activation = "inactive".into();
        } else {
            // EDG-21: the activation is persisted (marker inside the version
            // dir) before the in-memory flag moves; the slot the install
            // transaction already holds is passed so it is not re-claimed.
            if let Err(error) = persist_worker_enabled(
                &state.index,
                &transaction.installed.name,
                Some(&transaction.installed.version),
                true,
                Some(transaction.mutation_slot()),
            ) {
                rollback_failed_install(&state, &transaction).await?;
                return Err(error);
            }
            transaction.installed.activation = "active".into();
        }
        if replaced_existing {
            state
                .pool
                .recycle_worker(
                    &transaction.installed.name,
                    Some(&transaction.installed.version),
                )
                .await;
        }
        transaction.installed.default_version = state
            .index
            .resolve_public_worker(&transaction.installed.name, None)
            .map(|worker| worker.version)
            .unwrap_or_default();
        // (EDG-12) prewarm the min-processes floor in the background after
        // the version is active and the replaced old processes are recycled
        // — the response never waits for it.
        transaction.installed.prewarm = schedule_min_process_prewarm(
            &state,
            &transaction.installed.name,
            &transaction.installed.version,
        )
        .into();
        let installed = commit_install(transaction)?;
        Ok((installed, replaced_existing))
    }
    .await;
    match result {
        Ok((installed, true)) => (StatusCode::OK, Json(installed)).into_response(),
        Ok((installed, false)) => (StatusCode::CREATED, Json(installed)).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn rollback_failed_install(
    state: &OrchestratorState,
    transaction: &crate::deploy::InstallTransaction,
) -> Result<(), CoreError> {
    rollback_install(&state.index, transaction)?;
    state
        .pool
        .recycle_worker(
            &transaction.installed.name,
            Some(&transaction.installed.version),
        )
        .await;
    Ok(())
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

async fn delete_worker_route(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerVersionQuery>,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "workers:delete")?;
        validate_admin_mutation_security("DELETE", &headers, &principal)?;
        require_visible_worker(&state, &principal, &name, query.version.as_deref())?;
        let deleted = delete_worker_deployment(&state.index, &name, query.version.as_deref())?;
        let recycled = state
            .pool
            .recycle_worker(&name, query.version.as_deref())
            .await;
        Ok(json!({
            "name": deleted.name,
            "versions": deleted.versions,
            "recycledProcesses": recycled,
            "status": "deleted",
        }))
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn promote_worker(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerVersionQuery>,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "workers:promote")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        require_visible_worker(&state, &principal, &name, None)?;
        let eligible = public_versions(&state, &name)?;
        let version = query.version.ok_or_else(|| {
            CoreError::new(
                "PROMOTE_VERSION_REQUIRED",
                format!(
                    "version query parameter is required; eligible public versions: {}",
                    format_versions(&eligible)
                ),
            )
        })?;
        let worker = persist_default_version(&state.index, &name, &version)?;
        // (EDG-12) prewarm the promoted version's min-processes floor in the
        // background; the response never waits for it.
        let prewarm = schedule_min_process_prewarm(&state, &worker.name, &worker.version);
        Ok(json!({
            "name": worker.name,
            "version": worker.version,
            "defaultVersion": worker.version,
            "eligibleVersions": eligible,
            "status": "promoted",
            "prewarm": prewarm,
        }))
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

/// (EDG-38) Cofre de segredos de app: o valor NUNCA sai por estas rotas —
/// a listagem traz só nomes e datas, e a escrita/leitura do valor acontece
/// no spawn do processo do worker que declara o nome no manifest. Sem a
/// chave mestra (`EDGER_SECRETS_KEY_FILE`) o cofre é indisponível: 503
/// `SECRETS_UNAVAILABLE` (não é 404: o estado é da instância, não do
/// segredo).
fn app_secrets_vault(
    state: &OrchestratorState,
) -> Result<std::sync::Arc<crate::app_secrets::AppSecretVault>, CoreError> {
    state.server.app_secrets().cloned().ok_or_else(|| {
        CoreError::new(
            "SECRETS_UNAVAILABLE",
            "the app secrets vault is not configured on this instance (EDGER_SECRETS_KEY_FILE)",
        )
    })
}

/// (EDG-38 emenda 1 / F1) O escopo da chave vale para o `{name}` da rota
/// com o MESMO predicado único das outras rotas por worker — namespace
/// (derivado de `@scope/name`) E escopo de `workers` da chave, via
/// `ManifestIndex::principal_can_access_worker_name`. O worker NÃO precisa
/// estar instalado: o app grava o segredo antes do install que o declara.
/// Fora do escopo: 403 (a rota não vira 404 mentiroso sobre workers que
/// existem mas não são do chamador).
fn require_secret_worker_scope(principal: &ApiKeyPrincipal, name: &str) -> Result<(), CoreError> {
    if principal.is_root
        || crate::manifest_index_stub::ManifestIndex::principal_can_access_worker_name(
            principal, name,
        )
    {
        Ok(())
    } else {
        Err(CoreError::new(
            "FORBIDDEN",
            format!("worker scope does not cover: {name}"),
        ))
    }
}

/// (EDG-38 emenda 1 / F5, emenda 2 / R2) O `{name}` das rotas de segredo
/// passa pela regra de nome de worker do manifest — `name` simples ou
/// `@scope/name` — sem caracteres de controle, no máximo uma `/` (e só no
/// formato `@scope/name`), sem segmento `.`/`..` e sem espaço no começo ou
/// no fim de cada parte. Qualquer outra forma: 400.
fn validate_secret_route_worker_name(name: &str) -> Result<(), CoreError> {
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            "worker name must not be empty or contain control characters",
        ));
    }
    // (R2) cada parte separada por `/`: sem espaço nas bordas e nem `.`
    // nem `..` — um nome de worker não é um caminho.
    for part in name.split('/') {
        if part != part.trim() {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                "worker name parts must not have leading or trailing spaces",
            ));
        }
        if part == "." || part == ".." {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                "worker name parts must not be `.` or `..`",
            ));
        }
    }
    // (R2) no máximo UM `/`, e só no formato `@scope/name` (scope não vazio
    // a partir do `@`, nome local não vazio e sem `/` — `@acme/a/b` não é
    // um nome de worker válido).
    if let Some((scope, local)) = name.split_once('/') {
        if !scope.starts_with('@')
            || scope.trim_start_matches('@').is_empty()
            || local.is_empty()
            || local.contains('/')
        {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                "worker name must be `name` or `@scope/name`",
            ));
        }
    }
    Ok(())
}

/// Evento operacional do cofre (`secret.written` / `secret.deleted`): tem
/// worker, o NOME do segredo e o id da chave que agiu — NUNCA o valor.
fn secret_operational_event(
    kind: &'static str,
    request_id: Option<String>,
    principal: &ApiKeyPrincipal,
    worker: &str,
    secret: &str,
) -> crate::observability::OperationalEventInput {
    crate::observability::OperationalEventInput {
        source: crate::observability::OperationalEventSource::Orchestrator,
        kind: kind.into(),
        level: crate::observability::OperationalEventLevel::Info,
        namespace: None,
        worker: Some(worker.to_string()),
        version: None,
        process_id: None,
        request_id,
        trace_id: None,
        outcome: Some("ok".into()),
        status: None,
        duration_ms: None,
        timeout_ms: None,
        code: None,
        message: Some(format!("{secret} (actor api key id {})", principal.id)),
        truncated: None,
        dropped_count: None,
        method: None,
        path: None,
        content_type: None,
    }
}

#[derive(Deserialize)]
struct WriteAppSecretPayload {
    value: String,
}

// (F3) o valor de um segredo nunca pode vazar por um log/erro que imprima
// o payload: a implementação de `Debug` redige.
impl std::fmt::Debug for WriteAppSecretPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteAppSecretPayload")
            .field("value", &"<redacted>")
            .finish()
    }
}

async fn write_app_secret(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path((name, secret)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    let result = async {
        // (F3) autenticar e autorizar ANTES de ler o corpo: um chamador sem
        // credencial recebe 401 antes de qualquer parse.
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "secrets:write")?;
        validate_admin_mutation_security("PUT", &headers, &principal)?;
        validate_secret_route_worker_name(&name)?;
        require_secret_worker_scope(&principal, &name)?;
        let vault = app_secrets_vault(&state)?;
        // (F3) parse manual com mensagem fixa que NUNCA ecoa o corpo:
        // body malformado, campo ausente ou tipo errado dão o mesmo 400.
        let payload: WriteAppSecretPayload = serde_json::from_slice(&body).map_err(|_| {
            CoreError::new("VALIDATION_ERROR", "body must be {\"value\": \"<string>\"}")
        })?;
        validate_app_secret_name(&secret)?;
        if payload.value.is_empty() {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                "secret value must not be empty",
            ));
        }
        if payload.value.len() > crate::app_secrets::MAX_SECRET_VALUE_BYTES {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                format!(
                    "secret value must have at most {} bytes (16 KiB)",
                    crate::app_secrets::MAX_SECRET_VALUE_BYTES
                ),
            ));
        }
        vault.set(&name, &secret, &payload.value)?;
        let request_id = request_id_from_headers(&headers);
        state
            .server
            .operational_events()
            .record(secret_operational_event(
                "secret.written",
                request_id,
                &principal,
                &name,
                &secret,
            ));
        // Rotação: o novo valor vale no PRÓXIMO spawn. Para aplicar no
        // processo rodando agora, o operador usa `POST
        // /api/admin/workers/{name}/recycle?version=<v>` (EDG-16) — a
        // escrita NUNCA recicla sozinha.
        Ok(json!({ "worker": name, "secret": secret, "status": "written" }))
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn list_app_secrets(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "secrets:read")?;
        validate_secret_route_worker_name(&name)?;
        require_secret_worker_scope(&principal, &name)?;
        let vault = app_secrets_vault(&state)?;
        Ok::<_, CoreError>(json!(vault.list(&name)?))
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn delete_app_secret(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path((name, secret)): Path<(String, String)>,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "secrets:write")?;
        validate_admin_mutation_security("DELETE", &headers, &principal)?;
        validate_secret_route_worker_name(&name)?;
        require_secret_worker_scope(&principal, &name)?;
        let vault = app_secrets_vault(&state)?;
        validate_app_secret_name(&secret)?;
        if !vault.delete(&name, &secret)? {
            return Err(CoreError::new(
                "NOT_FOUND",
                format!("secret not found: {secret}"),
            ));
        }
        let request_id = request_id_from_headers(&headers);
        state
            .server
            .operational_events()
            .record(secret_operational_event(
                "secret.deleted",
                request_id,
                &principal,
                &name,
                &secret,
            ));
        Ok(json!({ "worker": name, "secret": secret, "status": "deleted" }))
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn invoke_worker_root(
    State(state): State<OrchestratorState>,
    Path(name): Path<String>,
    req: Request<axum::body::Body>,
) -> Response {
    invoke_worker_route(state, name, "/".into(), req).await
}
async fn invoke_worker_path(
    State(state): State<OrchestratorState>,
    Path((name, path)): Path<(String, String)>,
    req: Request<axum::body::Body>,
) -> Response {
    let path = format!("/{}", path.trim_start_matches('/'));
    invoke_worker_route(state, name, path, req).await
}

async fn invoke_worker_route(
    state: OrchestratorState,
    name: String,
    rewritten_path: String,
    req: Request<axum::body::Body>,
) -> Response {
    let headers = req.headers().clone();
    let request_id =
        request_id_from_headers(&headers).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let result = async {
        let principal = authenticate_invoke(&state, &headers).await?;
        require_permission(&principal, "workers:invoke")?;
        validate_admin_mutation_security(req.method().as_str(), &headers, &principal)?;
        let version = invocation_worker_version(&headers)?;
        require_visible_worker(&state, &principal, &name, version.as_deref())?;
        crate::pipeline::invoke_worker(
            &state,
            req,
            request_id,
            &name,
            version.as_deref(),
            rewritten_path,
            principal,
        )
        .await
    }
    .await;
    match result {
        Ok(response) => response,
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

fn invocation_worker_version(headers: &HeaderMap) -> Result<Option<String>, CoreError> {
    let Some(value) = headers.get(ADMIN_WORKER_VERSION_HEADER) else {
        return Ok(None);
    };
    let version = value
        .to_str()
        .map_err(|_| CoreError::new("BAD_REQUEST", "worker version header is not valid UTF-8"))?
        .trim();
    if version.is_empty() {
        return Err(CoreError::new(
            "BAD_REQUEST",
            "worker version header must not be empty",
        ));
    }
    Ok(Some(version.to_string()))
}

fn require_visible_worker(
    state: &OrchestratorState,
    principal: &ApiKeyPrincipal,
    name: &str,
    version: Option<&str>,
) -> Result<(), CoreError> {
    if state
        .index
        .admin_workers_for_principal(principal)
        .into_iter()
        .any(|worker| {
            worker.name == name && version.is_none_or(|version| worker.version == version)
        })
    {
        Ok(())
    } else {
        let target = version
            .map(|version| format!("{name}@{version}"))
            .unwrap_or_else(|| name.to_string());
        Err(CoreError::new(
            "NOT_FOUND",
            format!("worker not found: {target}"),
        ))
    }
}

fn public_versions(state: &OrchestratorState, name: &str) -> Result<Vec<String>, CoreError> {
    let workers = state
        .index
        .worker_refs()
        .into_iter()
        .filter(|worker| worker.name == name)
        .collect::<Vec<_>>();
    if workers.is_empty() {
        return Err(CoreError::new(
            "NOT_FOUND",
            format!("worker not found: {name}"),
        ));
    }
    let mut versions = workers
        .into_iter()
        .filter(|worker| worker.config.visibility == WorkerVisibility::Public)
        .map(|worker| worker.version)
        .collect::<Vec<_>>();
    versions.sort();
    Ok(versions)
}

fn format_versions(versions: &[String]) -> String {
    if versions.is_empty() {
        "(none)".into()
    } else {
        versions.join(", ")
    }
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
            // Escopo não vem da query string: quem o preenche é o handler,
            // com o principal autenticado.
            scope: None,
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
        Ok(principal) => {
            // O resumo é indexado por NOME de worker e traz o último erro de
            // cada um: entregá-lo inteiro daria, de uma vez, o inventário
            // alheio e a mensagem de cada falha. Root vê tudo; key escopada vê
            // o próprio pedaço.
            let mut summary = state.server.worker_errors().summary();
            if !principal.is_root {
                summary.retain(|name, _| principal_can_access_worker(&principal, name));
            }
            Json(json!({ "summary": summary })).into_response()
        }
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
        // O erro recente carrega mensagem e stack do worker. Sem este filtro a
        // rota lia pelo NOME cru, então uma key escopada em `a` recebia `200`
        // com os erros de `b` — o único ponto por onde o escopo por worker
        // vazava, já que as vizinhas resolvem o worker antes de responder.
        require_visible_worker(&state, &principal, &name, None)?;
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
    match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "observability:read")?;
        Ok(principal)
    }) {
        Ok(principal) => {
            let mut event_query: crate::observability::OperationalEventQuery = query.into();
            event_query.scope = Some(principal);
            Json(state.server.operational_events().query(event_query)).into_response()
        }
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

async fn observability_series(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Query(query): Query<ObservabilitySeriesQuery>,
) -> Response {
    let principal = match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "observability:read")?;
        Ok(principal)
    }) {
        Ok(principal) => principal,
        Err(err) => return admin_error(map_error_status(&err), &err, &headers),
    };
    let event_query = crate::observability::OperationalEventQuery {
        namespace: query.namespace,
        worker: query.worker,
        version: query.version,
        scope: Some(principal),
        ..Default::default()
    };
    Json(state.server.operational_events().series(
        event_query,
        query.window_ms.unwrap_or(5 * 60_000),
        query.bucket_ms.unwrap_or(15_000),
    ))
    .into_response()
}

/// (EDG-34) The cron jobs the scheduler tracks, for operators who cannot
/// read the logs. Permission `observability:read` (root passes); the list
/// is filtered by the key's worker scope — the same single predicate
/// (`principal_can_access_worker_name`) `list_workers`
/// (`admin_workers_for_principal`) applies per entry (emenda 2), applied
/// to each job's NAME rather than to index presence: a removed worker
/// whose job is still draining stays visible (with `version: null`) for
/// root and for the keys whose scope covers the name, and hidden from the
/// rest. Each item carries the job
/// identity, its `state` (`active`/`draining`), the `version` the
/// unversioned route resolves to AT QUERY TIME (the same `resolve_worker`
/// the tick uses; `null` when it does not resolve) and `nextRunAt` (the
/// wall-clock rendering of the monotonic deadline the task is waiting
/// for, RFC 3339 UTC; `null` while draining),
/// ordered by worker, schedule, path and method. `enabled` is `true` only
/// while the scheduler will actually dispatch — started and with the
/// Bearer root configured — and goes back to `false` when the supervisor
/// ends (emenda 2).
async fn list_cron_jobs(State(state): State<OrchestratorState>, headers: HeaderMap) -> Response {
    match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "observability:read")?;
        Ok(principal)
    }) {
        Ok(principal) => {
            // The same worker-scope choke point `list_workers` applies —
            // the SINGLE predicate on (principal, full worker name), the
            // one `admin_workers_for_principal` uses per entry (EDG-34
            // emenda 2). It is applied to the job's NAME, not to index
            // presence: a removed worker whose job is still draining keeps
            // its name in scope and stays visible (with `version: null`),
            // while a key whose scope does not cover the name sees none
            // of its jobs.
            let metrics = state.server.cron_metrics();
            let jobs = metrics.jobs().jobs;
            let principal = principal.clone();
            let jobs = cron_job_entries(
                jobs,
                |name| {
                    crate::manifest_index_stub::ManifestIndex::principal_can_access_worker_name(
                        &principal, name,
                    )
                },
                |name| state.index.resolve_default_version(name),
            );
            Json(json!({
                "enabled": metrics.scheduler_enabled(),
                "jobs": jobs,
            }))
            .into_response()
        }
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

/// (EDG-34) The job entries of the `GET /api/admin/cron/jobs` response:
/// the snapshot jobs that pass `scope_ok` (the key's worker scope, the
/// same single predicate `list_workers` applies — applied to the job's
/// NAME, so a removed worker whose job is still draining stays visible)
/// and, for each, its identity, the `version` the unversioned route
/// resolves to AT QUERY TIME through `resolve_version` (`null` when it
/// does not resolve) and `nextRunAt` (the wall-clock rendering of the
/// monotonic deadline the task is waiting for, RFC 3339 UTC).
///
/// (EDG-34 round 4, F1) The version of each worker NAME is resolved at
/// most ONCE per request: the scope filter runs first (a name out of
/// scope is never resolved) and the local memo, discarded with this call,
/// caches the first resolution per name — a manifest with many `cron[]`
/// entries costs O(distinct workers) per request, not O(jobs²).
fn cron_job_entries(
    jobs: Vec<crate::cron::CronJobInfo>,
    scope_ok: impl Fn(&str) -> bool,
    resolve_version: impl Fn(&str) -> Option<String>,
) -> Vec<serde_json::Value> {
    let mut memo: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    jobs.into_iter()
        .filter(|job| scope_ok(&job.worker))
        .map(|job| {
            // One resolution per worker NAME per request (F1): the memo
            // entry is only computed (and the name only cloned) on the
            // first job of the name; every later job of the name reuses it.
            let version = match memo.get(&job.worker) {
                Some(cached) => cached.clone(),
                None => {
                    let version = resolve_version(&job.worker);
                    memo.insert(job.worker.clone(), version.clone());
                    version
                }
            };
            json!({
                "worker": job.worker,
                "schedule": job.schedule,
                "path": job.path,
                "method": job.method,
                "version": version,
                "state": job.state.as_str(),
                "nextRunAt": job.next_run_at.map(|at| {
                    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                }),
            })
        })
        .collect()
}

async fn observability_events_stream(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Query(query): Query<ObservabilityEventsQuery>,
) -> Response {
    let principal = match authenticate(&state, &headers).await.and_then(|principal| {
        require_permission(&principal, "observability:read")?;
        Ok(principal)
    }) {
        Ok(principal) => principal,
        Err(err) => return admin_error(map_error_status(&err), &err, &headers),
    };
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
            query: {
                // O SSE fica aberto emitindo evento a evento: sem o escopo
                // aqui, a assinatura vaza continuamente o que a listagem
                // paginada já não entrega.
                let mut event_query: crate::observability::OperationalEventQuery = query.into();
                event_query.scope = Some(principal);
                event_query
            },
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

async fn recycle_worker_route(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerVersionQuery>,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "workers:toggle")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        let version = query
            .version
            .as_deref()
            .ok_or_else(|| CoreError::validation("version", "version is required"))?;
        require_visible_worker(&state, &principal, &name, Some(version))?;
        let worker = state
            .index
            .worker_refs()
            .into_iter()
            .find(|worker| worker.name == name && worker.version == version)
            .ok_or_else(|| {
                CoreError::new("NOT_FOUND", format!("worker {name}@{version} not found"))
            })?;
        let recycled = state.pool.recycle_worker(&name, Some(version)).await;
        let prewarm = if worker.config.enabled {
            schedule_min_process_prewarm(&state, &worker.name, &worker.version)
        } else {
            "not_configured"
        };
        Ok(json!({
            "name": name,
            "version": version,
            "recycled": recycled,
            "prewarm": prewarm,
        }))
    }
    .await;
    match result {
        Ok(response) => Json(response).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
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
            timeout_ms: None,
            code: code.clone(),
            message: Some(message.clone()),
            truncated: None,
            dropped_count: None,
            method: None,
            path: None,
            content_type: None,
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
    // workers:toggle, não root: ligar/desligar versões é decisão de tráfego
    // separada do promote (que escolhe a default).
    let auth = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "workers:toggle")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        require_visible_worker(&state, &principal, &name, version.as_deref())?;
        Ok(principal)
    }
    .await;
    match auth.and_then(|_| {
        // EDG-21: the toggle is persisted inside the version dir (same slot +
        // durable write pattern as the default-version pointer) before the
        // in-memory flag moves; a failed persist leaves memory unchanged and
        // errors here. MCP `enable_worker`/`disable_worker` reach the same
        // route, so they persist through this path too.
        persist_worker_enabled(&state.index, &name, version.as_deref(), enabled, None)
    }) {
        Ok(worker) => {
            // (EDG-12) enable prewarms the min-processes floor in the
            // background; disable never prewarms (the version is inactive,
            // so the helper reports `not_configured`). The response never
            // waits for the prewarm.
            let prewarm = schedule_min_process_prewarm(&state, &worker.name, &worker.version);
            Json(json!({
                "code": "OK",
                "message": format!(
                    "worker {}@{} {}",
                    worker.name, worker.version, worker.status
                ),
                "status": worker.status,
                "prewarm": prewarm,
            }))
            .into_response()
        }
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
        require_permission(&principal, "files:read")?;
        if bundle_artifact_in_sub_path(query.path.as_deref()) {
            return Err(CoreError::new("NOT_FOUND", "path not found"));
        }
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
        require_permission(&principal, "files:read")?;
        if bundle_artifact_in_sub_path(query.path.as_deref()) {
            return Err(CoreError::new("NOT_FOUND", "path not found"));
        }
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

// File upload into any user version (internal or public alike): mutability
// is gated by files:write alone and core workers stay read-only. The changed
// worker is recycled before the response so the next invocation observes the
// new files.
async fn upload_worker_files(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerFilesQuery>,
    body: Bytes,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "files:write")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        let payload = write_worker_files(
            &state,
            &principal,
            &name,
            query.version.as_deref(),
            query.path.as_deref(),
            &body,
        )?;
        let version = payload
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| CoreError::new("DEPLOY_INTERNAL", "worker version is missing"))?;
        state.pool.recycle_worker(&name, Some(version)).await;
        Ok(payload)
    }
    .await;
    match result {
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

// EDG-24: `release` runs outside the Deno sandbox as a subprocess with the
// worker's manifest env — declaring it is a root-only privilege. A non-root
// files upload that writes the version-root `manifest.yaml`/`manifest.yml`
// with a non-empty `release` is refused before any file is written. The
// same manifest parser as the rest of the code applies; a manifest that
// does not parse keeps the current behavior (the upload proceeds and the
// invalid manifest surfaces at the next index load).
//
// EDG-24 review (F2/F4): the refusal is MONOTONIC over every root manifest
// entry (both names, any order, duplicates) — a later manifest without
// `release` must not clear an earlier refusal, because the loader picks
// `manifest.yaml` first regardless of zip entry order. Manifest entries are
// read under a hard byte cap (`MAX_MANIFEST_BYTES`), checked against the
// central-directory metadata BEFORE decompressing and enforced on the read
// itself: a read error or a limit hit refuses the upload — it never
// silently authorizes it.
//
// EDG-24 review (G1): the cap is an AGGREGATE budget for ALL root manifest
// entries summed — the sum of the declared sizes is checked before reading
// ANY entry, then the sum of the bytes actually read. A single per-entry
// cap is not enough: 257 duplicated root `manifest.yaml` entries of ~1 MiB
// each (reachable even as `entry{n}/../manifest.yaml` — `enclosed_name`
// normalizes them back to the root) read ~269 MB.
// EDG-24 review (G2): manifest names are compared case-insensitively (ASCII)
// — on case-insensitive filesystems `MANIFEST.YAML` IS the version-root
// manifest.
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// EDG-24 (G2/G3): the version-root manifest names, compared by Unicode
/// case folding (see `deploy::fold_reserved_name`) — on case-insensitive
/// filesystems `MANIFEST.YAML` and the long-s alias `manife\u{17F}t.yaml`
/// ARE the version-root manifest.
fn is_root_manifest_name(name: &str) -> bool {
    let folded = crate::deploy::fold_reserved_name(name);
    folded == "manifest.yaml" || folded == "manifest.yml"
}

/// EDG-24 (emenda 3): the extra defense for non-root uploads. A root entry
/// whose FOLDED name is a manifest name but whose original name is not
/// exactly `manifest.yaml`/`manifest.yml` in ASCII (a Unicode alias such as
/// `manife\u{17F}t.yaml`) — refused even without `release`, so a Unicode
/// alias of the manifest is never written by a non-root key.
fn uploaded_zip_has_unicode_manifest_alias(zip_bytes: &[u8]) -> Result<bool, CoreError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|err| CoreError::new("DEPLOY_INVALID_PACKAGE", format!("invalid zip: {err}")))?;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|err| {
            CoreError::new(
                "DEPLOY_INVALID_PACKAGE",
                format!("invalid zip entry: {err}"),
            )
        })?;
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        if relative
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            continue;
        }
        let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_root_manifest_name(name) {
            continue;
        }
        if name != "manifest.yaml" && name != "manifest.yml" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn uploaded_manifest_declares_release(zip_bytes: &[u8]) -> Result<bool, CoreError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|err| CoreError::new("DEPLOY_INVALID_PACKAGE", format!("invalid zip: {err}")))?;
    // G1, pass 1: collect the root manifest candidates (same normalization
    // as `extract_zip`: `enclosed_name` — escaping entries are left for
    // `extract_zip` to reject) and their DECLARED sizes.
    let mut candidates: Vec<(usize, u64)> = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|err| {
            CoreError::new(
                "DEPLOY_INVALID_PACKAGE",
                format!("invalid zip entry: {err}"),
            )
        })?;
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        if relative
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            continue;
        }
        let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_root_manifest_name(name) {
            continue;
        }
        // F4/G1: the central-directory size is metadata the uploader
        // controls; the sum over ALL candidates is checked before reading
        // any entry.
        candidates.push((index, entry.size()));
    }
    let declared_total: u64 = candidates.iter().map(|(_, size)| *size).sum();
    if declared_total > MAX_MANIFEST_BYTES as u64 {
        return Err(CoreError::new("PAYLOAD_TOO_LARGE", "manifest too large"));
    }
    // G1, pass 2: read each candidate under a per-entry cap (bounds a
    // single decompression even when the metadata under-reports it) and
    // enforce the AGGREGATE read budget.
    let mut declares = false;
    let mut read_total: u64 = 0;
    for (index, _) in &candidates {
        let entry = archive.by_index(*index).map_err(|err| {
            CoreError::new(
                "DEPLOY_INVALID_PACKAGE",
                format!("invalid zip entry: {err}"),
            )
        })?;
        let mut limited = entry.take(MAX_MANIFEST_BYTES as u64 + 1);
        let mut text = String::new();
        limited.read_to_string(&mut text).map_err(|err| {
            CoreError::new(
                "DEPLOY_INVALID_PACKAGE",
                format!("failed to read manifest entry: {err}"),
            )
        })?;
        read_total = read_total.saturating_add(text.len() as u64);
        if read_total > MAX_MANIFEST_BYTES as u64 {
            return Err(CoreError::new("PAYLOAD_TOO_LARGE", "manifest too large"));
        }
        // F2: OR-accumulate — the refusal never clears, whatever comes
        // after in the archive. A manifest that does not parse keeps the
        // current behavior.
        if let Ok(manifest) =
            serde_saphyr::from_str_with_options::<WorkerManifest>(&text, manifest_yaml_options())
        {
            declares |= manifest
                .release
                .as_deref()
                .is_some_and(|release| !release.trim().is_empty());
        }
    }
    Ok(declares)
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
    // files:write is the only mutability gate: any user version (internal or
    // public) accepts the upload.
    let rel = sub_path.unwrap_or("").trim_matches('/');
    if rel.split('/').any(|segment| segment == "..") {
        return Err(CoreError::new(
            "FORBIDDEN",
            "path escapes the worker directory",
        ));
    }
    // Mesmo slot do install/delete, e ANTES de qualquer mutação (inclusive o
    // create_dir_all de subpasta nova): um upload no meio da transação de um
    // force mutaria o candidato e o rollback desfaria o upload em silêncio.
    // Segura até o fim da escrita + bump.
    let mutation_root = base
        .parent()
        .ok_or_else(|| CoreError::new("DEPLOY_INTERNAL", "worker directory has no parent"))?;
    let _slot =
        crate::deploy::claim_worker_mutation_slot(mutation_root, &worker.name, &worker.version)?;
    // EDG-39: a subpasta nova do `?path=` só nasce na fase 2 — a parte que
    // já existe é aberta segmento a segmento a partir da versão, o resto é
    // criado (e desfeito num rollback) junto com as entradas do zip.
    // EDG-40: nenhum segmento pode ser link simbólico (403 FORBIDDEN).
    let (existing_dir, existing, missing) = split_upload_path(&base, rel)?;
    // EDG-24 review (F1): whether the upload lands on the version root is
    // decided by the destination itself, not by the path spelling: `.` and
    // any other alias of the root (a `..` segment is rejected above and a
    // link segment by the split, EDG-40, so nothing else reaches base) get
    // the root guards. A path with a missing segment is never the root.
    let at_version_root = existing.as_os_str().is_empty() && missing.as_os_str().is_empty();
    // EDG-24: the `release` command runs outside the Deno sandbox — only
    // root may declare it, so a non-root upload that writes the version-
    // root manifest with a non-empty `release` is refused before any file
    // is written.
    if !principal.is_root && at_version_root {
        // EDG-24 (emenda 3): a Unicode alias of the manifest name is
        // refused on its own — even without `release` — so it is never
        // written (on APFS it would BE the version-root manifest).
        if uploaded_zip_has_unicode_manifest_alias(zip_bytes)? {
            return Err(CoreError::new(
                "RELEASE_REQUIRES_ROOT",
                "only root may declare a release command",
            ));
        }
        if uploaded_manifest_declares_release(zip_bytes)? {
            return Err(CoreError::new(
                "RELEASE_REQUIRES_ROOT",
                "only root may declare a release command",
            ));
        }
    }
    // EDG-21/EDG-24: uma extração na raiz da versão (por qualquer grafia)
    // nunca escreve os marcadores reservados (estado de operador/EdgeR) —
    // um upload não pode substituí-los; em subcaminhos o nome é arquivo
    // comum e segue para o destino.
    //
    // EDG-39 fase 1: tudo é extraído e validado num staging irmão da versão
    // (mesmo filesystem, para o rename); uma recusa aqui só apaga o staging.
    let staging = upload_temp_dir(mutation_root)?;
    let entries = extract_zip_entries(zip_bytes, staging.path(), at_version_root)?;
    // Fase 2: as entradas vão para a versão na ordem do zip, com rollback.
    let backup = upload_temp_dir(mutation_root)?;
    apply_staged_upload(
        &existing_dir,
        &existing,
        &missing,
        staging.path(),
        &entries,
        backup,
    )?;
    // Arquivos mudaram: a revisão CAS avança, senão um autosave force com a
    // revisão antiga passaria por cima da edição sem conflito.
    let revision = crate::deploy::bump_worker_revision(&base)?;
    let mut payload = list_worker_files(state, principal, name, version, sub_path)?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("revision".into(), serde_json::Value::String(revision));
    }
    Ok(payload)
}

// EDG-39: staging and backup dirs of a files upload. Siblings of the version
// dir (the mutation root), so every move is a same-filesystem rename; the
// prefix keeps the scanner and the state export away from them, and the
// `TempDir` removes them on success and failure.
const UPLOAD_TEMP_PREFIX: &str = ".edger-upload-";

fn upload_temp_dir(mutation_root: &std::path::Path) -> Result<tempfile::TempDir, CoreError> {
    tempfile::Builder::new()
        .prefix(UPLOAD_TEMP_PREFIX)
        .tempdir_in(mutation_root)
        .map_err(|err| upload_io(format!("failed to create staging dir: {err}")))
}

fn upload_io(message: String) -> CoreError {
    CoreError::new("DEPLOY_IO", message)
}

// EDG-40: below the version dir (`base`), no operation of the upload goes
// through a symbolic link, in an intermediate or in the final component. On
// the version side every operation is relative to a directory descriptor
// opened one component at a time from `base` with `O_NOFOLLOW`, and uses the
// `*at` calls (`statat` without following, `mkdirat`, `openat`, `linkat`,
// `renameat`, `unlinkat`): a link swapped in after a check is never followed
// either, the open of that component fails with `ELOOP`. The staging and
// backup dirs (private and new) stay path-based. The code is Unix only, as
// the rest of the workspace (the process runtime needs Unix sockets).
enum UploadFsError {
    // A symbolic link: refused, never followed.
    Link,
    Io(std::io::Error),
}

impl From<std::io::Error> for UploadFsError {
    fn from(err: std::io::Error) -> Self {
        UploadFsError::Io(err)
    }
}

impl From<rustix::io::Errno> for UploadFsError {
    fn from(errno: rustix::io::Errno) -> Self {
        UploadFsError::Io(errno.into())
    }
}

impl UploadFsError {
    // A link met by a segment of `?path=` (403) or by a zip entry (400).
    fn path_error(self) -> CoreError {
        match self {
            UploadFsError::Link => CoreError::new("FORBIDDEN", "path crosses a symbolic link"),
            UploadFsError::Io(err) => {
                CoreError::new("BAD_REQUEST", format!("cannot create path: {err}"))
            }
        }
    }

    fn entry_error(self, entry: &std::path::Path, context: &str) -> CoreError {
        match self {
            UploadFsError::Link => CoreError::new(
                "DEPLOY_PATH_DENIED",
                format!("zip entry crosses a symbolic link: {}", entry.display()),
            ),
            UploadFsError::Io(err) => upload_io(format!("{context}: {err}")),
        }
    }

    // For the rollback log, which records only the error kind.
    fn into_io(self) -> std::io::Error {
        match self {
            UploadFsError::Link => rustix::io::Errno::LOOP.into(),
            UploadFsError::Io(err) => err,
        }
    }
}

const UPLOAD_DIR_FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
    .union(rustix::fs::OFlags::DIRECTORY)
    .union(rustix::fs::OFlags::NOFOLLOW)
    .union(rustix::fs::OFlags::CLOEXEC);

// The version dir itself, canonical (`resolve_worker_dir`); `O_NOFOLLOW`
// refuses it if it was swapped for a link since.
fn open_upload_base(base: &std::path::Path) -> Result<std::os::fd::OwnedFd, CoreError> {
    rustix::fs::openat(
        rustix::fs::CWD,
        base,
        UPLOAD_DIR_FLAGS,
        rustix::fs::Mode::empty(),
    )
    .map_err(|err| {
        CoreError::new(
            "NOT_FOUND",
            format!(
                "worker directory unavailable: {}",
                std::io::Error::from(err)
            ),
        )
    })
}

// The type of `name` in `dir`, links not followed: `None` when absent, a
// link is `UploadFsError::Link`.
fn upload_entry_kind(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
) -> Result<Option<rustix::fs::FileType>, UploadFsError> {
    match rustix::fs::statat(dir, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
            rustix::fs::FileType::Symlink => Err(UploadFsError::Link),
            kind => Ok(Some(kind)),
        },
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(errno) => Err(errno.into()),
    }
}

// Opens the directory `name` of `dir` without following a link: a link
// there (`ELOOP`, or `ENOTDIR` where a platform answers that) is
// `UploadFsError::Link`.
fn open_upload_subdir(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
) -> Result<std::os::fd::OwnedFd, UploadFsError> {
    match rustix::fs::openat(dir, name, UPLOAD_DIR_FLAGS, rustix::fs::Mode::empty()) {
        Ok(fd) => Ok(fd),
        Err(rustix::io::Errno::LOOP) => Err(UploadFsError::Link),
        Err(rustix::io::Errno::NOTDIR) => match upload_entry_kind(dir, name) {
            Err(UploadFsError::Link) => Err(UploadFsError::Link),
            _ => Err(rustix::io::Errno::NOTDIR.into()),
        },
        Err(errno) => Err(errno.into()),
    }
}

// The normal segments of a relative path (`.` dropped; `..` never reaches
// here: refused for `?path=`, normalized away by `enclosed_name` for entries).
fn upload_segments(relative: &std::path::Path) -> Vec<&std::ffi::OsStr> {
    relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(segment) => Some(segment),
            _ => None,
        })
        .collect()
}

// Splits the `?path=` of an upload into its existing part — opened segment by
// segment from `base` without following links, so a link in any segment
// (pointing inside or outside the version) is 403 FORBIDDEN — and the missing
// segments, created only in phase 2. Every existing segment must be a
// directory: anything else is the same BAD_REQUEST, with the same system
// error, the old `create_dir_all` answered. Nothing here writes to disk; an
// obstacle that disappears in a race leaves its segment missing, and phase 2
// creates it transactionally. Returns the descriptor of the existing part and
// its path relative to `base`.
fn split_upload_path(
    base: &std::path::Path,
    rel: &str,
) -> Result<(std::os::fd::OwnedFd, std::path::PathBuf, std::path::PathBuf), CoreError> {
    let mut dir = open_upload_base(base)?;
    let mut existing = std::path::PathBuf::new();
    let mut missing = std::path::PathBuf::new();
    let segments = upload_segments(std::path::Path::new(rel));
    for (position, segment) in segments.iter().enumerate() {
        if !missing.as_os_str().is_empty() {
            missing.push(segment);
            continue;
        }
        match upload_entry_kind(&dir, segment).map_err(UploadFsError::path_error)? {
            Some(rustix::fs::FileType::Directory) => {
                dir = open_upload_subdir(&dir, segment).map_err(UploadFsError::path_error)?;
                existing.push(segment);
                continue;
            }
            None => {}
            Some(_) => {
                let is_last = position + 1 == segments.len();
                if let Some(err) = upload_obstacle_error(&dir, segment, is_last) {
                    return Err(err.path_error());
                }
            }
        }
        missing.push(segment);
    }
    Ok((dir, existing, missing))
}

#[cfg(test)]
thread_local! {
    // Runs once right before the error for an obstacle is selected.
    static UPLOAD_BEFORE_OBSTACLE_ERROR: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

// EDG-39 review R3: the error the old `create_dir_all` answered for the
// non-directory found at `name` in `dir`, selected with calls that change
// nothing on disk. Below a file it is `Not a directory`; on the last segment
// of the walk it is `File exists`. EDG-40: a link there is
// `UploadFsError::Link`. `None` when the obstacle is gone or became a
// directory (a race): the caller then creates the level through the record.
fn upload_obstacle_error(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    is_last: bool,
) -> Option<UploadFsError> {
    #[cfg(test)]
    if let Some(hook) = UPLOAD_BEFORE_OBSTACLE_ERROR.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
    match upload_entry_kind(dir, name) {
        Err(UploadFsError::Link) => Some(UploadFsError::Link),
        Err(UploadFsError::Io(_)) | Ok(None) | Ok(Some(rustix::fs::FileType::Directory)) => None,
        Ok(Some(_)) if is_last => Some(rustix::io::Errno::EXIST.into()),
        Ok(Some(_)) => Some(rustix::io::Errno::NOTDIR.into()),
    }
}

// EDG-39 review F3/F4: the old `fs::write` refused a target it could not open
// for writing (a read-only file, a directory). Opening it the same way, with
// neither `O_TRUNC` nor `O_CREAT`, returns that same system error before
// anything is published; it changes nothing on disk. EDG-39 review R1: the
// caller has already refused anything but a regular file or a directory
// (`statat` without following, in `apply_upload_file`) and the open is
// non-blocking, so a target swapped for a FIFO in a race fails instead of
// waiting for a reader. EDG-40: the open carries `O_NOFOLLOW`, so a target
// swapped for a link in a race is refused (`ELOOP`) instead of opened; the
// metadata (mode, link count) is read from the file that was opened, which
// must still be a regular file.
fn probe_upload_target(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
) -> Result<std::fs::Metadata, UploadFsError> {
    let flags = rustix::fs::OFlags::WRONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    let fd = match rustix::fs::openat(dir, name, flags, rustix::fs::Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::LOOP) => return Err(UploadFsError::Link),
        Err(errno) => return Err(errno.into()),
    };
    let metadata = std::fs::File::from(fd).metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("not a regular file").into());
    }
    Ok(metadata)
}

#[cfg(test)]
thread_local! {
    // Makes the backup hard link fail, to exercise the copy fallback.
    static UPLOAD_FAIL_HARD_LINK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// `linkat` without `AT_SYMLINK_FOLLOW`: never links through a link.
fn backup_hard_link(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    backup: &std::path::Path,
) -> std::io::Result<()> {
    #[cfg(test)]
    if UPLOAD_FAIL_HARD_LINK.with(|fail| fail.get()) {
        return Err(std::io::Error::other("injected hard link failure"));
    }
    rustix::fs::linkat(
        dir,
        name,
        rustix::fs::CWD,
        backup,
        rustix::fs::AtFlags::empty(),
    )
    .map_err(Into::into)
}

// The copy fallback of the backup reads the target through `openat` with
// `O_NOFOLLOW` (and refuses anything but a regular file once open), and keeps
// its mode, so a rollback that renames the copy back restores it.
fn backup_copy(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    backup: &std::path::Path,
    mode: u32,
) -> Result<(), UploadFsError> {
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    let fd = match rustix::fs::openat(dir, name, flags, rustix::fs::Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::LOOP) => return Err(UploadFsError::Link),
        Err(errno) => return Err(errno.into()),
    };
    let stat = rustix::fs::fstat(&fd)?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
        return Err(std::io::Error::other("not a regular file").into());
    }
    let mut source = std::fs::File::from(fd);
    let mut copy = std::fs::File::create(backup)?;
    std::io::copy(&mut source, &mut copy)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(backup, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

// One reversible effect of phase 2, undone in reverse order on failure. Paths
// are relative to the existing part of `?path=` (the descriptor the rollback
// walks from, again without following links).
enum UploadStep {
    CreatedDir(std::path::PathBuf),
    CreatedFile(std::path::PathBuf),
    // An existing file replaced by the staged one, recorded only once the
    // replacement is published. The backup is a hard link of the old file
    // (a copy where linking fails and the file has no other link), so the old
    // file stays visible until one rename publishes the new one, and one
    // rename puts it back.
    Replaced {
        target: std::path::PathBuf,
        backup: std::path::PathBuf,
    },
}

#[cfg(test)]
thread_local! {
    // Number of recorded phase-2 steps after which the next mutation fails.
    static UPLOAD_FAULT_AFTER: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    // Runs once right after the backup of a replaced file is taken.
    static UPLOAD_AFTER_BACKUP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    // EDG-40: runs once right after a phase-2 walk found an existing
    // directory and before it opens it.
    static UPLOAD_AFTER_DIR_CHECK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    // EDG-40: runs once right before a staged file is published.
    static UPLOAD_BEFORE_PUBLISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

fn upload_after_backup_point() {
    #[cfg(test)]
    if let Some(hook) = UPLOAD_AFTER_BACKUP.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
}

fn upload_after_dir_check_point() {
    #[cfg(test)]
    if let Some(hook) = UPLOAD_AFTER_DIR_CHECK.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
}

#[cfg(test)]
thread_local! {
    // EDG-40: runs once right after the final target was checked as a
    // regular file and before the write probe opens it.
    static UPLOAD_BEFORE_PROBE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    // EDG-40: runs once right after the write probe and before the backup of
    // the replaced file is taken.
    static UPLOAD_BEFORE_BACKUP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

fn upload_before_probe_point() {
    #[cfg(test)]
    if let Some(hook) = UPLOAD_BEFORE_PROBE.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
}

fn upload_before_backup_point() {
    #[cfg(test)]
    if let Some(hook) = UPLOAD_BEFORE_BACKUP.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
}

fn upload_before_publish_point() {
    #[cfg(test)]
    if let Some(hook) = UPLOAD_BEFORE_PUBLISH.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
}

fn upload_fault_point(steps: usize) -> Result<(), UploadFsError> {
    #[cfg(test)]
    if UPLOAD_FAULT_AFTER.with(|fault| fault.get()) == Some(steps) {
        return Err(std::io::Error::other("injected upload fault").into());
    }
    let _ = steps;
    Ok(())
}

// EDG-39 phase 2: replays the staged entries onto the version dir, in archive
// order, by rename. On any error every recorded step is undone (backups back,
// created files and dirs removed, deepest first) and the ORIGINAL error is
// returned; a step the rollback cannot undo is a `warn` with relative paths
// only, and the backup dir is then kept so no old contents are lost.
// `existing` is the descriptor of the existing part of `?path=` and
// `existing_rel` its path relative to the version dir (for the log).
fn apply_staged_upload(
    existing: &std::os::fd::OwnedFd,
    existing_rel: &std::path::Path,
    missing: &std::path::Path,
    staging: &std::path::Path,
    entries: &[crate::deploy::ExtractedEntry],
    backup: tempfile::TempDir,
) -> Result<(), CoreError> {
    let mut steps = Vec::new();
    let result = apply_staged_steps(
        existing,
        missing,
        staging,
        entries,
        backup.path(),
        &mut steps,
    );
    if let Err(error) = result {
        if !rollback_upload_steps(existing, existing_rel, &steps) {
            let kept = backup.keep();
            tracing::warn!(
                backup = %kept.file_name().unwrap_or_default().to_string_lossy(),
                "files upload rollback incomplete; backup dir kept"
            );
        }
        return Err(error);
    }
    Ok(())
}

fn apply_staged_steps(
    existing: &std::os::fd::OwnedFd,
    missing: &std::path::Path,
    staging: &std::path::Path,
    entries: &[crate::deploy::ExtractedEntry],
    backup_dir: &std::path::Path,
    steps: &mut Vec<UploadStep>,
) -> Result<(), CoreError> {
    let dest = ensure_upload_dirs(existing, std::path::Path::new(""), missing, steps)
        .map_err(UploadFsError::path_error)?;
    let firsts = staged_file_firsts(staging, entries)
        .map_err(|err| upload_io(format!("failed to write file: {err}")))?;
    for (entry, first) in entries.iter().zip(firsts) {
        if entry.is_dir {
            ensure_upload_dirs(&dest, missing, &entry.relative, steps)
                .map_err(|err| err.entry_error(&entry.relative, "failed to create dir"))?;
            continue;
        }
        // A repeated file: the staged file already holds the last entry.
        if !first {
            continue;
        }
        let parent = entry.relative.parent().unwrap_or(std::path::Path::new(""));
        let dir = ensure_upload_dirs(&dest, missing, parent, steps)
            .map_err(|err| err.entry_error(&entry.relative, "failed to create dir"))?;
        let Some(name) = entry.relative.file_name() else {
            continue;
        };
        let source = staging.join(&entry.relative);
        apply_upload_file(
            &dir,
            name,
            &source,
            missing.join(&entry.relative),
            backup_dir,
            steps,
        )
        .map_err(|err| err.entry_error(&entry.relative, "failed to write file"))?;
    }
    Ok(())
}

// `true` for each entry that is the first occurrence of its staged file. A
// repeated name, or (EDG-39 review F2) two names the filesystem treats as one
// file (`Name.txt` and `name.txt` on a case-insensitive volume), share one
// staged file: it is applied once, under the first name (the one on disk) and
// at the first position, and holds the last entry, as the old in-place writes
// left it. Identities are read before any staged file moves.
fn staged_file_firsts(
    staging: &std::path::Path,
    entries: &[crate::deploy::ExtractedEntry],
) -> std::io::Result<Vec<bool>> {
    use std::os::unix::fs::MetadataExt;
    let mut names = std::collections::HashSet::new();
    let mut files = std::collections::HashSet::new();
    let mut firsts = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut first = entry.is_dir || names.insert(entry.relative.clone());
        if first && !entry.is_dir {
            let metadata = std::fs::symlink_metadata(staging.join(&entry.relative))?;
            first = files.insert((metadata.dev(), metadata.ino()));
        }
        firsts.push(first);
    }
    Ok(firsts)
}

// Opens (creating what is missing) every directory of `relative` under
// `root`, one level at a time and without following links, recording each
// created one (`prefix` + its path, relative to the existing part of
// `?path=`) before the next step; returns the descriptor of the last one. A
// non-directory in the way is the old `create_dir_all` error, a link is
// `UploadFsError::Link`.
fn ensure_upload_dirs(
    root: &std::os::fd::OwnedFd,
    prefix: &std::path::Path,
    relative: &std::path::Path,
    steps: &mut Vec<UploadStep>,
) -> Result<std::os::fd::OwnedFd, UploadFsError> {
    let mut dir = root.try_clone()?;
    let mut current = prefix.to_path_buf();
    let segments = upload_segments(relative);
    for (position, segment) in segments.iter().enumerate() {
        current.push(segment);
        match upload_entry_kind(&dir, segment)? {
            Some(rustix::fs::FileType::Directory) => {
                upload_after_dir_check_point();
                dir = open_upload_subdir(&dir, segment)?;
                continue;
            }
            None => {}
            Some(_) => {
                let is_last = position + 1 == segments.len();
                if let Some(err) = upload_obstacle_error(&dir, segment, is_last) {
                    return Err(err);
                }
            }
        }
        upload_fault_point(steps.len())?;
        rustix::fs::mkdirat(&dir, *segment, rustix::fs::Mode::from_raw_mode(0o777))?;
        steps.push(UploadStep::CreatedDir(current.clone()));
        dir = open_upload_subdir(&dir, segment)?;
    }
    Ok(dir)
}

// Publishes the staged `source` as `name` in `dir`. `at` is the path the
// step records.
fn apply_upload_file(
    dir: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    source: &std::path::Path,
    at: std::path::PathBuf,
    backup_dir: &std::path::Path,
    steps: &mut Vec<UploadStep>,
) -> Result<(), UploadFsError> {
    upload_fault_point(steps.len())?;
    let backup_path = backup_dir.join(steps.len().to_string());
    let stat = match rustix::fs::statat(dir, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => {
            upload_before_publish_point();
            rustix::fs::renameat(rustix::fs::CWD, source, dir, name)?;
            steps.push(UploadStep::CreatedFile(at));
            return Ok(());
        }
        Err(errno) => return Err(errno.into()),
        Ok(stat) => stat,
    };
    match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
        // EDG-40: a final link is refused before any effect.
        rustix::fs::FileType::Symlink => return Err(UploadFsError::Link),
        // Opening a directory for writing always fails (`Is a directory`);
        // the fallback only covers a platform that would let it through.
        rustix::fs::FileType::Directory => {
            return Err(match probe_upload_target(dir, name) {
                Err(err) => err,
                Ok(_) => rustix::io::Errno::ISDIR.into(),
            })
        }
        rustix::fs::FileType::RegularFile => {}
        // A FIFO, socket or device is refused without being opened.
        _ => return Err(std::io::Error::other("not a regular file").into()),
    }
    upload_before_probe_point();
    let metadata = probe_upload_target(dir, name)?;
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o7777
    };
    // EDG-39 review F1: the old file never leaves `target` before the new
    // one replaces it — a rescan that does not take the slot always sees a
    // manifest there.
    // EDG-39 review R2: a copy has another inode, so it can only stand in
    // for a file without other links to keep.
    upload_before_backup_point();
    if let Err(err) = backup_hard_link(dir, name, &backup_path) {
        if std::os::unix::fs::MetadataExt::nlink(&metadata) > 1 {
            return Err(err.into());
        }
        backup_copy(dir, name, &backup_path, mode)?;
    }
    upload_after_backup_point();
    // fs::write kept the mode of the file it overwrote.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(source, std::fs::Permissions::from_mode(mode))?;
    }
    upload_before_publish_point();
    // A target swapped for a link after the checks: `renameat` replaces the
    // link entry itself, it never writes through it.
    rustix::fs::renameat(rustix::fs::CWD, source, dir, name)?;
    // Recorded only now: a failure above left `target` untouched, so the
    // rollback must not "restore" it (the backup just goes away with its
    // dir).
    steps.push(UploadStep::Replaced {
        target: at,
        backup: backup_path,
    });
    Ok(())
}

// Runs `op` on the last segment of `path`, in its parent opened from `root`
// without following links.
fn with_upload_parent(
    root: &std::os::fd::OwnedFd,
    path: &std::path::Path,
    op: impl FnOnce(&std::os::fd::OwnedFd, &std::ffi::OsStr) -> rustix::io::Result<()>,
) -> Result<(), UploadFsError> {
    let mut dir = root.try_clone()?;
    for segment in upload_segments(path.parent().unwrap_or(std::path::Path::new(""))) {
        dir = open_upload_subdir(&dir, segment)?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("empty upload path"))?;
    op(&dir, name).map_err(Into::into)
}

// Undoes the recorded steps in reverse order; `false` when any step could
// not be undone (logged with paths relative to the version dir only).
fn rollback_upload_steps(
    existing: &std::os::fd::OwnedFd,
    existing_rel: &std::path::Path,
    steps: &[UploadStep],
) -> bool {
    let mut clean = true;
    for step in steps.iter().rev() {
        let (path, outcome) = match step {
            UploadStep::CreatedDir(path) => (
                path,
                with_upload_parent(existing, path, |dir, name| {
                    rustix::fs::unlinkat(dir, name, rustix::fs::AtFlags::REMOVEDIR)
                }),
            ),
            UploadStep::CreatedFile(path) => (
                path,
                with_upload_parent(existing, path, |dir, name| {
                    rustix::fs::unlinkat(dir, name, rustix::fs::AtFlags::empty())
                }),
            ),
            UploadStep::Replaced { target, backup } => (
                target,
                with_upload_parent(existing, target, |dir, name| {
                    rustix::fs::renameat(rustix::fs::CWD, backup, dir, name)
                }),
            ),
        };
        if let Err(err) = outcome {
            clean = false;
            tracing::warn!(
                path = %existing_rel.join(path).display(),
                error = %err.into_io().kind(),
                "files upload rollback step failed"
            );
        }
    }
    clean
}

// Batch delete of files/directories inside a deployed user version. Per-item
// failures come back in the 200 body (partial failure is the contract); the
// CAS revision advances only when at least one item was actually removed.
const MAX_FILE_DELETIONS: usize = 1000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteWorkerFilesBody {
    paths: Vec<String>,
}

fn delete_within(base: &std::path::Path, rel: &str) -> Result<(), CoreError> {
    if rel.is_empty() || rel == "." {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            "use the version delete to remove the whole version",
        ));
    }
    if rel.split('/').any(|segment| segment == "..") {
        return Err(CoreError::new(
            "FORBIDDEN",
            "path escapes the worker directory",
        ));
    }
    // Parent canônico + último segmento: o alvo NUNCA é canonicalizado, que
    // seguiria o symlink (`root-alias -> .` apontaria para a própria raiz).
    let segments: Vec<&str> = rel
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let name = *segments.last().expect("rel is non-empty and not .");
    let parent_rel = segments[..segments.len() - 1].join("/");
    let parent = std::fs::canonicalize(base.join(parent_rel))
        .map_err(|err| CoreError::new("NOT_FOUND", format!("path not found: {err}")))?;
    if !parent.starts_with(base) {
        return Err(CoreError::new(
            "FORBIDDEN",
            "path escapes the worker directory",
        ));
    }
    // EDG-21/EDG-24: o nome reservado vale pela regra de NOME — pai canônico
    // do alvo + nome de marcador — e roda ANTES do check de existência:
    // numa caixa diferente (`.EDGER-RELEASE`, sem distinção de caixa pela
    // G2) o alvo pode nem existir no filesystem case-sensitive (Linux) e a
    // reserva ainda tem que valer; em APFS/Windows é o próprio marcador.
    if parent == *base && crate::deploy::is_enabled_marker_name(name) {
        return Err(CoreError::new("FORBIDDEN", "reserved file"));
    }
    // EDG-24: o marcador de release é estado do EdgeR (a command de release
    // da versão já rodou) — apagar pela API de arquivos faria o rescan/boot
    // rodar a command de novo; vale para todos, inclusive root, só na raiz.
    if parent == *base && crate::deploy::is_release_marker_name(name) {
        return Err(CoreError::new("FORBIDDEN", "reserved file"));
    }
    let target = parent.join(name);
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CoreError::new(
                "NOT_FOUND",
                format!("path not found: {err}"),
            ));
        }
        Err(err) => {
            return Err(CoreError::new(
                "DEPLOY_INTERNAL",
                format!("cannot read path: {err}"),
            ));
        }
    };
    if name == crate::deploy::REVISION_FILE {
        return Err(CoreError::new("FORBIDDEN", "reserved file"));
    }
    if target == *base {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            "use the version delete to remove the whole version",
        ));
    }
    if metadata.file_type().is_symlink() {
        // Symlink some como ele mesmo — remove_file não segue o link: nem o
        // marcador, nem a raiz, nem uma pasta fora da versão são tocados.
        std::fs::remove_file(&target).map_err(|err| {
            CoreError::new("DEPLOY_INTERNAL", format!("cannot remove symlink: {err}"))
        })?;
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(&target).map_err(|err| {
            CoreError::new("DEPLOY_INTERNAL", format!("cannot remove directory: {err}"))
        })?;
    } else {
        std::fs::remove_file(&target).map_err(|err| {
            CoreError::new("DEPLOY_INTERNAL", format!("cannot remove file: {err}"))
        })?;
    }
    Ok(())
}

fn delete_worker_file_paths(
    state: &OrchestratorState,
    principal: &ApiKeyPrincipal,
    name: &str,
    version: Option<&str>,
    body: &[u8],
) -> Result<(bool, String, serde_json::Value), CoreError> {
    let requested: DeleteWorkerFilesBody = serde_json::from_slice(body)
        .map_err(|err| CoreError::new("VALIDATION_ERROR", format!("invalid body: {err}")))?;
    if requested.paths.is_empty() || requested.paths.len() > MAX_FILE_DELETIONS {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            format!("paths must contain 1..={MAX_FILE_DELETIONS} entries"),
        ));
    }
    let (base, worker) = resolve_worker_dir(state, principal, name, version)?;
    if worker.origin != WorkerOrigin::User {
        return Err(CoreError::new(
            "FORBIDDEN",
            "core worker files are read-only; publish a new core overlay version instead",
        ));
    }
    // Mesma exclusão mútua do upload: apagar e force/install de outra
    // transação não podem correr em paralelo sobre a mesma versão.
    let mutation_root = base
        .parent()
        .ok_or_else(|| CoreError::new("DEPLOY_INTERNAL", "worker directory has no parent"))?;
    let _slot =
        crate::deploy::claim_worker_mutation_slot(mutation_root, &worker.name, &worker.version)?;
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    for path in &requested.paths {
        let rel = path.trim_matches('/');
        match delete_within(&base, rel) {
            Ok(()) => deleted.push(path.clone()),
            Err(err) => errors.push(json!({
                "path": path,
                "code": err.code,
                "message": err.message,
            })),
        }
    }
    let deleted_any = !deleted.is_empty();
    let revision = if deleted_any {
        crate::deploy::bump_worker_revision(&base)?
    } else {
        crate::deploy::worker_revision(&base).unwrap_or_default()
    };
    let entries = list_worker_files(state, principal, name, version, None)?
        .get("entries")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok((
        deleted_any,
        worker.version,
        json!({
            "deleted": deleted,
            "errors": errors,
            "revision": revision,
            "entries": entries,
        }),
    ))
}

async fn delete_worker_files(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<WorkerFilesQuery>,
    body: Bytes,
) -> Response {
    let result = async {
        let principal = authenticate(&state, &headers).await?;
        require_permission(&principal, "files:delete")?;
        validate_admin_mutation_security("POST", &headers, &principal)?;
        let (deleted_any, version, payload) =
            delete_worker_file_paths(&state, &principal, &name, query.version.as_deref(), &body)?;
        if deleted_any {
            state.pool.recycle_worker(&name, Some(&version)).await;
        }
        Ok(payload)
    }
    .await;
    match result {
        Ok(payload) => Json(payload).into_response(),
        Err(err) => admin_error(map_error_status(&err), &err, &headers),
    }
}

// EdgeR-owned bundle artifact prefixes (EDG-20). The cache dir name is the
// public constant from edger-isolation; the two legacy per-process tempdir
// prefixes are private there, so the admin surfaces carry them here once.
const LEGACY_BUNDLE_TMP_PREFIX: &str = ".edger-bundle-";
const LEGACY_BRIDGE_TMP_PREFIX: &str = ".edger-bridge-";

/// True when `name` is one of EdgeR's own bundle artifacts (EDG-20) that
/// stays out of the worker file listing and the zip export, at any depth:
/// the per-version bundle cache dir, the legacy per-process bundle tempdirs
/// and the legacy bridge script tempdirs. User dot files (`.env.example`,
/// `.well-known/`, ...) stay visible.
fn is_edger_bundle_artifact(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name == edger_isolation::BUNDLE_CACHE_DIR
        || name.starts_with(LEGACY_BUNDLE_TMP_PREFIX)
        || name.starts_with(LEGACY_BRIDGE_TMP_PREFIX)
}

/// True when ANY segment of the requested sub-path names an EdgeR bundle
/// artifact (EDG-20): the files listing and download answer 404 (as if the
/// path did not exist), at any depth.
fn bundle_artifact_in_sub_path(sub_path: Option<&str>) -> bool {
    sub_path.is_some_and(|path| {
        path.split('/').any(|segment| {
            !segment.is_empty() && is_edger_bundle_artifact(std::ffi::OsStr::new(segment))
        })
    })
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
        if is_edger_bundle_artifact(entry.file_name().as_ref()) {
            continue;
        }
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
        if metadata.len() > MAX_DEPLOY_PACKAGE_BYTES as u64 {
            return Err(download_too_large());
        }
        let bytes = std::fs::read(&target)
            .map_err(|err| CoreError::new("BAD_REQUEST", format!("cannot read file: {err}")))?;
        if bytes.len() > MAX_DEPLOY_PACKAGE_BYTES {
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
    if archive.len() > MAX_DEPLOY_PACKAGE_BYTES {
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
        if is_edger_bundle_artifact(entry.file_name().as_ref()) {
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
        if *total_bytes > MAX_DEPLOY_PACKAGE_BYTES as u64 {
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

async fn authenticate_invoke(
    state: &OrchestratorState,
    headers: &HeaderMap,
) -> Result<ApiKeyPrincipal, CoreError> {
    // Authorization também pode ser a credencial opaca do próprio worker.
    // Só uma sessão real pendente aciona o gate por esse header; a
    // credencial de controle abaixo mantém sua autenticação estrita.
    if matches!(state.auth.must_change_password(headers), Ok(true)) {
        return Err(CoreError::new(
            "PASSWORD_CHANGE_REQUIRED",
            "password change required",
        ));
    }
    if state.auth.is_open() {
        return Ok(root_principal());
    }
    let credential = headers.get(ADMIN_CONTROL_AUTH_HEADER).ok_or_else(|| {
        CoreError::new(
            "UNAUTHORIZED",
            format!("missing {ADMIN_CONTROL_AUTH_HEADER}"),
        )
    })?;
    let mut control_headers = HeaderMap::new();
    control_headers.insert(AUTHORIZATION, credential.clone());
    authenticate(state, &control_headers).await.map_err(|err| {
        if err.code == "UNAUTHORIZED" {
            CoreError::new("UNAUTHORIZED", "invalid control authorization")
        } else {
            err
        }
    })
}

fn download_too_large() -> CoreError {
    CoreError::new(
        "DOWNLOAD_TOO_LARGE",
        format!("download exceeds the {MAX_DEPLOY_PACKAGE_BYTES} byte limit"),
    )
}

pub(crate) async fn authenticate(
    state: &OrchestratorState,
    headers: &HeaderMap,
) -> Result<ApiKeyPrincipal, CoreError> {
    let principal = authenticate_unrestricted(state, headers).await?;
    state.auth.require_password_changed(headers)?;
    Ok(principal)
}

async fn authenticate_unrestricted(
    state: &OrchestratorState,
    headers: &HeaderMap,
) -> Result<ApiKeyPrincipal, CoreError> {
    if state.auth.is_open() {
        return Ok(root_principal());
    }

    if let Some(credential) = extract_api_key(headers)
        .filter(|credential| credential.starts_with(crate::api_keys::API_KEY_PREFIX))
    {
        if state
            .auth
            .root_key_for_internal_clients()
            .is_some_and(|root_key| root_key == credential)
        {
            return Ok(root_principal());
        }
        let Some(keys) = state.auth.key_service() else {
            return Err(CoreError::new("UNAUTHORIZED", "missing or invalid API key"));
        };
        return keys
            .try_authenticate(&credential)?
            .ok_or_else(|| CoreError::new("UNAUTHORIZED", "missing or invalid API key"));
    }

    state
        .auth
        .authenticate_headers(headers)
        .await
        .ok_or_else(|| CoreError::new("UNAUTHORIZED", "missing or invalid API key"))
}

/// D36: export de estado online. Só root; o ZIP é montado sob o plano de
/// mutações (exporting) e o guard é solto ANTES do stream. O arquivo
/// temporário é dono RAII até o stream assumir (`into_owned` após o open):
/// cancelamento/erro antes disso remove o ZIP; no EOF/abort o body apaga o
/// arquivo (sem o teto de 64 MiB do download de package).
async fn state_export_route(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
) -> Response {
    if let Err(err) = require_root(&state, &headers).await {
        return admin_error(map_error_status(&err), &err, &headers);
    }
    let export = match crate::state_export::export_state(
        &state.index,
        state.auth.key_service(),
        state.server.app_secrets(),
        None,
    )
    .await
    {
        Ok(export) => export,
        Err(err) => return admin_error(map_error_status(&err), &err, &headers),
    };
    // Enquanto `export` vive, o ZIP é dono RAII: cancelar aqui (ou com o
    // `File::open` pendendo) ou falhar no open dropa o export e REMOVE o
    // arquivo — sem janela de ZIP órfão.
    let file = match tokio::fs::File::open(&export.path).await {
        Ok(file) => file,
        Err(err) => {
            let error = CoreError::new(
                "STATE_EXPORT_FAILED",
                format!("cannot open exported state file: {err}"),
            );
            return admin_error(map_error_status(&error), &error, &headers);
        }
    };
    // Só depois do open passar a posse passa para o stream: o body apaga o
    // ZIP no EOF/abort (uma única remoção, sem duplicar o owner).
    let (zip_path, filename) = export.into_owned();
    let body = state_export_chunks(StateExportBody {
        file,
        path: zip_path,
    });
    let disposition = format!("attachment; filename=\"{}\"", filename);
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, "application/zip"),
            (CONTENT_DISPOSITION, disposition.as_str()),
            // Backup com hashes de API keys e todos os workers: nunca
            // cacheável (revisão P2).
            (CACHE_CONTROL, "no-store"),
        ],
        axum::body::Body::from_stream(body),
    )
        .into_response()
}

/// Estado do stream do export (D36): assume a posse do ZIP após o
/// `File::open` passar (via `StateExport::into_owned`) e apaga o arquivo no
/// `Drop` — cobre tanto o fim do stream quanto o abandono pelo cliente.
struct StateExportBody {
    file: tokio::fs::File,
    path: std::path::PathBuf,
}

const STATE_EXPORT_CHUNK_BYTES: usize = 64 * 1024;

impl tokio::io::AsyncRead for StateExportBody {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.file).poll_read(cx, buf)
    }
}

impl Drop for StateExportBody {
    fn drop(&mut self) {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let _ = std::fs::remove_file(path);
        });
    }
}

/// Lê `reader` em chunks e devolve o erro de LEITURA como item do stream
/// (o último item): a transferência termina como falha detectável — o hyper
/// aborta o chunked sem o chunk final — e nunca como EOF limpo com o ZIP
/// truncado (revisão P1). O arquivo temporário segue no estado do stream e
/// é apagado no `Drop` de `StateExportBody`.
pub(crate) fn state_export_chunks<R>(
    reader: R,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    R: AsyncRead + Unpin + Send + 'static,
{
    futures_util::stream::unfold(Some(reader), move |reader| async move {
        let mut reader = reader?;
        let mut buffer = vec![0u8; STATE_EXPORT_CHUNK_BYTES];
        match reader.read(&mut buffer).await {
            Ok(0) => None,
            Ok(count) => {
                buffer.truncate(count);
                Some((Ok(Bytes::from(buffer)), Some(reader)))
            }
            Err(err) => {
                tracing::error!("state export stream aborted: {err}");
                Some((Err(err), None))
            }
        }
    })
}

async fn require_user_manager(
    state: &OrchestratorState,
    headers: &HeaderMap,
) -> Result<(ApiKeyPrincipal, ConsoleActor), CoreError> {
    let principal = authenticate(state, headers).await?;
    if !edger_core::security::console_actor_is_manager(&principal) {
        return Err(CoreError::new("FORBIDDEN", "forbidden"));
    }
    let credential = extract_api_key(headers);
    let is_root_key = credential.as_ref().is_some_and(|token| {
        state
            .auth
            .root_key_for_internal_clients()
            .is_some_and(|root| root == *token)
    });
    let actor = console_store(state)?.verified_actor(
        &principal,
        if is_root_key {
            None
        } else {
            credential.as_deref()
        },
    )?;
    Ok((principal, actor))
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

pub(crate) fn require_permission(
    principal: &ApiKeyPrincipal,
    permission: &str,
) -> Result<(), CoreError> {
    if principal_has_permission(principal, permission) {
        Ok(())
    } else {
        Err(CoreError::new(
            "FORBIDDEN",
            format!("permission required: {permission}"),
        ))
    }
}

pub(crate) fn map_error_status(err: &CoreError) -> StatusCode {
    match err.code.as_str() {
        "BAD_REQUEST"
        | "PARSE_ERROR"
        | "VALIDATION_ERROR"
        | "DEPLOY_INVALID_PACKAGE"
        | "DEPLOY_STAGED_REQUIRES_PUBLIC"
        | "DEPLOY_PATH_DENIED"
        | "DEPLOY_VISIBILITY_IMMUTABLE"
        | "CORE_NAME_RESERVED"
        | "PROMOTE_VERSION_REQUIRED"
        | "PROMOTE_INVALID_VERSION"
        | "PROMOTE_STAGED_MARKER_PRESENT"
        | "PROMOTE_INTERNAL_VERSION" => StatusCode::BAD_REQUEST,
        "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
        "NOT_FOUND" => StatusCode::NOT_FOUND,
        "CSRF_DENIED"
        | "PASSWORD_CHANGE_REQUIRED"
        | "FORBIDDEN"
        | "KEY_GRANT_DENIED"
        | "USER_IMMUTABLE"
        | "RELEASE_REQUIRES_ROOT" => StatusCode::FORBIDDEN,
        "STATE_EXPORT_IN_PROGRESS" => StatusCode::CONFLICT,
        "CONSOLE_STORE_UNAVAILABLE"
        | "KEYS_STORE_UNAVAILABLE"
        | "SECRETS_UNAVAILABLE"
        | "STORE_ERROR"
        | "STATE_BUSY"
        | "LOCK_ERROR"
        | "CONSOLE_BUSY" => StatusCode::SERVICE_UNAVAILABLE,
        "RATE_LIMITED" => StatusCode::TOO_MANY_REQUESTS,
        "DOWNLOAD_TOO_LARGE" | "PAYLOAD_TOO_LARGE" => StatusCode::PAYLOAD_TOO_LARGE,
        "USERNAME_TAKEN"
        | "COLLISION"
        | "DEPLOY_REVISION_REQUIRED"
        | "DEPLOY_IN_PROGRESS"
        | "DEPLOY_REVISION_UNKNOWN"
        | "DEPLOY_REVISION_STALE"
        | "DEPLOY_FORCE_TARGET_REQUIRED"
        | "CORE_BUNDLED_IMMUTABLE"
        | "CORE_DEFAULT_REQUIRED"
        | "CORE_WORKER_IMMUTABLE"
        | "DEPLOY_TARGET_EXISTS"
        | "DEPLOY_PUBLIC_VERSION_IMMUTABLE"
        | "DEPLOY_HEALTH_CHECK_FAILED"
        | "HEALTH_CHECK_NOT_CONFIGURED"
        | "KEY_REVOKED"
        | "KEY_NOT_REVOKED"
        | "SECRETS_LIMIT" => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(crate) fn admin_error(status: StatusCode, err: &CoreError, headers: &HeaderMap) -> Response {
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

#[cfg(test)]
mod state_export_stream_tests {
    use super::{state_export_chunks, StateExportBody, STATE_EXPORT_CHUNK_BYTES};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, ReadBuf};

    /// Leitor que entrega exatamente N bytes (em várias leituras) e falha
    /// na leitura seguinte — simula falha de I/O no meio do stream.
    struct FailAfterRead {
        data: Vec<u8>,
        pos: usize,
    }

    impl AsyncRead for FailAfterRead {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.pos >= self.data.len() {
                return Poll::Ready(Err(std::io::Error::other("injected read failure")));
            }
            let remaining = self.data.len() - self.pos;
            let count = remaining.min(buf.remaining());
            buf.put_slice(&self.data[self.pos..self.pos + count]);
            self.pos += count;
            Poll::Ready(Ok(()))
        }
    }

    /// Revisão P1: a falha de leitura aparece como item `Err` do stream (e o
    /// stream termina ali) — nunca como EOF limpo com os N bytes entregues.
    #[tokio::test]
    async fn read_error_surfaces_as_stream_error_not_eof() {
        let total = STATE_EXPORT_CHUNK_BYTES + 5;
        let reader = FailAfterRead {
            data: vec![7u8; total],
            pos: 0,
        };
        let items: Vec<Result<axum::body::Bytes, std::io::Error>> =
            futures_util::StreamExt::collect(state_export_chunks(reader)).await;

        // N bytes (em 2 chunks de 64 KiB + 5) e, depois, um Err — não um fim
        // limpo.
        assert_eq!(items.len(), 3, "itens: {items:?}");
        let (Ok(first), Ok(rest)) = (&items[0], &items[1]) else {
            panic!("os dois primeiros itens deveriam ser chunks Ok: {items:?}");
        };
        assert_eq!(first.len(), STATE_EXPORT_CHUNK_BYTES);
        assert!(first.iter().all(|&byte| byte == 7));
        assert_eq!(rest.len(), 5);
        assert!(rest.iter().all(|&byte| byte == 7));
        match &items[2] {
            Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::Other),
            Ok(_) => panic!("o stream terminou limpo depois da falha: {items:?}"),
        }
    }

    /// Revisão P2 (ciclo de vida): o body assume a posse do ZIP e o `Drop`
    /// apaga o arquivo — cobre EOF, erro e abandono do cliente (o mesmo
    /// caminho de drop).
    #[tokio::test]
    async fn body_drop_removes_zip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("edger-state-test.zip");
        std::fs::write(&zip_path, b"zip-fake").unwrap();

        let file = tokio::fs::File::open(&zip_path).await.unwrap();
        let body = StateExportBody {
            file,
            path: zip_path.clone(),
        };
        // Drop direto: simula EOF/abort antes de o consumidor ler.
        drop(body);

        // O `Drop` agenda a remoção na pool de blocking: espera o arquivo
        // sumir (o `TempDir` ainda existe, só o ZIP precisa ir).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while zip_path.exists() {
            assert!(
                deadline > std::time::Instant::now(),
                "o body não removeu o ZIP"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!zip_path.exists());
    }
}

#[cfg(test)]
mod api_key_auth_tests {
    use super::{authenticate, map_error_status};
    use crate::api_keys::ApiKeyService;
    use crate::auth::ControlAuth;
    use crate::manifest_index_stub::ManifestIndex;
    use crate::pipeline::OrchestratorState;
    use crate::server::ServerState;
    use edger_isolation::MockIsolate;
    use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    struct TestFactory;

    impl IsolateFactory for TestFactory {
        fn create_isolate(
            &self,
            _worker_ref: &edger_core::WorkerRef,
        ) -> Box<dyn edger_core::Isolate> {
            Box::new(MockIsolate::new())
        }
    }

    #[tokio::test]
    async fn locked_api_key_store_waits_then_returns_service_unavailable_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("shared-auth.db");
        let keys = Arc::new(ApiKeyService::open(&db).unwrap());
        let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(TestFactory));
        let server = ServerState::new_unready();
        server.mark_ready(pool.clone());
        let state = OrchestratorState {
            server,
            pool,
            index: ManifestIndex::new(),
            auth: ControlAuth::with_static_key("test-root").with_key_service(keys),
        };
        let lock = rusqlite::Connection::open(&db).unwrap();
        lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-api-key",
            axum::http::HeaderValue::from_static("egk_lock-probe"),
        );
        assert_eq!(
            super::extract_api_key(&headers).as_deref(),
            Some("egk_lock-probe")
        );
        assert!(!state.auth.is_open());
        assert!(state.auth.key_service().is_some());
        assert_ne!(
            state.auth.root_key_for_internal_clients().as_deref(),
            Some("egk_lock-probe")
        );

        let started = Instant::now();
        let error = authenticate(&state, &headers).await.unwrap_err();
        assert_eq!(error.code, "STORE_ERROR");
        assert_eq!(
            map_error_status(&error),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(started.elapsed() >= Duration::from_secs(4));
        lock.execute_batch("ROLLBACK").unwrap();
    }
}

#[cfg(test)]
mod upload_release_gate_tests {
    use super::is_root_manifest_name;
    use super::uploaded_manifest_declares_release;
    use super::uploaded_zip_has_unicode_manifest_alias;
    use std::io::Write;

    fn zip(files: &[(&str, &str)]) -> Vec<u8> {
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

    #[test]
    fn root_manifest_yaml_with_release_is_detected() {
        let bytes = zip(&[
            (
                "manifest.yaml",
                "name: app\nversion: \"1.0.0\"\nrelease: \"migrate\"\n",
            ),
            ("index.ts", "Deno.serve(() => new Response('ok'))"),
        ]);
        assert!(uploaded_manifest_declares_release(&bytes).unwrap());
    }

    #[test]
    fn root_manifest_yml_with_release_is_detected() {
        let bytes = zip(&[("manifest.yml", "name: app\nrelease: migrate")]);
        assert!(uploaded_manifest_declares_release(&bytes).unwrap());
    }

    #[test]
    fn empty_or_blank_release_is_not_declared() {
        for manifest in [
            "name: app\nrelease: \"\"\n",
            "name: app\nrelease: \"   \"\n",
            "name: app\n",
        ] {
            let bytes = zip(&[("manifest.yaml", manifest)]);
            assert!(
                !uploaded_manifest_declares_release(&bytes).unwrap(),
                "manifest {manifest:?} must not count as a release declaration"
            );
        }
    }

    #[test]
    fn nested_manifest_with_release_is_not_the_version_root_manifest() {
        let bytes = zip(&[
            ("sub/manifest.yaml", "name: app\nrelease: migrate\n"),
            ("manifest.yaml", "name: app\n"),
        ]);
        assert!(!uploaded_manifest_declares_release(&bytes).unwrap());
    }

    #[test]
    fn a_manifest_that_does_not_parse_keeps_the_current_behavior() {
        // invalid YAML for WorkerManifest: the check must not fail the upload
        let bytes = zip(&[("manifest.yaml", "version: [unclosed\nrelease: migrate\n")]);
        assert!(!uploaded_manifest_declares_release(&bytes).unwrap());
        // a release field that is not a string does not parse either
        let bytes = zip(&[("manifest.yaml", "name: app\nrelease:\n  a: b\n")]);
        assert!(!uploaded_manifest_declares_release(&bytes).unwrap());
    }

    #[test]
    fn non_manifest_entries_are_ignored() {
        let bytes = zip(&[("notes.txt", "release: migrate\n"), ("index.ts", "x")]);
        assert!(!uploaded_manifest_declares_release(&bytes).unwrap());
    }

    /// Monta um zip com um `manifest.yaml` único e o tamanho não comprimido
    /// do diretório central patched para `claimed_size` (F4: o uploader
    /// controla esse metadado; a recusa tem que olhar o metadado antes de
    /// descomprimir qualquer coisa).
    fn zip_with_patched_size(content: &str, claimed_size: u32) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            writer
                .start_file("manifest.yaml", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content.as_bytes()).unwrap();
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        bytes[central + 24..central + 28].copy_from_slice(&claimed_size.to_le_bytes());
        bytes
    }

    // F4: metadado de manifesto acima do limite é recusado ANTES da leitura,
    // com PAYLOAD_TOO_LARGE (413).
    #[test]
    fn manifest_metadata_over_cap_is_refused_before_read() {
        let bytes = zip_with_patched_size("name: app\n", 2 * 1024 * 1024);
        let err = uploaded_manifest_declares_release(&bytes).unwrap_err();
        assert_eq!(err.code, "PAYLOAD_TOO_LARGE");
        assert_eq!(err.message, "manifest too large");
        assert_eq!(
            super::map_error_status(&err),
            axum::http::StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    // F4: com metadado subdeclarado dentro do limite, conteúdo que passa do
    // cap é recusado no meio da leitura — um erro de leitura/limite nunca
    // autoriza o upload a prosseguir. Desde o zip 9.0.1 (zip-rs/zip2#1022)
    // a própria leitura falha ao passar do tamanho declarado, então a
    // recusa chega como pacote inválido antes do teto duro do EdgeR, que
    // fica como defesa em profundidade.
    #[test]
    fn manifest_read_over_cap_is_refused_even_within_metadata_cap() {
        let content = "x".repeat(super::MAX_MANIFEST_BYTES + 1);
        let bytes = zip_with_patched_size(&content, super::MAX_MANIFEST_BYTES as u32);
        let err = uploaded_manifest_declares_release(&bytes).unwrap_err();
        assert_eq!(err.code, "DEPLOY_INVALID_PACKAGE");
        assert!(
            err.message.starts_with("failed to read manifest entry:"),
            "{}",
            err.message
        );
    }

    // F4: o limite é `>` — um manifesto com exatamente o cap é lido e
    // enviado ao parser (aqui ele não faz parse e segue o comportamento
    // atual), não é recusado como grande demais.
    #[test]
    fn manifest_exactly_at_cap_is_not_refused_as_too_large() {
        let content = "x".repeat(super::MAX_MANIFEST_BYTES);
        let bytes = zip(&[("manifest.yaml", &content)]);
        assert!(!uploaded_manifest_declares_release(&bytes).unwrap());
    }

    /// Monta um zip com várias entradas (nomes duplicados permitidos),
    /// patchando o tamanho não comprimido de CADA diretório central para
    /// o `claimed_size` respectivo (G1/F4: o uploader controla esse
    /// metadado; o orçamento agregado tem que somar os tamanhos declarados
    /// antes de ler qualquer uma).
    fn zip_with_patched_sizes(entries: &[(&str, &str, u32)]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            for (name, content, _) in entries {
                writer
                    .start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(content.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        let mut bytes = cursor.into_inner();
        let mut central = 0usize;
        for (_, _, claimed) in entries {
            // procura a PARTIR do registro atual: cada PK\x01\x02 é um
            // registro de diretório central, em ordem de entrada.
            let offset = bytes[central..]
                .windows(4)
                .position(|w| w == b"PK\x01\x02")
                .expect("central directory record");
            central += offset;
            bytes[central + 24..central + 28].copy_from_slice(&claimed.to_le_bytes());
            central += 4;
        }
        bytes
    }

    // G1: manifestos de raiz duplicados cujo TOTAL declarado passa do
    // orçamento são recusados ANTES de ler qualquer um — o limite por
    // entrada sozinho não basta (257 entradas de ~1 MiB leriam ~269 MB).
    // Os nomes `m{n}/../manifest.yaml` são únicos no zip (o writer rejeita
    // duplicatas exatas) e `enclosed_name` os normaliza de volta para o
    // `manifest.yaml` da raiz — o mesmo truque que um pacote malicioso usa.
    #[test]
    fn aggregate_declared_manifest_budget_is_refused_before_reading() {
        let kib = 1024u32;
        let bytes = zip_with_patched_sizes(&[
            ("m0/../manifest.yaml", "name: app\n", 400 * kib),
            ("m1/../manifest.yaml", "name: app\n", 400 * kib),
            ("m2/../manifest.yaml", "name: app\n", 400 * kib),
        ]);
        let err = uploaded_manifest_declares_release(&bytes).unwrap_err();
        assert_eq!(err.code, "PAYLOAD_TOO_LARGE");
        assert_eq!(err.message, "manifest too large");
        assert_eq!(
            super::map_error_status(&err),
            axum::http::StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    // G1: o orçamento agregado vale também para os bytes REALMENTE lidos —
    // metadado subdeclarado dentro do orçamento não autoriza a leitura
    // somada estourar o teto. Desde o zip 9.0.1 (zip-rs/zip2#1022) a
    // leitura do primeiro candidato já falha ao passar do 1 byte declarado,
    // então a recusa chega como pacote inválido antes do orçamento somado.
    #[test]
    fn aggregate_read_manifest_budget_is_refused_when_metadata_lies() {
        let half_plus_one = super::MAX_MANIFEST_BYTES / 2 + 1;
        let content = "x".repeat(half_plus_one);
        let bytes = zip_with_patched_sizes(&[
            ("manifest.yaml", &content, 1),
            ("manifest.yml", &content, 1),
        ]);
        let err = uploaded_manifest_declares_release(&bytes).unwrap_err();
        assert_eq!(err.code, "DEPLOY_INVALID_PACKAGE");
        assert!(
            err.message.starts_with("failed to read manifest entry:"),
            "{}",
            err.message
        );
    }

    // G1: o orçamento é `>` — candidatos cujo total declarado e lido é
    // exatamente o cap seguem para o parser (aqui sem release).
    #[test]
    fn aggregate_manifest_total_at_cap_is_not_refused() {
        let bytes = zip_with_patched_sizes(&[
            ("manifest.yaml", "name: app\n", 512 * 1024),
            ("manifest.yml", "name: app\n", 512 * 1024),
        ]);
        assert!(!uploaded_manifest_declares_release(&bytes).unwrap());
    }

    // G2: nomes de manifesto sem distinção de caixa — `MANIFEST.YAML` e
    // `Manifest.yml` são o manifesto da raiz em filesystem case-insensitive
    // e a recusa tem que cobri-los em todos.
    #[test]
    fn manifest_name_case_variants_are_candidates() {
        let upper = zip(&[(
            "MANIFEST.YAML",
            "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nrelease: \"true\"\n",
        )]);
        assert!(uploaded_manifest_declares_release(&upper).unwrap());
        let mixed = zip(&[(
            "Manifest.yml",
            "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nrelease: \"true\"\n",
        )]);
        assert!(uploaded_manifest_declares_release(&mixed).unwrap());
    }

    // Emenda 3: o alias Unicode do nome de manifesto é detectado pela
    // defesa extra (não root) — e um nome ASCII exato não é alias.
    #[test]
    fn unicode_manifest_alias_is_detected_by_the_extra_defense() {
        let alias = zip(&[(
            "manife\u{17F}t.yaml",
            "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
        )]);
        assert!(uploaded_zip_has_unicode_manifest_alias(&alias).unwrap());
        let ascii = zip(&[(
            "manifest.yaml",
            "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\n",
        )]);
        assert!(!uploaded_zip_has_unicode_manifest_alias(&ascii).unwrap());
        // o alias também é candidato da detecção de release
        let alias_release = zip(&[(
            "manife\u{17F}t.yaml",
            "name: app\nversion: \"1.0.0\"\nentrypoint: index.ts\nkind: fetch\nrelease: \"true\"\n",
        )]);
        assert!(uploaded_manifest_declares_release(&alias_release).unwrap());
        assert!(is_root_manifest_name("manife\u{17F}t.yaml"));
        assert!(!is_root_manifest_name("notes.yaml"));
    }
}

#[cfg(test)]
mod cron_jobs_list_tests {
    use super::cron_job_entries;
    use crate::cron::{CronJobInfo, CronJobState};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn job(worker: &str, path: &str) -> CronJobInfo {
        CronJobInfo {
            worker: worker.into(),
            schedule: "@every 1h".into(),
            path: path.into(),
            method: "GET".into(),
            state: CronJobState::Active,
            next_run_at: None,
        }
    }

    /// (EDG-34 round 4, F1) The listing resolves the version of each
    /// worker name at most ONCE per request: with 2_000 jobs of one
    /// worker (the F1 shape) the resolver is called once for the name —
    /// never once per job — and every entry gets the version.
    #[test]
    fn cron_job_entries_resolve_each_worker_name_once_per_request() {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let resolve = move |name: &str| {
            counter.fetch_add(1, Ordering::SeqCst);
            (name != "gone").then(|| "1.0.0".to_owned())
        };
        let jobs: Vec<CronJobInfo> = (0..2000).map(|i| job("many", &format!("/p{i}"))).collect();
        let entries = cron_job_entries(jobs, |name| name == "many", resolve);
        assert_eq!(entries.len(), 2000);
        assert!(entries.iter().all(|e| e["version"] == "1.0.0"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one resolution per NAME per request, not per job"
        );
    }

    /// (EDG-34 round 4, F1) The scope filter runs BEFORE the resolution:
    /// a name out of scope is never resolved at all; and a name in scope
    /// that does not resolve renders `version: null` from the CACHED
    /// value (the `None` is memoized like the versions).
    #[test]
    fn cron_job_entries_skip_resolution_for_names_out_of_scope() {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let resolve = move |name: &str| {
            counter.fetch_add(1, Ordering::SeqCst);
            (name != "gone").then(|| "1.0.0".to_owned())
        };
        let jobs = vec![
            job("in", "/a"),
            job("in", "/b"),
            job("gone", "/a"),
            job("gone", "/b"),
        ];
        let entries = cron_job_entries(jobs, |name| name == "in", resolve);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e["worker"] == "in"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the out-of-scope name is never resolved"
        );

        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let entries = cron_job_entries(
            vec![job("gone", "/a"), job("gone", "/b")],
            |_| true,
            move |name: &str| {
                counter.fetch_add(1, Ordering::SeqCst);
                (name != "gone").then(|| "1.0.0".to_owned())
            },
        );
        assert!(entries.iter().all(|e| e["version"].is_null()));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the `None` is cached too");
    }
}

#[cfg(test)]
mod upload_rollback_tests {
    use super::{
        apply_staged_upload, open_upload_base, split_upload_path, upload_temp_dir,
        UPLOAD_AFTER_BACKUP, UPLOAD_AFTER_DIR_CHECK, UPLOAD_BEFORE_BACKUP,
        UPLOAD_BEFORE_OBSTACLE_ERROR, UPLOAD_BEFORE_PROBE, UPLOAD_BEFORE_PUBLISH,
        UPLOAD_FAIL_HARD_LINK, UPLOAD_FAULT_AFTER, UPLOAD_TEMP_PREFIX,
    };
    use crate::deploy::ExtractedEntry;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    // Relative path -> type and contents. A link records its target and a
    // special file (FIFO) only its type: neither is followed nor opened.
    fn tree(root: &Path) -> BTreeMap<PathBuf, String> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                let file_type = fs::symlink_metadata(&path).unwrap().file_type();
                if file_type.is_symlink() {
                    let target = fs::read_link(&path).unwrap();
                    out.insert(relative, format!("link:{}", target.display()));
                } else if file_type.is_dir() {
                    out.insert(relative, "dir".into());
                    walk(root, &path, out);
                } else if file_type.is_file() {
                    let contents = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
                    out.insert(relative, format!("file:{contents}"));
                } else {
                    out.insert(relative, "special".into());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    // The descriptor phase 2 starts from, as `split_upload_path` opens it.
    fn version_dir(base: &Path) -> std::os::fd::OwnedFd {
        open_upload_base(base).unwrap()
    }

    fn file(relative: &str) -> ExtractedEntry {
        ExtractedEntry {
            relative: PathBuf::from(relative),
            is_dir: false,
        }
    }

    // Mutation captured: skipping `rollback_upload_steps` leaves `new/`,
    // `new/inner.txt` and the new `over.txt` behind and this test goes red.
    #[test]
    fn a_fault_mid_apply_restores_backups_and_removes_what_was_created() {
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().join("app");
        fs::create_dir_all(base.join("kept")).unwrap();
        fs::write(base.join("kept/old.txt"), b"old").unwrap();
        fs::write(base.join("over.txt"), b"before").unwrap();
        let before = tree(&base);

        let staging = tempfile::tempdir_in(parent.path()).unwrap();
        fs::create_dir_all(staging.path().join("new")).unwrap();
        fs::write(staging.path().join("new/inner.txt"), b"inner").unwrap();
        fs::write(staging.path().join("over.txt"), b"after").unwrap();
        fs::write(staging.path().join("z.txt"), b"z").unwrap();
        let entries = [file("new/inner.txt"), file("over.txt"), file("z.txt")];
        let backup = tempfile::Builder::new()
            .prefix(UPLOAD_TEMP_PREFIX)
            .tempdir_in(parent.path())
            .unwrap();
        let backup_path = backup.path().to_path_buf();

        // Steps: created `new/`, created `new/inner.txt`, replaced
        // `over.txt` -> the fault hits before `z.txt`.
        UPLOAD_FAULT_AFTER.with(|fault| fault.set(Some(3)));
        let result = apply_staged_upload(
            &version_dir(&base),
            Path::new(""),
            Path::new(""),
            staging.path(),
            &entries,
            backup,
        );
        UPLOAD_FAULT_AFTER.with(|fault| fault.set(None));

        let error = result.expect_err("the injected fault must surface");
        assert_eq!(error.code, "DEPLOY_IO");
        assert!(error.message.contains("injected upload fault"), "{error:?}");
        assert_eq!(tree(&base), before);
        assert_eq!(fs::read(base.join("over.txt")).unwrap(), b"before");
        assert!(!base.join("new").exists());
        // A clean rollback lets the backup TempDir go.
        assert!(!backup_path.exists());
    }

    // EDG-39 review F1: a rescan (which does not take the slot) running right
    // after the backup of a `manifest.yaml` overwrite still sees the manifest,
    // so the version keeps its identity in the index.
    // Mutation captured: going back to `rename(target, backup)` hides the
    // manifest from that rescan, which swaps `review-app@1.0.0` for the
    // inferred `review-app@latest`, and this test goes red.
    #[test]
    fn a_rescan_right_after_the_backup_keeps_the_version_identity() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("review-app");
        fs::create_dir_all(&base).unwrap();
        let manifest = "name: review-app\nversion: '1.0.0'\nentrypoint: index.ts\nkind: fetch\n";
        fs::write(base.join("manifest.yaml"), manifest).unwrap();
        fs::write(
            base.join("index.ts"),
            "Deno.serve(() => new Response('ok'));",
        )
        .unwrap();
        let index =
            crate::manifest_loader::load_manifests_from_dirs(&[root.path().to_path_buf()]).unwrap();
        let identity = |index: &crate::ManifestIndex| {
            index
                .admin_workers()
                .into_iter()
                .map(|worker| format!("{}@{}", worker.name, worker.version))
                .collect::<Vec<_>>()
        };
        assert_eq!(identity(&index), ["review-app@1.0.0"]);
        let _slot =
            crate::deploy::claim_worker_mutation_slot(root.path(), "review-app", "1.0.0").unwrap();
        let scanned = index.clone();
        let report = std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen = report.clone();
        UPLOAD_AFTER_BACKUP.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                *seen.borrow_mut() = Some(crate::deploy::rescan_workers(&scanned, false).unwrap());
            }))
        });
        let staging = upload_temp_dir(root.path()).unwrap();
        fs::write(staging.path().join("manifest.yaml"), manifest).unwrap();
        let backup = upload_temp_dir(root.path()).unwrap();
        let result = apply_staged_upload(
            &version_dir(&base),
            Path::new(""),
            Path::new(""),
            staging.path(),
            &[file("manifest.yaml")],
            backup,
        );
        UPLOAD_AFTER_BACKUP.with(|hook| hook.borrow_mut().take());
        result.unwrap();

        let report = report
            .borrow_mut()
            .take()
            .expect("the rescan ran mid-apply");
        assert!(report.removed.is_empty(), "removed: {:?}", report.removed);
        assert!(report.added.is_empty(), "added: {:?}", report.added);
        assert_eq!(identity(&index), ["review-app@1.0.0"]);
        assert!(base.join("manifest.yaml").is_file());
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn inode(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).unwrap().ino()
    }

    // EDG-39 review R1: a FIFO target (or, EDG-40, a link to one) is refused
    // at once, without being opened — the old open for writing waited for a
    // reader.
    // Mutation captured: dropping the type check makes the probe block on the
    // FIFO and the `recv_timeout` below fails the test instead of hanging.
    #[cfg(unix)]
    #[test]
    fn a_fifo_target_is_refused_without_blocking() {
        for name in ["pipe", "pipe-link"] {
            let parent = tempfile::tempdir().unwrap();
            let base = parent.path().join("app");
            fs::create_dir_all(&base).unwrap();
            let status = std::process::Command::new("mkfifo")
                .arg(base.join("pipe"))
                .status()
                .unwrap();
            assert!(status.success(), "mkfifo failed");
            std::os::unix::fs::symlink("pipe", base.join("pipe-link")).unwrap();
            let before = tree(&base);
            let staging = tempfile::tempdir_in(parent.path()).unwrap();
            fs::write(staging.path().join("fresh.txt"), b"fresh").unwrap();
            fs::write(staging.path().join(name), b"new").unwrap();

            let (sender, receiver) = std::sync::mpsc::channel();
            let (thread_base, staging_path) = (base.clone(), staging.path().to_path_buf());
            let backup = tempfile::tempdir_in(parent.path()).unwrap();
            let entries = [file("fresh.txt"), file(name)];
            std::thread::spawn(move || {
                let result = apply_staged_upload(
                    &version_dir(&thread_base),
                    Path::new(""),
                    Path::new(""),
                    &staging_path,
                    &entries,
                    backup,
                );
                let _ = sender.send(result);
            });
            let result = receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("{name}: the upload blocked on the FIFO"));
            let error = result.expect_err(name);
            // EDG-40: the link itself is refused before its FIFO is reached.
            let (code, message) = match name {
                "pipe" => ("DEPLOY_IO", "failed to write file: not a regular file"),
                _ => (
                    "DEPLOY_PATH_DENIED",
                    "zip entry crosses a symbolic link: pipe-link",
                ),
            };
            assert_eq!(error.code, code, "{name}");
            assert_eq!(error.message, message, "{name}");
            assert_eq!(tree(&base), before, "{name}");
        }
    }

    // EDG-39 review R2: a failure between the backup and the publication
    // leaves the target untouched — same inode, contents, mode and links —
    // with the hard-link backup (two links) and with the copy (one link).
    // Mutation captured: recording `Replaced` before the publication makes
    // the rollback put the backup over the untouched target; with the copy
    // that is another inode and this test goes red.
    #[cfg(unix)]
    #[test]
    fn a_failure_before_publication_leaves_the_target_untouched() {
        for force_copy in [false, true] {
            let parent = tempfile::tempdir().unwrap();
            let base = parent.path().join("app");
            fs::create_dir_all(&base).unwrap();
            fs::write(base.join("a"), b"old").unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(base.join("a"), fs::Permissions::from_mode(0o751)).unwrap();
            }
            if !force_copy {
                fs::hard_link(base.join("a"), base.join("b")).unwrap();
            }
            let inode_before = inode(&base.join("a"));
            let staging = tempfile::tempdir_in(parent.path()).unwrap();
            fs::write(staging.path().join("a"), b"new").unwrap();
            let staged = staging.path().join("a");
            // The publication rename fails: its source is gone.
            UPLOAD_AFTER_BACKUP.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || fs::remove_file(staged).unwrap()))
            });
            UPLOAD_FAIL_HARD_LINK.with(|fail| fail.set(force_copy));
            let backup = tempfile::tempdir_in(parent.path()).unwrap();
            let backup_path = backup.path().to_path_buf();
            let result = apply_staged_upload(
                &version_dir(&base),
                Path::new(""),
                Path::new(""),
                staging.path(),
                &[file("a")],
                backup,
            );
            UPLOAD_FAIL_HARD_LINK.with(|fail| fail.set(false));
            UPLOAD_AFTER_BACKUP.with(|hook| hook.borrow_mut().take());

            let error = result.expect_err("the publication rename fails");
            assert_eq!(error.code, "DEPLOY_IO");
            assert_eq!(fs::read(base.join("a")).unwrap(), b"old");
            assert_eq!(inode(&base.join("a")), inode_before, "copy={force_copy}");
            assert_eq!(mode(&base.join("a")), 0o751);
            assert!(!backup_path.exists(), "a clean rollback drops the backup");
            if !force_copy {
                fs::write(base.join("b"), b"consumer update").unwrap();
                assert_eq!(fs::read(base.join("a")).unwrap(), b"consumer update");
            }
        }
    }

    // EDG-39 review R2: with two links and the hard link failing, a copy
    // could not keep `b` tied to `a` through a rollback: the entry is refused
    // cleanly with the hard-link error and both links stay tied.
    // Mutation captured: falling back to the copy regardless of `nlink`
    // turns this refusal into a success and the test goes red.
    #[cfg(unix)]
    #[test]
    fn a_failed_hard_link_on_a_multiply_linked_file_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().join("app");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("a"), b"old").unwrap();
        fs::hard_link(base.join("a"), base.join("b")).unwrap();
        let before = tree(&base);
        let inode_before = inode(&base.join("a"));
        let staging = tempfile::tempdir_in(parent.path()).unwrap();
        fs::write(staging.path().join("fresh.txt"), b"fresh").unwrap();
        fs::write(staging.path().join("a"), b"new").unwrap();
        let backup = tempfile::tempdir_in(parent.path()).unwrap();

        UPLOAD_FAIL_HARD_LINK.with(|fail| fail.set(true));
        let result = apply_staged_upload(
            &version_dir(&base),
            Path::new(""),
            Path::new(""),
            staging.path(),
            &[file("fresh.txt"), file("a")],
            backup,
        );
        UPLOAD_FAIL_HARD_LINK.with(|fail| fail.set(false));

        let error = result.expect_err("a copy cannot keep the links");
        assert_eq!(error.code, "DEPLOY_IO");
        assert_eq!(
            error.message,
            "failed to write file: injected hard link failure"
        );
        assert_eq!(tree(&base), before);
        assert_eq!(inode(&base.join("a")), inode_before);
        fs::write(base.join("b"), b"consumer update").unwrap();
        assert_eq!(fs::read(base.join("a")).unwrap(), b"consumer update");
    }

    // EDG-39 review R2: a single-link file backed up by copy and rolled back
    // after its publication gets its bytes and mode back; with the hard link
    // (two links) the rollback also restores the inode, so `b` stays tied.
    #[cfg(unix)]
    #[test]
    fn a_rollback_after_publication_restores_bytes_mode_and_links() {
        for force_copy in [true, false] {
            let parent = tempfile::tempdir().unwrap();
            let base = parent.path().join("app");
            fs::create_dir_all(&base).unwrap();
            fs::write(base.join("a"), b"old").unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(base.join("a"), fs::Permissions::from_mode(0o751)).unwrap();
            }
            if !force_copy {
                fs::hard_link(base.join("a"), base.join("b")).unwrap();
            }
            let inode_before = inode(&base.join("a"));
            let staging = tempfile::tempdir_in(parent.path()).unwrap();
            fs::write(staging.path().join("a"), b"new").unwrap();
            fs::write(staging.path().join("z"), b"z").unwrap();
            let backup = tempfile::tempdir_in(parent.path()).unwrap();

            // Step 1 is the published `a`; the fault hits before `z`.
            UPLOAD_FAIL_HARD_LINK.with(|fail| fail.set(force_copy));
            UPLOAD_FAULT_AFTER.with(|fault| fault.set(Some(1)));
            let result = apply_staged_upload(
                &version_dir(&base),
                Path::new(""),
                Path::new(""),
                staging.path(),
                &[file("a"), file("z")],
                backup,
            );
            UPLOAD_FAULT_AFTER.with(|fault| fault.set(None));
            UPLOAD_FAIL_HARD_LINK.with(|fail| fail.set(false));

            assert!(result.is_err());
            assert_eq!(
                fs::read(base.join("a")).unwrap(),
                b"old",
                "copy={force_copy}"
            );
            assert_eq!(mode(&base.join("a")), 0o751, "copy={force_copy}");
            assert!(!base.join("z").exists());
            if !force_copy {
                assert_eq!(inode(&base.join("a")), inode_before);
                assert_eq!(inode(&base.join("b")), inode_before);
            }
        }
    }

    // EDG-39 review R3: an obstacle in `?path=` that disappears right before
    // its error is selected never makes the upload create anything outside
    // the record: the path check writes nothing, the segment is created in
    // phase 2 (and undone by a rollback), or the upload succeeds.
    // Mutation captured: selecting the error with `create_dir_all` (the old
    // helper) creates `taken/nested` during the check and this test goes red.
    #[test]
    fn a_path_obstacle_removed_in_a_race_leaves_no_unrecorded_dir() {
        for fault in [Some(1), None] {
            let parent = tempfile::tempdir().unwrap();
            let base = parent.path().join("app");
            fs::create_dir_all(&base).unwrap();
            fs::write(base.join("taken"), b"obstacle").unwrap();
            let base = fs::canonicalize(&base).unwrap();
            let obstacle = base.join("taken");
            UPLOAD_BEFORE_OBSTACLE_ERROR.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || fs::remove_file(obstacle).unwrap()))
            });
            let split = split_upload_path(&base, "taken/nested");
            UPLOAD_BEFORE_OBSTACLE_ERROR.with(|hook| hook.borrow_mut().take());
            let (existing_dir, existing, missing) = split.unwrap();
            assert_eq!(existing, Path::new(""));
            assert_eq!(missing, Path::new("taken/nested"));
            assert!(!base.join("taken").exists(), "the path check wrote nothing");

            let staging = tempfile::tempdir_in(parent.path()).unwrap();
            fs::write(staging.path().join("fresh.txt"), b"fresh").unwrap();
            let backup = tempfile::tempdir_in(parent.path()).unwrap();
            UPLOAD_FAULT_AFTER.with(|cell| cell.set(fault));
            let result = apply_staged_upload(
                &existing_dir,
                &existing,
                &missing,
                staging.path(),
                &[file("fresh.txt")],
                backup,
            );
            UPLOAD_FAULT_AFTER.with(|cell| cell.set(None));
            match fault {
                Some(_) => {
                    assert!(result.is_err());
                    assert_eq!(tree(&base), BTreeMap::new());
                }
                None => {
                    result.unwrap();
                    assert_eq!(
                        fs::read(base.join("taken/nested/fresh.txt")).unwrap(),
                        b"fresh"
                    );
                }
            }
        }
    }

    // EDG-39 review R3, phase 2: an entry whose parent is a file that
    // disappears right before its error is selected is created level by
    // level through the record (a later fault removes it again).
    #[test]
    fn an_entry_obstacle_removed_in_a_race_is_created_through_the_record() {
        for fault in [Some(1), None] {
            let parent = tempfile::tempdir().unwrap();
            let base = parent.path().join("app");
            fs::create_dir_all(&base).unwrap();
            fs::write(base.join("x"), b"obstacle").unwrap();
            let obstacle = base.join("x");
            let staging = tempfile::tempdir_in(parent.path()).unwrap();
            fs::create_dir_all(staging.path().join("x")).unwrap();
            fs::write(staging.path().join("x/y.txt"), b"y").unwrap();
            let backup = tempfile::tempdir_in(parent.path()).unwrap();
            UPLOAD_BEFORE_OBSTACLE_ERROR.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || fs::remove_file(obstacle).unwrap()))
            });
            UPLOAD_FAULT_AFTER.with(|cell| cell.set(fault));
            let result = apply_staged_upload(
                &version_dir(&base),
                Path::new(""),
                Path::new(""),
                staging.path(),
                &[file("x/y.txt")],
                backup,
            );
            UPLOAD_FAULT_AFTER.with(|cell| cell.set(None));
            UPLOAD_BEFORE_OBSTACLE_ERROR.with(|hook| hook.borrow_mut().take());
            match fault {
                Some(_) => {
                    assert!(result.is_err());
                    assert_eq!(tree(&base), BTreeMap::new());
                }
                None => {
                    result.unwrap();
                    assert_eq!(fs::read(base.join("x/y.txt")).unwrap(), b"y");
                }
            }
        }
    }

    // Control: without the fault the same plan applies in full.
    #[test]
    fn without_a_fault_the_staged_entries_apply() {
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().join("app");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("over.txt"), b"before").unwrap();
        let staging = tempfile::tempdir_in(parent.path()).unwrap();
        fs::create_dir_all(staging.path().join("new")).unwrap();
        fs::write(staging.path().join("new/inner.txt"), b"inner").unwrap();
        fs::write(staging.path().join("over.txt"), b"after").unwrap();
        let entries = [file("new/inner.txt"), file("over.txt")];
        let backup = tempfile::tempdir_in(parent.path()).unwrap();

        apply_staged_upload(
            &version_dir(&base),
            Path::new(""),
            Path::new(""),
            staging.path(),
            &entries,
            backup,
        )
        .unwrap();
        assert_eq!(fs::read(base.join("over.txt")).unwrap(), b"after");
        assert_eq!(fs::read(base.join("new/inner.txt")).unwrap(), b"inner");
    }

    // EDG-40: an intermediate directory swapped for a link to an outside dir
    // in a race never makes the upload write outside. Swapped after the walk
    // checked it and before it opened it, the `O_NOFOLLOW` open fails
    // (`ELOOP`): 400 DEPLOY_PATH_DENIED and the earlier entry is rolled back.
    // Swapped after it was opened and right before the publication, the
    // `renameat` is relative to the descriptor already held, so the file is
    // published inside the original directory (now `sub-orig`).
    // Mutation captured: opening the directory without `O_NOFOLLOW` follows
    // the swapped link, publishes `x.txt` in `outside` and this test goes red.
    #[cfg(unix)]
    #[test]
    fn a_directory_swapped_for_a_link_in_a_race_never_writes_outside() {
        for (after_open, replace) in [(false, false), (false, true), (true, false), (true, true)] {
            let case = format!("after_open={after_open} replace={replace}");
            let parent = tempfile::tempdir().unwrap();
            let base = parent.path().join("app");
            let outside = parent.path().join("outside");
            fs::create_dir_all(base.join("sub")).unwrap();
            fs::create_dir_all(&outside).unwrap();
            fs::write(base.join("kept.txt"), b"kept").unwrap();
            if replace {
                fs::write(base.join("sub/x.txt"), b"old").unwrap();
            }
            let original_sub = tree(&base.join("sub"));
            let staging = tempfile::tempdir_in(parent.path()).unwrap();
            fs::create_dir_all(staging.path().join("sub")).unwrap();
            fs::write(staging.path().join("fresh.txt"), b"fresh").unwrap();
            fs::write(staging.path().join("sub/x.txt"), b"new").unwrap();
            let backup = tempfile::tempdir_in(parent.path()).unwrap();

            let (hook_base, hook_outside) = (base.clone(), outside.clone());
            let swap: Box<dyn FnOnce()> = Box::new(move || {
                fs::rename(hook_base.join("sub"), hook_base.join("sub-orig")).unwrap();
                std::os::unix::fs::symlink(&hook_outside, hook_base.join("sub")).unwrap();
            });
            if after_open {
                UPLOAD_BEFORE_PUBLISH.with(|hook| *hook.borrow_mut() = Some(swap));
            } else {
                UPLOAD_AFTER_DIR_CHECK.with(|hook| *hook.borrow_mut() = Some(swap));
            }
            // The publication hook fires on the first publication: `sub/x.txt`
            // goes first there; the check hook needs `fresh.txt` applied
            // before, to show its rollback.
            let entries = if after_open {
                [file("sub/x.txt"), file("fresh.txt")]
            } else {
                [file("fresh.txt"), file("sub/x.txt")]
            };
            let result = apply_staged_upload(
                &version_dir(&base),
                Path::new(""),
                Path::new(""),
                staging.path(),
                &entries,
                backup,
            );
            let hook_ran = UPLOAD_BEFORE_PUBLISH
                .with(|hook| hook.borrow_mut().take())
                .is_none()
                && UPLOAD_AFTER_DIR_CHECK
                    .with(|hook| hook.borrow_mut().take())
                    .is_none();
            assert!(hook_ran, "{case}: the swap ran");

            assert_eq!(tree(&outside), BTreeMap::new(), "{case}: nothing outside");
            assert_eq!(
                fs::read_link(base.join("sub")).unwrap(),
                outside,
                "{case}: the link itself is untouched"
            );
            if after_open {
                result.unwrap_or_else(|err| panic!("{case}: {err:?}"));
                assert_eq!(fs::read(base.join("sub-orig/x.txt")).unwrap(), b"new");
                assert_eq!(fs::read(base.join("fresh.txt")).unwrap(), b"fresh");
            } else {
                let error = result.expect_err(&case);
                assert_eq!(error.code, "DEPLOY_PATH_DENIED", "{case}");
                assert_eq!(
                    error.message, "zip entry crosses a symbolic link: sub/x.txt",
                    "{case}"
                );
                assert_eq!(tree(&base.join("sub-orig")), original_sub, "{case}");
                assert!(!base.join("fresh.txt").exists(), "{case}: rolled back");
                assert_eq!(fs::read(base.join("kept.txt")).unwrap(), b"kept");
            }
        }
    }

    // EDG-40: a version dir swapped for a link since `resolve_worker_dir`
    // canonicalized it is refused by the `O_NOFOLLOW` open of the base, and
    // nothing is written through it.
    // Mutation captured: opening the base without `O_NOFOLLOW` accepts the
    // link and this test goes red.
    #[cfg(unix)]
    #[test]
    fn a_version_dir_swapped_for_a_link_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let real = parent.path().join("real");
        fs::create_dir(&real).unwrap();
        let alias = parent.path().join("app");
        std::os::unix::fs::symlink(&real, &alias).unwrap();

        let error = split_upload_path(&alias, "").expect_err("the base is a link");
        assert_eq!(error.code, "NOT_FOUND");
        assert!(
            error.message.starts_with("worker directory unavailable: "),
            "{error:?}"
        );
        assert_eq!(tree(&real), BTreeMap::new());
        assert!(fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    // Plants `<base>/<name>` as a link to `<outside>/<name>` from a hook.
    #[cfg(unix)]
    fn swap_for_outside_link(base: &Path, outside: &Path, name: &str) -> Box<dyn FnOnce()> {
        let (target, link) = (outside.join(name), base.join(name));
        Box::new(move || {
            fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink(&target, &link).unwrap();
        })
    }

    // A version `app` with `a` = "old" and an outside dir with `a` =
    // "outside", plus a staging dir holding `fresh.txt`, `a` and `z`.
    #[cfg(unix)]
    struct SwapFixture {
        parent: tempfile::TempDir,
        base: PathBuf,
        outside: PathBuf,
        staging: tempfile::TempDir,
    }

    #[cfg(unix)]
    fn swap_fixture() -> SwapFixture {
        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().join("app");
        let outside = parent.path().join("outside");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(base.join("a"), b"old").unwrap();
        fs::write(outside.join("a"), b"outside").unwrap();
        let staging = tempfile::tempdir_in(parent.path()).unwrap();
        fs::write(staging.path().join("fresh.txt"), b"fresh").unwrap();
        fs::write(staging.path().join("a"), b"new").unwrap();
        fs::write(staging.path().join("z"), b"z").unwrap();
        SwapFixture {
            parent,
            base,
            outside,
            staging,
        }
    }

    #[cfg(unix)]
    fn assert_link_kept(fixture: &SwapFixture, case: &str) {
        assert_eq!(
            fs::read_link(fixture.base.join("a")).unwrap(),
            fixture.outside.join("a"),
            "{case}: the link entry is back in place"
        );
        assert_eq!(
            fs::read(fixture.outside.join("a")).unwrap(),
            b"outside",
            "{case}: the outside bytes are untouched"
        );
        assert_eq!(
            std::os::unix::fs::MetadataExt::nlink(
                &fs::metadata(fixture.outside.join("a")).unwrap()
            ),
            1,
            "{case}: no link to the outside file was taken"
        );
        assert!(
            !fixture.base.join("fresh.txt").exists(),
            "{case}: rolled back"
        );
    }

    // EDG-40: the final target checked as a regular file and swapped for a
    // link to an outside file right before the write probe: the probe's
    // `O_NOFOLLOW` open refuses it (400 DEPLOY_PATH_DENIED), the earlier entry
    // is rolled back and the outside file keeps its bytes.
    // Mutation captured: probing without `O_NOFOLLOW` opens the outside file,
    // the upload succeeds and this test goes red.
    #[cfg(unix)]
    #[test]
    fn a_target_swapped_for_a_link_before_the_probe_is_refused() {
        let fixture = swap_fixture();
        let swap = swap_for_outside_link(&fixture.base, &fixture.outside, "a");
        UPLOAD_BEFORE_PROBE.with(|hook| *hook.borrow_mut() = Some(swap));
        let backup = tempfile::tempdir_in(fixture.parent.path()).unwrap();
        let result = apply_staged_upload(
            &version_dir(&fixture.base),
            Path::new(""),
            Path::new(""),
            fixture.staging.path(),
            &[file("fresh.txt"), file("a")],
            backup,
        );
        let ran = UPLOAD_BEFORE_PROBE
            .with(|hook| hook.borrow_mut().take())
            .is_none();
        assert!(ran, "the swap ran");

        let error = result.expect_err("a link target is refused");
        assert_eq!(error.code, "DEPLOY_PATH_DENIED");
        assert_eq!(error.message, "zip entry crosses a symbolic link: a");
        assert_link_kept(&fixture, "probe");
    }

    // EDG-40: with the hard link failing, the copy fallback of the backup
    // reads the target with `O_NOFOLLOW`: a target swapped for a link to an
    // outside file right before the backup is refused, nothing of the
    // outside file is copied, and the rollback is clean (no backup kept).
    // Mutation captured: opening the copy source without `O_NOFOLLOW` copies
    // the outside file, the upload succeeds and this test goes red.
    #[cfg(unix)]
    #[test]
    fn a_target_swapped_for_a_link_before_the_copy_backup_is_refused() {
        let fixture = swap_fixture();
        let swap = swap_for_outside_link(&fixture.base, &fixture.outside, "a");
        UPLOAD_BEFORE_BACKUP.with(|hook| *hook.borrow_mut() = Some(swap));
        UPLOAD_FAIL_HARD_LINK.with(|fail| fail.set(true));
        let backup = tempfile::tempdir_in(fixture.parent.path()).unwrap();
        let backup_path = backup.path().to_path_buf();
        let result = apply_staged_upload(
            &version_dir(&fixture.base),
            Path::new(""),
            Path::new(""),
            fixture.staging.path(),
            &[file("fresh.txt"), file("a")],
            backup,
        );
        UPLOAD_FAIL_HARD_LINK.with(|fail| fail.set(false));
        let ran = UPLOAD_BEFORE_BACKUP
            .with(|hook| hook.borrow_mut().take())
            .is_none();
        assert!(ran, "the swap ran");

        let error = result.expect_err("a link target is refused");
        assert_eq!(error.code, "DEPLOY_PATH_DENIED");
        assert_eq!(error.message, "zip entry crosses a symbolic link: a");
        assert_link_kept(&fixture, "copy");
        assert!(!backup_path.exists(), "a clean rollback drops the backup");
    }

    // EDG-40: the backup hard link is taken with `linkat` WITHOUT
    // `AT_SYMLINK_FOLLOW`: a target swapped for a link to an outside file
    // right before the backup is backed up as that link entry itself (the
    // backup holds the link, never a second name of the outside file), and
    // a later fault puts the link entry back. The outside file keeps its
    // bytes and its single name.
    // Mutation captured: `linkat` with `AT_SYMLINK_FOLLOW` backs up the
    // outside file (a regular file with two names), the rollback renames it
    // over `a` and this test goes red.
    #[cfg(unix)]
    #[test]
    fn a_target_swapped_for_a_link_before_the_hard_link_backup_backs_up_the_link() {
        let fixture = swap_fixture();
        let swap = swap_for_outside_link(&fixture.base, &fixture.outside, "a");
        UPLOAD_BEFORE_BACKUP.with(|hook| *hook.borrow_mut() = Some(swap));
        let backup = tempfile::tempdir_in(fixture.parent.path()).unwrap();
        let backup_entry = backup.path().join("0");
        let seen = std::rc::Rc::new(std::cell::RefCell::new(None));
        let record = seen.clone();
        UPLOAD_AFTER_BACKUP.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                *record.borrow_mut() = Some(fs::read_link(&backup_entry).ok());
            }))
        });
        // Step 1 is the published `a`; the fault hits before `z`.
        UPLOAD_FAULT_AFTER.with(|fault| fault.set(Some(1)));
        let result = apply_staged_upload(
            &version_dir(&fixture.base),
            Path::new(""),
            Path::new(""),
            fixture.staging.path(),
            &[file("a"), file("z")],
            backup,
        );
        UPLOAD_FAULT_AFTER.with(|fault| fault.set(None));
        UPLOAD_AFTER_BACKUP.with(|hook| hook.borrow_mut().take());
        let ran = UPLOAD_BEFORE_BACKUP
            .with(|hook| hook.borrow_mut().take())
            .is_none();
        assert!(ran, "the swap ran");

        let error = result.expect_err("the injected fault must surface");
        assert!(error.message.contains("injected upload fault"), "{error:?}");
        assert_eq!(
            seen.borrow_mut().take().expect("the backup was taken"),
            Some(fixture.outside.join("a")),
            "the backup holds the link entry itself"
        );
        assert_link_kept(&fixture, "hard link");
        assert!(!fixture.base.join("z").exists());
    }

    // EDG-40: the rollback walks back to each step without following links.
    // `sub/a` is replaced, then `sub` is moved to `sub-orig` inside the
    // version and swapped for a link to an outside dir, and the next entry
    // fails: the rollback refuses to cross that link, so the outside `a`
    // never receives the old bytes; the step stays undone (`sub-orig/a`
    // keeps the new bytes), both `warn` lines are emitted and the backup dir
    // is kept with the old bytes, as decision 4 says.
    // Mutation captured: walking the rollback without `O_NOFOLLOW` renames
    // the backup over the outside `a` and this test goes red.
    #[cfg(unix)]
    #[test]
    fn a_rollback_never_follows_a_directory_swapped_for_a_link() {
        #[derive(Clone)]
        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = Capture(log.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);

        let parent = tempfile::tempdir().unwrap();
        let base = parent.path().join("app");
        let outside = parent.path().join("outside");
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(base.join("sub/a"), b"old").unwrap();
        fs::write(outside.join("a"), b"outside").unwrap();
        let staging = tempfile::tempdir_in(parent.path()).unwrap();
        fs::create_dir_all(staging.path().join("sub")).unwrap();
        fs::write(staging.path().join("sub/a"), b"new").unwrap();
        fs::write(staging.path().join("b"), b"b").unwrap();
        let backup = tempfile::tempdir_in(parent.path()).unwrap();
        let backup_path = backup.path().to_path_buf();

        let (hook_base, hook_outside) = (base.clone(), outside.clone());
        UPLOAD_BEFORE_PUBLISH.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(hook_base.join("sub"), hook_base.join("sub-orig")).unwrap();
                std::os::unix::fs::symlink(&hook_outside, hook_base.join("sub")).unwrap();
            }))
        });
        // Step 1 is the published `sub/a`; the fault hits before `b`.
        UPLOAD_FAULT_AFTER.with(|fault| fault.set(Some(1)));
        let result = apply_staged_upload(
            &version_dir(&base),
            Path::new(""),
            Path::new(""),
            staging.path(),
            &[file("sub/a"), file("b")],
            backup,
        );
        UPLOAD_FAULT_AFTER.with(|fault| fault.set(None));
        let ran = UPLOAD_BEFORE_PUBLISH
            .with(|hook| hook.borrow_mut().take())
            .is_none();
        assert!(ran, "the swap ran");

        let error = result.expect_err("the injected fault must surface");
        assert!(error.message.contains("injected upload fault"), "{error:?}");
        assert_eq!(fs::read(outside.join("a")).unwrap(), b"outside");
        assert_eq!(
            tree(&outside),
            BTreeMap::from([(PathBuf::from("a"), "file:outside".to_string())])
        );
        assert_eq!(fs::read(base.join("sub-orig/a")).unwrap(), b"new");
        assert_eq!(fs::read(backup_path.join("0")).unwrap(), b"old");
        let text = String::from_utf8(log.lock().unwrap().clone()).unwrap();
        assert!(text.contains("files upload rollback step failed"), "{text}");
        assert!(text.contains("path=sub/a"), "{text}");
        assert!(
            text.contains("files upload rollback incomplete; backup dir kept"),
            "{text}"
        );
    }
}
