//! Request pipeline — route resolution, hook stub, pool dispatch (story 05.03).

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::{Json, Router};
use edger_core::{CoreError, ExecutionKind, SerializedRequest, SerializedResponse, WorkerRef};
use edger_worker::{WorkerError, WorkerPool};
use serde_json::json;
use tower_http::trace::TraceLayer;

use crate::auth::AuthGate;
use crate::context::RequestContext;
use crate::manifest_index_stub::ManifestIndex;
use crate::router::{resolve_route, ReservedPath, ResolvedRoute};
use crate::server::{request_id_from_headers, request_id_middleware, ServerState};
use crate::wire::{axum_to_serialized, serialized_to_axum};

/// Stub hook runner — registry wiring lands in story 05.05.
#[derive(Clone, Debug, Default)]
pub struct HookRunner;

impl HookRunner {
    pub fn run_on_request(
        &self,
        _req: &mut SerializedRequest,
        _ctx: &RequestContext,
    ) -> Option<SerializedResponse> {
        None
    }
}

/// Shared orchestrator state for health probes and worker dispatch.
#[derive(Clone)]
pub struct OrchestratorState {
    pub server: ServerState,
    pub pool: WorkerPool,
    pub index: ManifestIndex,
    pub hooks: HookRunner,
    pub auth: AuthGate,
}

/// Build the full axum application (health + readiness + pipeline fallback).
pub fn build_pipeline(state: OrchestratorState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .fallback(any(pipeline_handler))
        .layer(axum::middleware::from_fn(request_id_middleware))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
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

async fn pipeline_handler(
    State(state): State<OrchestratorState>,
    req: Request<Body>,
) -> Response<Body> {
    let request_id = request_id_from_headers(req.headers())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    match handle_request(&state, req, request_id).await {
        Ok(res) => res,
        Err(err) => error_response(map_error_status(&err), &err),
    }
}

async fn handle_request(
    state: &OrchestratorState,
    req: Request<Body>,
    request_id: String,
) -> Result<Response<Body>, CoreError> {
    let path = req.uri().path().to_string();
    let route = resolve_route(&path, None, &state.index)?;

    match route {
        ResolvedRoute::Reserved { kind } => handle_reserved(kind),
        ResolvedRoute::PluginBase { .. } => {
            let principal = state.auth.authorize(&path, req.headers(), None, None)?;
            dispatch_plugin_stub(principal)
        }
        ResolvedRoute::HomepageFallback { worker } => {
            let principal = state.auth.authorize(
                &path,
                req.headers(),
                worker.config.public_routes.as_ref(),
                worker.namespace.as_deref(),
            )?;
            dispatch_worker(
                state,
                req,
                request_id,
                worker,
                "/".into(),
                None,
                principal,
            )
            .await
        }
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            kind_hint,
        } => {
            let principal = state.auth.authorize(
                &path,
                req.headers(),
                worker.config.public_routes.as_ref(),
                worker.namespace.as_deref(),
            )?;
            dispatch_worker(
                state,
                req,
                request_id,
                worker,
                rewritten_path,
                Some(kind_hint),
                principal,
            )
            .await
        }
    }
}

fn dispatch_plugin_stub(_principal: Option<edger_core::ApiKeyPrincipal>) -> Result<Response<Body>, CoreError> {
    Ok(json_error(
        StatusCode::NOT_IMPLEMENTED,
        "PLUGIN_BASE",
        "plugin dispatch not implemented in story 05.03",
    ))
}

async fn dispatch_worker(
    state: &OrchestratorState,
    req: Request<Body>,
    request_id: String,
    worker: WorkerRef,
    rewritten_path: String,
    kind_hint: Option<ExecutionKind>,
    principal: Option<edger_core::ApiKeyPrincipal>,
) -> Result<Response<Body>, CoreError> {
    let mut serialized = axum_to_serialized(req, request_id.clone()).await?;
    serialized.uri = rewritten_path;
    serialized.base_href = Some(format!("/{}/", worker.name.trim_start_matches('@')));

    let mut ctx = RequestContext::new(request_id);
    ctx.principal = principal;
    ctx.worker = Some(worker.clone());

    if let Some(short_circuit) = state.hooks.run_on_request(&mut serialized, &ctx) {
        return serialized_to_axum(short_circuit);
    }

    let response = state
        .pool
        .fetch(&worker.dir, &worker.config, serialized, kind_hint)
        .await
        .map_err(worker_error_to_core)?;

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
        "VALIDATION_ERROR" | "PARSE_ERROR" | "BODY_ERROR" => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn worker_error_to_core(err: WorkerError) -> CoreError {
    CoreError::new("WORKER_ERROR", err.to_string())
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
    use crate::store::SqliteApiKeyStore;

    struct StubFactory;
    impl IsolateFactory for StubFactory {
        fn create_isolate(&self) -> Box<dyn edger_core::Isolate> {
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
            hooks: HookRunner,
            auth: AuthGate::new(
                AuthGateConfig {
                    root_api_key: Some("test-root".into()),
                    ..Default::default()
                },
                Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
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
}