//! Request pipeline — route resolution and pool dispatch.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::{Json, Router};
use edger_core::{
    effective_max_body_size_bytes_usize, ApiKeyPrincipal, CoreError, ExecutionKind, WorkerRef,
    INTERNAL_REQUEST_HEADER,
};
use edger_worker::{WorkerError, WorkerPool};
use serde_json::json;
use tower_http::compression::predicate::Predicate;
use tower_http::trace::TraceLayer;

use crate::admin_api;
use crate::auth::ControlAuth;
use crate::compression::{
    compression_layer, mark_app_response, mark_worker_without_encoding, weaken_worker_etag,
};
use crate::manifest_index_stub::ManifestIndex;
use crate::metrics::{
    cron_metrics_prometheus, metrics_stats_response, pool_metrics_prometheus,
    tenant_routing_metrics_prometheus,
};
use crate::observability::{OperationalEventInput, OperationalEventLevel, OperationalEventSource};
use crate::operational_log::log_operational_error;
use crate::router::{
    resolve_host_route_with_internal, resolve_route_with_internal, ReservedPath, ResolvedRoute,
};
use crate::routing_policy::{
    cohort_for_cookie_header, cohort_set_cookie, RoutingPolicy, TenantAccess,
};
use crate::server::{
    request_id_from_headers, request_id_middleware, request_metrics_middleware, ServerState,
};
use crate::wire::{axum_to_serialized_with_limit, serialized_to_axum};

/// Shared orchestrator state for health probes and worker dispatch.
#[derive(Clone)]
pub struct OrchestratorState {
    pub server: ServerState,
    pub pool: WorkerPool,
    pub index: ManifestIndex,

    pub auth: ControlAuth,
}

pub(crate) const ADMIN_WORKER_VERSION_HEADER: &str = "x-edger-worker-version";
pub(crate) const ADMIN_CONTROL_AUTH_HEADER: &str = "x-edger-control-authorization";

/// Build the full axum application (health + readiness + pipeline fallback).
pub fn build_pipeline(state: OrchestratorState) -> Router {
    let metrics_state = state.server.clone();
    Router::new()
        .route("/", get(root_redirect))
        .route("/health", get(health_handler))
        .route("/healthz", get(health_handler))
        .route("/livez", get(live_handler))
        .route("/metrics", get(metrics_handler))
        .route("/metrics/stats", get(metrics_stats_handler))
        .route("/ready", get(ready_handler))
        .route("/readyz", get(ready_handler))
        // MCP por HTTP: o corpo carrega zipBase64 de deploy (64 MiB de ZIP
        // viram ~86 MiB de base64 + envelope JSON).
        .route(
            "/api/mcp",
            axum::routing::post(crate::mcp_http::handle)
                .layer(axum::extract::DefaultBodyLimit::max(96 * 1024 * 1024)),
        )
        .merge(admin_api::router())
        .fallback(any(pipeline_handler))
        // Domínio com dono é inteiro do app (D6): aplicada depois do fallback
        // para que, num Host com dono, toda a requisição vá para o worker
        // dono antes das rotas fixas do control plane. As layers de métricas,
        // request-id e tracing continuam envolvendo a requisição.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            owned_host_middleware,
        ))
        // EDG-2/EDG-3: compression (brotli + gzip) sits OUTSIDE
        // `owned_host_middleware` — covering the fallback and owned domains —
        // and INSIDE `request_metrics_middleware`. Only responses marked as
        // app (`pipeline_handler`) are compressed; the control plane passes
        // through untouched. Immediately outside the compression layer, the
        // ETag middleware weakens a worker's strong ETag once compression
        // changed the content-coding.
        .layer(compression_layer())
        .layer(axum::middleware::from_fn(weaken_worker_etag))
        .layer(axum::middleware::from_fn_with_state(
            metrics_state,
            request_metrics_middleware,
        ))
        .layer(axum::middleware::from_fn(request_id_middleware))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// The runtime's own control panel is the front door: `/` redirects to the
/// cPanel worker at its canonical mount (`/cpanel/`). Keeping it a plain 302
/// (instead of serving the SPA at `/`) means the cPanel has one canonical URL
/// with a stable base path — swappable/client-routed frontends stay correct —
/// while app workers keep the bare `/<worker>` namespace and unknown paths 404.
/// The Location is RELATIVE on purpose: the browser resolves it against the
/// PUBLIC URL, so behind a stripping proxy `/apps/` becomes `/apps/cpanel/`
/// instead of escaping the prefix (an absolute `/cpanel/` landed on whatever
/// owned that path on the shared host).
async fn root_redirect() -> impl IntoResponse {
    axum::response::Redirect::temporary("cpanel/")
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

// D7: /metrics e /metrics/stats expõem nome e versão de cada app instalado,
// então exigem credencial com observability:read (ou a root). No modo
// aberto (sem root key) o `authenticate` já devolve a root e nada muda.
async fn metrics_handler(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
) -> Response<Body> {
    if let Err(err) = admin_api::authenticate(&state, &headers)
        .await
        .and_then(|principal| admin_api::require_permission(&principal, "observability:read"))
    {
        return admin_api::admin_error(admin_api::map_error_status(&err), &err, &headers);
    }
    let mut body = pool_metrics_prometheus(&state.pool.get_metrics());
    body.push_str(&cron_metrics_prometheus(&state.server.cron_metrics()));
    body.push_str(&crate::metrics::http_metrics_prometheus(
        &state.server.http_metrics(),
    ));
    body.push_str(&tenant_routing_metrics_prometheus(
        &state.server.tenant_routing_metrics(),
    ));
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

async fn metrics_stats_handler(
    State(state): State<OrchestratorState>,
    headers: HeaderMap,
) -> Response<Body> {
    if let Err(err) = admin_api::authenticate(&state, &headers)
        .await
        .and_then(|principal| admin_api::require_permission(&principal, "observability:read"))
    {
        return admin_api::admin_error(admin_api::map_error_status(&err), &err, &headers);
    }
    (
        StatusCode::OK,
        Json(metrics_stats_response(
            &state.pool.get_metrics(),
            &state.pool.worker_stats(),
        )),
    )
        .into_response()
}

/// A autoridade da requisição para fins de roteamento (D20): em HTTP/2 ela
/// vem do pseudo-header `:authority` — o hyper a expõe em
/// `uri().authority()` — e o header `Host` pode não existir. No
/// request-target em forma absoluta a autoridade do URI prevalece sobre o
/// `Host` (RFC 9112 §3.2.2, RFC 9113 §8.3.1). Do request-target usamos só o
/// `host[:port]` — `userinfo@` não faz parte da autoridade (D20, revisada);
/// o ramo do header `Host` continua devolvendo o valor cru, que o
/// `normalize_host_alias` valida.
fn request_authority(req: &Request<Body>) -> Option<String> {
    if let Some(authority) = req.uri().authority() {
        return match authority.port_u16() {
            Some(port) => Some(format!("{}:{}", authority.host(), port)),
            None => Some(authority.host().to_string()),
        };
    }
    req.headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Num Host com dono, o domínio é inteiro do app (D6): a requisição vai
/// direto para o pipeline do worker dono, sem passar pelas rotas fixas do
/// control plane. Hosts sem dono seguem o app normal (rotas fixas + fallback).
async fn owned_host_middleware(
    State(state): State<OrchestratorState>,
    req: Request<Body>,
    next: axum::middleware::Next,
) -> Response<Body> {
    let host = request_authority(&req);
    if host
        .as_deref()
        .is_some_and(|host| state.index.host_owner(host).is_some())
    {
        pipeline_handler(State(state), req).await
    } else {
        next.run(req).await
    }
}

/// Every response this handler produces — worker responses and pipeline
/// errors alike — is marked with the `AppResponse` extension: it is the only
/// data the EDG-2 compression predicate uses to decide a response belongs to
/// an app and may be compressed.
async fn pipeline_handler(
    State(state): State<OrchestratorState>,
    req: Request<Body>,
) -> Response<Body> {
    let request_id =
        request_id_from_headers(req.headers()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    match handle_request(&state, req, request_id.clone()).await {
        Ok(mut res) => {
            mark_app_response(&mut res);
            // The worker produced this body WITHOUT a content-encoding:
            // record it, so the ETag-weakening middleware knows a later
            // compression transforms the representation and must downgrade
            // a strong ETag. Responses already carrying a content-encoding
            // keep their validator intact.
            if !res.headers().contains_key(header::CONTENT_ENCODING) {
                mark_worker_without_encoding(&mut res);
            }
            res
        }
        Err(err) => {
            let status = map_error_status(&err);
            log_operational_error("pipeline", Some(&request_id), status, &err);
            let mut res = error_response(status, &err);
            mark_app_response(&mut res);
            res
        }
    }
}

async fn handle_request(
    state: &OrchestratorState,
    req: Request<Body>,
    request_id: String,
) -> Result<Response<Body>, CoreError> {
    let path = req.uri().path().to_string();
    tracing::debug!(request_id = %request_id, path = %path, method = %req.method(), "http request pipeline");
    let allow_internal = if header_is_true(req.headers(), INTERNAL_REQUEST_HEADER) {
        state
            .auth
            .authenticate_headers(req.headers())
            .await
            .is_some_and(|principal| principal.is_root)
    } else {
        false
    };
    let host = request_authority(&req);
    if let Some(route) =
        resolve_host_route_with_internal(&path, host.as_deref(), &state.index, allow_internal)?
    {
        return dispatch_resolved_route(
            state,
            req,
            request_id,
            &path,
            route,
            allow_internal,
            host.as_deref(),
        )
        .await;
    }

    let route = resolve_route_with_internal(&path, None, &state.index, allow_internal)?;

    dispatch_resolved_route(state, req, request_id, &path, route, allow_internal, None).await
}

async fn dispatch_resolved_route(
    state: &OrchestratorState,
    req: Request<Body>,
    request_id: String,
    _path: &str,
    route: ResolvedRoute,
    allow_internal: bool,
    route_host: Option<&str>,
) -> Result<Response<Body>, CoreError> {
    // Data plane is OPEN (Epic 17): the edger does not authenticate worker
    // requests. The worker receives the raw request (Authorization intact) and
    // owns its own auth. Only the control plane (`/api/admin/*`) is gated.
    match route {
        ResolvedRoute::Reserved { kind } => handle_reserved(kind),
        ResolvedRoute::PluginBase { plugin, remainder } => {
            let worker = state.index.resolve_plugin_worker(&plugin)?;
            let base = plugin.base.clone();
            let input = CohortInput {
                version_pinned: false,
                required_host: None,
                required_plugin_base: Some(base.clone()),
                authority: request_authority(&req),
                cookie_header: cookie_header_from(&req),
            };
            let (worker, trusted_tenant, cohort_cookie) =
                prepare_worker(state, worker, allow_internal, input).await?;
            let kind_hint = worker.kind.clone();
            dispatch_worker(
                state,
                req,
                DispatchParams {
                    request_id,
                    worker,
                    rewritten_path: normalize_rewritten_path(&remainder),
                    kind_hint: Some(kind_hint),
                    principal: None,
                    base_path: Some(base),
                    trusted_tenant,
                    cohort_cookie,
                },
            )
            .await
        }
        ResolvedRoute::HomepageFallback { worker } => {
            let input = CohortInput {
                version_pinned: false,
                required_host: None,
                required_plugin_base: None,
                authority: request_authority(&req),
                cookie_header: cookie_header_from(&req),
            };
            let (worker, trusted_tenant, cohort_cookie) =
                prepare_worker(state, worker, allow_internal, input).await?;
            dispatch_worker(
                state,
                req,
                DispatchParams {
                    request_id,
                    worker,
                    rewritten_path: "/".into(),
                    kind_hint: None,
                    principal: None,
                    base_path: None,
                    trusted_tenant,
                    cohort_cookie,
                },
            )
            .await
        }
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            kind_hint,
            version_pinned,
        } => {
            let input = CohortInput {
                version_pinned,
                required_host: route_host.map(str::to_string),
                required_plugin_base: None,
                authority: request_authority(&req),
                cookie_header: cookie_header_from(&req),
            };
            let (worker, trusted_tenant, cohort_cookie) =
                prepare_worker(state, worker, allow_internal, input).await?;
            dispatch_worker(
                state,
                req,
                DispatchParams {
                    request_id,
                    worker,
                    rewritten_path,
                    kind_hint: Some(kind_hint),
                    principal: None,
                    base_path: None,
                    trusted_tenant,
                    cohort_cookie,
                },
            )
            .await
        }
    }
}

struct CohortInput {
    version_pinned: bool,
    required_host: Option<String>,
    required_plugin_base: Option<String>,
    authority: Option<String>,
    cookie_header: Option<String>,
}

/// One `RoutingPolicy` snapshot feeds the tenant gate and, only after it
/// approves, the weight split. A concurrent PUT cannot change the weights
/// seen by this request. `edger_cohort` is never copied into `x-tenant-id`.
/// Cookie bytes are copied before any await so the request is not borrowed
/// across the tenant lookup.
async fn prepare_worker(
    state: &OrchestratorState,
    worker: WorkerRef,
    allow_internal: bool,
    input: CohortInput,
) -> Result<(WorkerRef, Option<String>, Option<String>), CoreError> {
    let CohortInput {
        version_pinned,
        required_host,
        required_plugin_base,
        authority,
        cookie_header,
    } = input;
    let policy = if state.server.tenant_routing_enabled() || state.server.weighted_routing_enabled()
    {
        state.index.routing_policy(&worker.name)?
    } else {
        None
    };
    let trusted_tenant =
        tenant_for_worker(state, authority, &worker, allow_internal, policy.as_ref()).await?;
    if version_pinned || allow_internal || !state.server.weighted_routing_enabled() {
        return Ok((worker, trusted_tenant, None));
    }
    let Some(policy) = policy.as_ref().filter(|policy| policy.traffic.is_some()) else {
        return Ok((worker, trusted_tenant, None));
    };
    let (cohort, minted) = cohort_for_cookie_header(cookie_header.as_deref());
    let Some(selected) = state.index.select_weighted_worker(
        &worker.name,
        &cohort,
        policy,
        required_host.as_deref(),
        required_plugin_base.as_deref(),
    )?
    else {
        return Ok((worker, trusted_tenant, None));
    };
    let cohort_cookie = minted.then(|| cohort_set_cookie(&cohort));
    Ok((selected, trusted_tenant, cohort_cookie))
}

fn cookie_header_from(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Domain availability is checked for the logical app, independently of the
/// version selected for this request. User authorization remains in the worker.
async fn tenant_for_worker(
    state: &OrchestratorState,
    authority: Option<String>,
    _worker: &WorkerRef,
    allow_internal: bool,
    policy: Option<&RoutingPolicy>,
) -> Result<Option<String>, CoreError> {
    // Cron and other root-authenticated internal invocations carry no visitor
    // domain. The public `@version` route still passes the tenant gate.
    // `policy` is the snapshot already loaded for this request's logical app.
    if allow_internal {
        return Ok(None);
    }
    if !state.server.tenant_routing_enabled() {
        return Ok(None);
    }
    let Some(policy) = policy else {
        return Ok(None);
    };
    let TenantAccess::Allowlist { tenants } = &policy.tenant_access else {
        return Ok(None);
    };
    let metrics = state.server.tenant_routing_metrics();
    let authority = authority.ok_or_else(|| {
        metrics.denied();
        CoreError::new("TENANT_NOT_FOUND", "application not available")
    })?;
    let client = state.server.tenant_identity_client().ok_or_else(|| {
        metrics.unavailable();
        CoreError::new("TENANT_IDENTITY_UNAVAILABLE", "tenant identity unavailable")
    })?;
    let tenant = client.identify(&authority).await.map_err(|err| match err {
        crate::tenant_identity::IdentifyError::NotFound => {
            metrics.denied();
            CoreError::new("TENANT_NOT_FOUND", "application not available")
        }
        crate::tenant_identity::IdentifyError::Unavailable => {
            metrics.unavailable();
            CoreError::new("TENANT_IDENTITY_UNAVAILABLE", "tenant identity unavailable")
        }
    })?;
    if tenants.iter().any(|allowed| allowed == &tenant) {
        metrics.allowed();
        Ok(Some(tenant))
    } else {
        metrics.denied();
        Err(CoreError::new(
            "TENANT_NOT_FOUND",
            "application not available",
        ))
    }
}

struct DispatchParams {
    request_id: String,
    worker: WorkerRef,
    rewritten_path: String,
    kind_hint: Option<ExecutionKind>,
    principal: Option<edger_core::ApiKeyPrincipal>,
    base_path: Option<String>,
    trusted_tenant: Option<String>,
    cohort_cookie: Option<String>,
}

pub(crate) async fn invoke_worker(
    state: &OrchestratorState,
    mut req: Request<Body>,
    request_id: String,
    name: &str,
    version: Option<&str>,
    rewritten_path: String,
    principal: ApiKeyPrincipal,
) -> Result<Response<Body>, CoreError> {
    let worker = state.index.resolve_worker(name, version)?;
    let kind_hint = worker.kind.clone();
    req.headers_mut().remove(ADMIN_CONTROL_AUTH_HEADER);
    req.headers_mut().remove(ADMIN_WORKER_VERSION_HEADER);
    req.headers_mut().remove(INTERNAL_REQUEST_HEADER);
    dispatch_worker(
        state,
        req,
        DispatchParams {
            request_id,
            worker,
            rewritten_path,
            kind_hint: Some(kind_hint),
            principal: Some(principal),
            base_path: Some(format!("/{}", name.trim_end_matches('/'))),
            trusted_tenant: None,
            cohort_cookie: None,
        },
    )
    .await
}

#[tracing::instrument(
    name = "worker.dispatch",
    skip_all,
    fields(
        request_id = %params.request_id,
        worker.name = %params.worker.name,
        worker.version = %params.worker.version,
        worker.namespace = params.worker.namespace.as_deref().unwrap_or("")
    )
)]
async fn dispatch_worker(
    state: &OrchestratorState,
    mut req: Request<Body>,
    params: DispatchParams,
) -> Result<Response<Body>, CoreError> {
    let trace_id = trace_id_from_headers(req.headers());
    #[cfg(feature = "otel")]
    attach_remote_trace_parent(req.headers());
    // Conditional revalidation (RFC 9110 §13.1.2, EDG-5 part 2): the method
    // and If-None-Match must be read BEFORE the request is consumed by the
    // dispatch below — the buffered response is turned into a 304 Not
    // Modified at the response conversion using these values.
    let conditional_method = req.method().to_string();
    // If-None-Match is a list field and may arrive as several header lines;
    // RFC 9110 §5.2 makes a repeated field equivalent to the comma-joined
    // value in received order, so join every value BEFORE the parser sees it.
    // If any value is not valid header text the header is malformed and must
    // never yield a 304 (a malformed validator is a "no match").
    let if_none_match = req
        .headers()
        .get_all("if-none-match")
        .into_iter()
        .map(|value| value.to_str())
        .collect::<Result<Vec<&str>, _>>()
        .ok()
        .filter(|values| !values.is_empty())
        .map(|values| values.join(", "));
    let DispatchParams {
        request_id,
        worker,
        rewritten_path,
        kind_hint,
        principal,
        base_path,
        trusted_tenant,
        cohort_cookie,
    } = params;

    // No request header can assert tenant identity. Only the authenticated
    // service-to-service lookup above may supply this value to a worker.
    req.headers_mut().remove("x-tenant-id");
    if let Some(tenant) = trusted_tenant {
        req.headers_mut().insert(
            "x-tenant-id",
            HeaderValue::from_str(&tenant).expect("validated Tenancit slug is a header value"),
        );
    }

    // Per-worker admission ceiling (Epic 20.08): protects an individual worker
    // from a single abusive caller exhausting its queue/slots before the pool's
    // backpressure engages. Ops-of-runtime; the worker still owns app auth.
    if let Some(rps) = worker.config.rate_limit_rps {
        if !crate::rate_limit::allow(&worker.name, rps) {
            return Err(CoreError::new(
                "RATE_LIMITED",
                format!("worker '{}' rate limit exceeded ({rps}/s)", worker.name),
            ));
        }
    }

    let started = std::time::Instant::now();
    tracing::info!(
        request_id = %request_id,
        worker_name = %worker.name,
        worker_version = %worker.version,
        worker_namespace = worker.namespace.as_deref().unwrap_or(""),
        "worker dispatch"
    );
    // Data plane is open, but the `x-edger-internal` marker (used by cron and
    // authenticated control-plane invocation) must stay trustworthy. External
    // callers can present the header only when their control credential is root.
    let internal_principal = match principal {
        Some(principal) => Some(principal),
        None => state.auth.authenticate_headers(req.headers()).await,
    };
    sanitize_internal_headers(&mut req, internal_principal.as_ref());
    // Behind a stripping proxy (Kong route with strip_path) the public URL
    // carries a prefix this process never sees in the request path. Honoring
    // X-Forwarded-Prefix keeps the emitted <base href> — and x-base — aligned
    // with the URL the browser actually used; the value is charset-checked
    // because it lands inside served HTML.
    let forwarded_prefix = req
        .headers()
        .get("x-forwarded-prefix")
        .and_then(|value| value.to_str().ok())
        .and_then(normalized_forwarded_prefix)
        .unwrap_or_default();
    let max_body_bytes = effective_max_body_size_bytes_usize(&worker.config);
    let mut serialized =
        axum_to_serialized_with_limit(req, request_id.clone(), max_body_bytes).await?;
    let (original_path, query) = split_path_query(&serialized.uri);
    let base_path = base_path.unwrap_or_else(|| worker_base_path(&worker, original_path));
    let public_base_path = if base_path == "/" {
        if forwarded_prefix.is_empty() {
            base_path.clone()
        } else {
            forwarded_prefix.clone()
        }
    } else {
        format!("{forwarded_prefix}{base_path}")
    };
    serialized.uri = append_query(rewritten_path, query);
    serialized.base_href = Some(base_href(&public_base_path));
    set_header(&mut serialized.headers, "x-request-id", &request_id);
    set_header(&mut serialized.headers, "x-base", &public_base_path);
    #[cfg(feature = "otel")]
    inject_current_trace_context(&mut serialized.headers);

    let retry_request = serialized.clone();
    let retry_kind_hint = kind_hint.clone();
    let request_method = serialized.method.clone();
    let first_attempt = state
        .pool
        .fetch_worker_stream(&worker, serialized, kind_hint)
        .await;
    let worker_result = match first_attempt {
        Err(err) if should_retry_worker_transport(&request_method, &err) => {
            tracing::warn!(
                request_id = %request_id,
                worker_name = %worker.name,
                worker_version = %worker.version,
                error = %err,
                "retrying idempotent worker request after transient transport failure"
            );
            state
                .pool
                .fetch_worker_stream(&worker, retry_request, retry_kind_hint)
                .await
        }
        result => result,
    };

    let worker_response = match worker_result.map_err(worker_error_to_core) {
        Ok(response) => response,
        Err(err) => {
            let status = map_error_status(&err).as_u16();
            state.server.worker_errors().record(
                &worker.name,
                &request_id,
                status,
                &err.code,
                &err.message,
            );
            state
                .server
                .operational_events()
                .record(OperationalEventInput {
                    source: OperationalEventSource::Runtime,
                    kind: "dispatch".into(),
                    level: OperationalEventLevel::Error,
                    namespace: worker.namespace.clone(),
                    worker: Some(worker.name.clone()),
                    version: Some(worker.version.clone()),
                    process_id: None,
                    request_id: Some(request_id.clone()),
                    trace_id: trace_id.clone(),
                    outcome: Some(err.code.clone()),
                    status: Some(status),
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    code: Some(err.code.clone()),
                    message: Some(err.message.clone()),
                    truncated: None,
                    dropped_count: None,
                });
            crate::operational_log::log_dispatch_event(
                &request_id,
                &worker.name,
                &worker.version,
                worker.namespace.as_deref().unwrap_or(""),
                &err.code,
                started.elapsed().as_millis() as u64,
                status,
            );
            return Err(err);
        }
    };

    let response_status = match &worker_response {
        edger_core::WorkerResponse::Buffered(response) => response.status,
        edger_core::WorkerResponse::Streamed(response) => response.status,
    };
    let duration_ms = started.elapsed().as_millis() as u64;

    state
        .server
        .operational_events()
        .record(OperationalEventInput {
            source: OperationalEventSource::Runtime,
            kind: "dispatch".into(),
            level: if response_status >= 500 {
                OperationalEventLevel::Error
            } else {
                OperationalEventLevel::Info
            },
            namespace: worker.namespace.clone(),
            worker: Some(worker.name.clone()),
            version: Some(worker.version.clone()),
            process_id: None,
            request_id: Some(request_id.clone()),
            trace_id,
            outcome: Some(if response_status >= 500 {
                "http_5xx".into()
            } else {
                "ok".into()
            }),
            status: Some(response_status),
            duration_ms: Some(duration_ms),
            code: None,
            message: None,
            truncated: None,
            dropped_count: None,
        });

    crate::operational_log::log_dispatch_event(
        &request_id,
        &worker.name,
        &worker.version,
        worker.namespace.as_deref().unwrap_or(""),
        "ok",
        duration_ms,
        response_status,
    );

    let mut response = match worker_response {
        edger_core::WorkerResponse::Buffered(response) => {
            // Only a BUFFERED response can be short-circuited to a 304: its
            // status and headers fully describe the representation a client
            // already has. Streams are never short-circuited — the body may
            // not have been fully read. A 304 carries no body and only the
            // revalidation fields; it is built BEFORE the cohort Set-Cookie
            // append and the pipeline_handler markers (AppResponse /
            // x-request-id), so those survive on the 304 as well.
            let not_modified = crate::conditional::should_not_modify(
                &conditional_method,
                if_none_match.as_deref(),
                response.status,
                &response.headers,
            );
            if not_modified {
                // A 304 must carry the `Vary` the 200 of the SAME request
                // would carry. The tower-http compression layer appends
                // `Vary: Accept-Encoding` to a 200 whenever the EDG-2
                // predicate deems it eligible (even when the client asks for
                // no compression) but never to a 304 (bodyless, skipped). So
                // evaluate the ORIGINAL 200's eligibility as it would be
                // marked AppResponse — that marker is applied by
                // pipeline_handler AFTER dispatch_worker returns — and, if
                // eligible, make sure the 304 lists accept-encoding in Vary,
                // without duplicating or dropping existing Vary fields.
                let mut original_200 = serialized_to_axum(response.clone())?;
                mark_app_response(&mut original_200);
                // Mirror the tower-http compression layer's guards exactly
                // (future.rs: `content-encoding` and `content-range` are
                // rejected BEFORE the predicate runs, and the predicate does
                // not see either header). A 200 that already carries one of
                // them is never compressed, so its 304 must not announce
                // `Vary: accept-encoding` either.
                let headers = original_200.headers();
                let compressible = !headers.contains_key(header::CONTENT_ENCODING)
                    && !headers.contains_key(header::CONTENT_RANGE)
                    && crate::compression::app_response_predicate().should_compress(&original_200);
                let not_modified = crate::conditional::build_not_modified(&response);
                let response = serialized_to_axum(not_modified)?;
                let response = if compressible {
                    response_with_vary_accept_encoding(response)
                } else {
                    response
                };
                // RFC 9110 §15.4.5: a 304 must not carry representation-data
                // fields. Axum stamps `content-length: 0` on any exact-size
                // empty body at the top-level route (outside every
                // middleware), so the 304 body is an empty stream with an
                // unknown — not exact — size, which keeps that header from
                // being injected.
                response.map(|_| {
                    Body::from_stream(futures_util::stream::empty::<
                        std::result::Result<bytes::Bytes, std::io::Error>,
                    >())
                })
            } else {
                serialized_to_axum(response)?
            }
        }
        edger_core::WorkerResponse::Streamed(streamed) => crate::wire::streamed_to_axum(streamed)?,
    };
    if let Some(cookie) = cohort_cookie {
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    Ok(response)
}

/// Ensure `accept-encoding` is listed in the response's `Vary`. Mirrors the
/// rule the tower-http compression layer applies to a compressible 200: it
/// appends `Vary: Accept-Encoding` unless some Vary value already contains
/// "accept-encoding" (case-insensitive). Existing Vary fields are preserved.
fn response_with_vary_accept_encoding(mut res: Response<Body>) -> Response<Body> {
    const NEEDLE: &[u8] = b"accept-encoding";
    if !res
        .headers()
        .get_all(header::VARY)
        .into_iter()
        .any(|value| bytes_contains_ignore_ascii_case(value.as_bytes(), NEEDLE))
    {
        res.headers_mut()
            .append(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    res
}

/// Case-insensitive byte-substring test — the same primitive tower-http uses
/// when deciding whether to append `Vary: Accept-Encoding`.
fn bytes_contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

fn trace_id_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = headers.get("traceparent")?.to_str().ok()?;
    let mut parts = value.split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let parent_id = parts.next()?;
    let flags = parts.next()?;
    if parts.next().is_some()
        || version.len() != 2
        || trace_id.len() != 32
        || parent_id.len() != 16
        || flags.len() != 2
        || trace_id.bytes().all(|byte| byte == b'0')
        || !trace_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !parent_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !flags.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(trace_id.to_ascii_lowercase())
}

#[cfg(feature = "otel")]
fn attach_remote_trace_parent(headers: &axum::http::HeaderMap) {
    use opentelemetry::global;
    use opentelemetry::propagation::Extractor;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    struct HeaderExtractor<'a>(&'a axum::http::HeaderMap);
    impl Extractor for HeaderExtractor<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0.get(key).and_then(|value| value.to_str().ok())
        }
        fn keys(&self) -> Vec<&str> {
            self.0.keys().map(axum::http::HeaderName::as_str).collect()
        }
    }

    let parent =
        global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(headers)));
    let _ = tracing::Span::current().set_parent(parent);
}

#[cfg(feature = "otel")]
struct WorkerHeaderInjector<'a>(&'a mut Vec<(String, String)>);

#[cfg(feature = "otel")]
impl opentelemetry::propagation::Injector for WorkerHeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        set_header(self.0, key, &value);
    }
}

#[cfg(feature = "otel")]
fn inject_current_trace_context(headers: &mut Vec<(String, String)>) {
    use opentelemetry::global;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let context = tracing::Span::current().context();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut WorkerHeaderInjector(headers));
    });
}

fn should_retry_worker_transport(method: &str, err: &WorkerError) -> bool {
    let is_idempotent = method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD");
    is_idempotent
        && matches!(
            err,
            WorkerError::Isolation(isolation)
                if matches!(isolation.code.as_str(), "UDS_IO" | "UDS_POISONED")
        )
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
        "NOT_FOUND" | "TENANT_NOT_FOUND" => StatusCode::NOT_FOUND,
        "COLLISION" => StatusCode::CONFLICT,
        "PAYLOAD_TOO_LARGE" => StatusCode::PAYLOAD_TOO_LARGE,
        "HEADER_TOO_LARGE" => StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
        "HEADER_INVALID" => StatusCode::BAD_REQUEST,
        "WORKER_QUEUE_FULL" => StatusCode::TOO_MANY_REQUESTS,
        "RATE_LIMITED" => StatusCode::TOO_MANY_REQUESTS,
        "WORKER_QUEUE_TIMEOUT"
        | "TENANT_IDENTITY_UNAVAILABLE"
        | "LOCK_ERROR"
        | "ROUTING_UNAVAILABLE" => StatusCode::SERVICE_UNAVAILABLE,
        "WORKER_CIRCUIT_OPEN" => StatusCode::SERVICE_UNAVAILABLE,
        "VALIDATION_ERROR" | "PARSE_ERROR" | "BODY_ERROR" => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn worker_error_to_core(err: WorkerError) -> CoreError {
    match err {
        WorkerError::WorkerQueueFull => CoreError::new("WORKER_QUEUE_FULL", err.to_string()),
        WorkerError::WorkerQueueTimeout => CoreError::new("WORKER_QUEUE_TIMEOUT", err.to_string()),
        WorkerError::CircuitOpen { .. } => CoreError::new("WORKER_CIRCUIT_OPEN", err.to_string()),
        _ => CoreError::new("WORKER_ERROR", err.to_string()),
    }
}

fn worker_base_path(worker: &WorkerRef, original_path: &str) -> String {
    let base = format!("/{}", worker.name);
    if original_path == base || original_path.starts_with(&format!("{base}/")) {
        return base;
    }
    // `/name@version/...` (and `/@scope/name@version/...`) is a public address
    // too: its base keeps the version segment, otherwise the <base href> of a
    // SPA served there points at `/` and every relative asset misses.
    if let Some(rest) = original_path.strip_prefix(&format!("{base}@")) {
        let version = rest.split('/').next().unwrap_or_default();
        if !version.is_empty() {
            return format!("{base}@{version}");
        }
    }
    "/".into()
}

fn normalize_rewritten_path(remainder: &str) -> String {
    if remainder.is_empty() {
        "/".into()
    } else if remainder.starts_with('/') {
        remainder.to_string()
    } else {
        format!("/{remainder}")
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

// Only a proxy of ours sets this header, but the data plane is open: a forged
// value would otherwise flow verbatim into the <base href> of served HTML.
// Reject anything that is not a plain absolute path with a tame charset.
fn normalized_forwarded_prefix(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() || !trimmed.starts_with('/') || trimmed.contains("..") {
        return None;
    }
    let safe = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '@'));
    if !safe {
        return None;
    }
    Some(trimmed.to_string())
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

fn sanitize_internal_headers(req: &mut Request<Body>, principal: Option<&ApiKeyPrincipal>) {
    if !header_is_true(req.headers(), INTERNAL_REQUEST_HEADER) {
        return;
    }

    if principal.is_some_and(|principal| principal.is_root) {
        req.headers_mut().remove(header::AUTHORIZATION);
        req.headers_mut().remove("x-api-key");
    } else {
        req.headers_mut().remove(INTERNAL_REQUEST_HEADER);
    }
}

fn header_is_true(headers: &HeaderMap, name: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
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

    #[test]
    fn forwarded_prefix_is_normalized_and_charset_checked() {
        assert_eq!(normalized_forwarded_prefix("/apps"), Some("/apps".into()));
        assert_eq!(normalized_forwarded_prefix("/apps/"), Some("/apps".into()));
        assert_eq!(normalized_forwarded_prefix(" /apps "), Some("/apps".into()));
        assert_eq!(normalized_forwarded_prefix("/"), None);
        assert_eq!(normalized_forwarded_prefix(""), None);
        assert_eq!(normalized_forwarded_prefix("apps"), None);
        // The value lands inside served HTML: anything that could break out
        // of the <base href> attribute must be rejected, not escaped.
        assert_eq!(normalized_forwarded_prefix("/a\">x"), None);
        assert_eq!(normalized_forwarded_prefix("/../etc"), None);
    }

    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use edger_core::WorkerManifest;
    use edger_isolation::MockIsolate;
    use edger_worker::{IsolateFactory, PoolConfig};
    use tower::ServiceExt;

    use crate::auth::ControlAuth;

    #[cfg(feature = "otel")]
    use opentelemetry::propagation::Injector as _;

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
        pipeline_with_auth(ControlAuth::with_static_key("test-root"))
    }

    fn pipeline_with_auth(auth: ControlAuth) -> OrchestratorState {
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
            auth,
        }
    }

    fn auth_header() -> (&'static str, &'static str) {
        ("authorization", "Bearer test-root")
    }

    #[cfg(feature = "otel")]
    #[test]
    fn worker_header_injector_replaces_existing_trace_context() {
        let mut headers = vec![("TraceParent".into(), "stale".into())];
        WorkerHeaderInjector(&mut headers).set(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".into(),
        );

        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "TraceParent");
        assert_eq!(
            headers[0].1,
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        );
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

    #[tokio::test]
    async fn admin_session_is_open_when_no_root_key_source_exists() {
        let app = build_pipeline(pipeline_with_auth(ControlAuth::new(Default::default())));
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["principal"]["isRoot"], true);
    }

    #[tokio::test]
    async fn admin_workers_requires_configured_root_key() {
        let app = build_pipeline(pipeline_with_hello());
        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/workers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let wrong = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/workers")
                    .header("x-api-key", "wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let ok = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/workers")
                    .header("x-api-key", "test-root")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn weighted_homepage_dispatch_uses_the_cohort_version() {
        let root = tempfile::tempdir().unwrap();
        for (directory, version) in [("v1", "1.0.0"), ("v2", "2.0.0")] {
            let dir = root.path().join(directory);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("manifest.yaml"),
                format!(
                    "name: app\nversion: '{version}'\nentrypoint: index.ts\nkind: fetch\nbase: /\n"
                ),
            )
            .unwrap();
            std::fs::write(
                dir.join("index.ts"),
                "export default () => new Response('ok')",
            )
            .unwrap();
        }
        let index =
            crate::load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap();
        let policy = crate::parse_routing_policy(
            br#"{"name":"app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
        )
        .unwrap();
        crate::persist_routing_policy(&index, &policy).unwrap();
        let seen = Arc::new(std::sync::Mutex::new(None));

        struct Rec(Arc<std::sync::Mutex<Option<String>>>);
        impl IsolateFactory for Rec {
            fn create_isolate(
                &self,
                worker_ref: &edger_core::WorkerRef,
            ) -> Box<dyn edger_core::Isolate> {
                *self.0.lock().unwrap() = Some(worker_ref.version.clone());
                Box::new(MockIsolate::new())
            }
        }

        let server = ServerState::new_unready();
        server.enable_weighted_routing();
        let pool = WorkerPool::with_factory(PoolConfig::default(), Arc::new(Rec(seen.clone())));
        server.mark_ready(pool.clone());
        let state = OrchestratorState {
            server,
            pool,
            index,
            auth: ControlAuth::with_static_key("root-key"),
        };
        assert_eq!(state.index.homepage().unwrap().version, "2.0.0");
        let response = handle_request(
            &state,
            Request::builder().uri("/").body(Body::empty()).unwrap(),
            "req-home".into(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(seen.lock().unwrap().as_deref(), Some("1.0.0"));
        assert!(response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| value.to_str().unwrap_or("").starts_with("edger_cohort=")));
        assert_eq!(state.index.default_version("app"), None);
    }
}
