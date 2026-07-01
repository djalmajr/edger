//! Request pipeline — route resolution, hook stub, pool dispatch (story 05.03).

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::{Json, Router};
use edger_core::{CoreError, ExecutionKind, WorkerRef};
use edger_worker::{WorkerError, WorkerPool};
use serde_json::json;
use tower_http::trace::TraceLayer;

use crate::admin_api;
use crate::auth::{is_public_route, AuthGate};
use crate::context::RequestContext;
use crate::hooks::{
    run_on_request, run_on_response, run_on_worker_complete, run_on_worker_dispatch,
    run_on_worker_error,
};
use crate::manifest_index_stub::ManifestIndex;
use crate::metrics::{metrics_stats_response, pool_metrics_prometheus};
use crate::operational_log::log_operational_error;
use crate::registry::ExtensionRegistry;
use crate::router::{resolve_host_route, resolve_route, ReservedPath, ResolvedRoute};
use crate::server::{request_id_from_headers, request_id_middleware, ServerState};
use crate::service_bindings::{resolve_service_bindings, SERVICE_BINDINGS_HEADER};
use crate::shell_gateway::resolve_shell_worker;
use crate::wire::{axum_to_serialized, serialized_to_axum};

/// Shared orchestrator state for health probes and worker dispatch.
#[derive(Clone)]
pub struct OrchestratorState {
    pub server: ServerState,
    pub pool: WorkerPool,
    pub index: ManifestIndex,
    pub registry: ExtensionRegistry,
    pub auth: AuthGate,
}

/// Build the full axum application (health + readiness + pipeline fallback).
pub fn build_pipeline(state: OrchestratorState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/healthz", get(health_handler))
        .route("/livez", get(live_handler))
        .route("/metrics", get(metrics_handler))
        .route("/metrics/stats", get(metrics_stats_handler))
        .route("/ready", get(ready_handler))
        .route("/readyz", get(ready_handler))
        .merge(admin_api::router())
        .fallback(any(pipeline_handler))
        .layer(axum::middleware::from_fn(request_id_middleware))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

async fn live_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "live" })))
}

async fn ready_handler(State(state): State<OrchestratorState>) -> impl IntoResponse {
    if state.server.is_ready() {
        (StatusCode::OK, Json(json!({ "status": "ready" })))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready" })),
        )
    }
}

async fn metrics_handler(State(state): State<OrchestratorState>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        pool_metrics_prometheus(&state.pool.get_metrics()),
    )
}

async fn metrics_stats_handler(State(state): State<OrchestratorState>) -> impl IntoResponse {
    Json(metrics_stats_response(
        &state.pool.get_metrics(),
        &state.pool.worker_stats(),
    ))
}

async fn pipeline_handler(
    State(state): State<OrchestratorState>,
    req: Request<Body>,
) -> Response<Body> {
    let request_id =
        request_id_from_headers(req.headers()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    match handle_request(&state, req, request_id.clone()).await {
        Ok(res) => res,
        Err(err) => {
            let status = map_error_status(&err);
            log_operational_error("pipeline", Some(&request_id), status, &err);
            error_response(status, &err)
        }
    }
}

async fn handle_request(
    state: &OrchestratorState,
    req: Request<Body>,
    request_id: String,
) -> Result<Response<Body>, CoreError> {
    let path = req.uri().path().to_string();
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if let Some(route) = resolve_host_route(&path, host, &state.index)? {
        return dispatch_resolved_route(state, req, request_id, &path, route).await;
    }

    if let Some(shell) =
        resolve_shell_worker(req.method().as_str(), &path, req.headers(), &state.index)
    {
        let public_worker = is_public_worker(&shell);
        let principal = if public_worker {
            None
        } else {
            state.auth.authorize(
                &path,
                req.headers(),
                shell.config.public_routes.as_ref(),
                shell.namespace.as_deref(),
            )?
        };
        let skip_hooks = public_worker
            || should_skip_hooks(&path, &state.auth, shell.config.public_routes.as_ref());
        return dispatch_worker(
            state,
            req,
            DispatchParams {
                request_id,
                rewritten_path: path,
                kind_hint: Some(shell.kind.clone()),
                principal,
                skip_hooks,
                worker: shell,
            },
        )
        .await;
    }

    let route = resolve_route(&path, None, &state.index)?;

    dispatch_resolved_route(state, req, request_id, &path, route).await
}

async fn dispatch_resolved_route(
    state: &OrchestratorState,
    req: Request<Body>,
    request_id: String,
    path: &str,
    route: ResolvedRoute,
) -> Result<Response<Body>, CoreError> {
    match route {
        ResolvedRoute::Reserved { kind } => handle_reserved(kind),
        ResolvedRoute::PluginBase { .. } => {
            let principal = state.auth.authorize(path, req.headers(), None, None)?;
            dispatch_plugin_stub(principal)
        }
        ResolvedRoute::HomepageFallback { worker } => {
            let public_worker = is_public_worker(&worker);
            let principal = if public_worker {
                None
            } else {
                state.auth.authorize(
                    path,
                    req.headers(),
                    worker.config.public_routes.as_ref(),
                    worker.namespace.as_deref(),
                )?
            };
            let skip_hooks = public_worker
                || should_skip_hooks(path, &state.auth, worker.config.public_routes.as_ref());
            dispatch_worker(
                state,
                req,
                DispatchParams {
                    request_id,
                    worker,
                    rewritten_path: "/".into(),
                    kind_hint: None,
                    principal,
                    skip_hooks,
                },
            )
            .await
        }
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            kind_hint,
        } => {
            let public_worker = is_public_worker(&worker);
            let principal = if public_worker {
                None
            } else {
                state.auth.authorize(
                    path,
                    req.headers(),
                    worker.config.public_routes.as_ref(),
                    worker.namespace.as_deref(),
                )?
            };
            let skip_hooks = public_worker
                || should_skip_hooks(path, &state.auth, worker.config.public_routes.as_ref());
            dispatch_worker(
                state,
                req,
                DispatchParams {
                    request_id,
                    worker,
                    rewritten_path,
                    kind_hint: Some(kind_hint),
                    principal,
                    skip_hooks,
                },
            )
            .await
        }
    }
}

fn should_skip_hooks(
    path: &str,
    auth: &AuthGate,
    worker_public_routes: Option<&edger_core::PublicRoutesConfig>,
) -> bool {
    is_public_route(path, &auth.config.global_public_routes)
        || worker_public_routes.is_some_and(|routes| is_public_route(path, routes))
}

fn is_public_worker(worker: &WorkerRef) -> bool {
    worker.config.visibility == "public"
}

fn dispatch_plugin_stub(
    _principal: Option<edger_core::ApiKeyPrincipal>,
) -> Result<Response<Body>, CoreError> {
    Ok(json_error(
        StatusCode::NOT_IMPLEMENTED,
        "PLUGIN_BASE",
        "plugin dispatch not implemented in story 05.03",
    ))
}

struct DispatchParams {
    request_id: String,
    worker: WorkerRef,
    rewritten_path: String,
    kind_hint: Option<ExecutionKind>,
    principal: Option<edger_core::ApiKeyPrincipal>,
    skip_hooks: bool,
}

async fn dispatch_worker(
    state: &OrchestratorState,
    req: Request<Body>,
    params: DispatchParams,
) -> Result<Response<Body>, CoreError> {
    let DispatchParams {
        request_id,
        worker,
        rewritten_path,
        kind_hint,
        principal,
        skip_hooks,
    } = params;

    let mut serialized = axum_to_serialized(req, request_id.clone()).await?;
    let (original_path, query) = split_path_query(&serialized.uri);
    let base_path = worker_base_path(&worker, original_path);
    serialized.uri = append_query(rewritten_path, query);
    serialized.base_href = Some(base_href(&base_path));
    set_header(&mut serialized.headers, "x-request-id", &request_id);
    set_header(&mut serialized.headers, "x-base", &base_path);
    if let Some(bindings) = resolve_service_bindings(&worker, principal.as_ref(), &state.registry)?
    {
        let bindings = serde_json::to_string(&bindings)
            .map_err(|err| CoreError::new("SERIALIZE_ERROR", err.to_string()))?;
        set_header(&mut serialized.headers, SERVICE_BINDINGS_HEADER, &bindings);
    }

    let mut ctx = RequestContext::new(request_id);
    ctx.principal = principal;
    ctx.worker = Some(worker.clone());

    if !skip_hooks {
        let short_circuit = run_on_request(&state.registry, &mut serialized, &ctx)
            .map_err(|e| CoreError::new("HOOK_ERROR", e.to_string()))?;
        if let Some(response) = short_circuit {
            return serialized_to_axum(response);
        }
        run_on_worker_dispatch(&state.registry, &ctx)
            .map_err(|e| CoreError::new("HOOK_ERROR", e.to_string()))?;
    }

    let mut response = match state
        .pool
        .fetch_worker(&worker, serialized, kind_hint)
        .await
        .map_err(worker_error_to_core)
    {
        Ok(response) => response,
        Err(err) => {
            if !skip_hooks {
                let error = err.to_string();
                run_on_worker_error(&state.registry, &error, &ctx);
            }
            return Err(err);
        }
    };

    if !skip_hooks {
        run_on_worker_complete(&state.registry, &response, &ctx);
        run_on_response(&state.registry, &mut response, &ctx);
    }

    serialized_to_axum(response)
}

fn handle_reserved(kind: ReservedPath) -> Result<Response<Body>, CoreError> {
    match kind {
        ReservedPath::Health => Ok(json_error(StatusCode::OK, "OK", "use /health route")),
        ReservedPath::Ready => Ok(json_error(StatusCode::OK, "OK", "use /ready route")),
        ReservedPath::Api => Ok(json_error(
            StatusCode::NOT_FOUND,
            "API_STUB",
            "api proxy not configured",
        )),
        ReservedPath::WellKnown => Ok(json_error(
            StatusCode::NOT_FOUND,
            "WELL_KNOWN",
            "well-known handler not configured",
        )),
    }
}

fn map_error_status(err: &CoreError) -> StatusCode {
    match err.code.as_str() {
        "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
        "FORBIDDEN" => StatusCode::FORBIDDEN,
        "NOT_FOUND" => StatusCode::NOT_FOUND,
        "COLLISION" => StatusCode::CONFLICT,
        "PAYLOAD_TOO_LARGE" => StatusCode::PAYLOAD_TOO_LARGE,
        "HEADER_TOO_LARGE" => StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
        "HEADER_INVALID" => StatusCode::BAD_REQUEST,
        "VALIDATION_ERROR" | "PARSE_ERROR" | "BODY_ERROR" => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn worker_error_to_core(err: WorkerError) -> CoreError {
    CoreError::new("WORKER_ERROR", err.to_string())
}

fn worker_base_path(worker: &WorkerRef, original_path: &str) -> String {
    let base = format!("/{}", worker.name);
    if original_path == base || original_path.starts_with(&format!("{base}/")) {
        base
    } else {
        "/".into()
    }
}

fn split_path_query(uri: &str) -> (&str, Option<&str>) {
    match uri.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (uri, None),
    }
}

fn append_query(mut path: String, query: Option<&str>) -> String {
    if let Some(query) = query {
        path.push('?');
        path.push_str(query);
    }
    path
}

fn base_href(base_path: &str) -> String {
    if base_path == "/" {
        "/".into()
    } else {
        format!("{}/", base_path.trim_end_matches('/'))
    }
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some((_, existing)) = headers
        .iter_mut()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
    {
        *existing = value.to_string();
    } else {
        headers.push((name.to_string(), value.to_string()));
    }
}

fn json_error(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    let body = Json(json!({ "code": code, "message": message }));
    (status, body).into_response()
}

fn error_response(status: StatusCode, err: &CoreError) -> Response<Body> {
    json_error(status, &err.code, &err.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use edger_core::WorkerManifest;
    use edger_isolation::MockIsolate;
    use edger_worker::{IsolateFactory, PoolConfig};
    use tower::ServiceExt;

    use crate::auth::{AuthGate, AuthGateConfig};
    use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};

    struct StubFactory;
    impl IsolateFactory for StubFactory {
        fn create_isolate(
            &self,
            _worker_ref: &edger_core::WorkerRef,
        ) -> Box<dyn edger_core::Isolate> {
            Box::new(MockIsolate::new())
        }
    }

    fn pipeline_with_hello() -> OrchestratorState {
        let mut index = ManifestIndex::new();
        index
            .insert(
                PathBuf::from("/workers/hello"),
                WorkerManifest {
                    name: "hello".into(),
                    version: Some("1.0.0".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let server = ServerState::new_unready();
        let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(StubFactory));
        server.mark_ready(pool.clone());
        OrchestratorState {
            server,
            pool,
            index,
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

    fn auth_header() -> (&'static str, &'static str) {
        ("authorization", "Bearer test-root")
    }

    #[tokio::test]
    async fn health_via_pipeline_does_not_hit_worker() {
        let state = pipeline_with_hello();
        let app = build_pipeline(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn worker_request_dispatches_to_pool_mock() {
        let state = pipeline_with_hello();
        let app = build_pipeline(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/hello")
                    .header(auth_header().0, auth_header().1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("fetch:GET /"));
    }

    #[tokio::test]
    async fn worker_request_preserves_query_after_path_rewrite() {
        let state = pipeline_with_hello();
        let app = build_pipeline(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/hello?name=Alice")
                    .header(auth_header().0, auth_header().1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("fetch:GET /?name=Alice"));
    }
}
