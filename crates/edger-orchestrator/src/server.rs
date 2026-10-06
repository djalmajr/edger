//! HTTP server — health/readiness probes and request tracing (story 05.01).

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use edger_worker::WorkerPool;
use serde_json::json;
use tower_http::trace::TraceLayer;
use tracing::info;
use uuid::Uuid;

use crate::compression::CompressionConfig;
use crate::cron::CronMetrics;
use crate::metrics::{
    cron_metrics_prometheus, http_metrics_prometheus, pool_metrics_prometheus,
    stream_detach_metrics_prometheus, tenant_routing_metrics_prometheus, CompressionMetrics,
    HttpMetrics, TenantRoutingMetrics,
};

/// Listener configuration (addr from `PORT` env in the binary).
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub addr: SocketAddr,
}

impl ServerConfig {
    pub fn from_port(port: u16) -> Self {
        Self::from_bind(IpAddr::from([0, 0, 0, 0]), port)
    }

    /// Build the listener config from an explicit IP (e.g. `EDGER_BIND`) and port.
    pub fn from_bind(ip: IpAddr, port: u16) -> Self {
        Self {
            addr: SocketAddr::new(ip, port),
        }
    }
}

struct ServerStateInner {
    ready: AtomicBool,
    tenant_routing_enabled: AtomicBool,
    weighted_routing_enabled: AtomicBool,
    pool: std::sync::RwLock<Option<WorkerPool>>,
    tenant_identity: std::sync::RwLock<Option<crate::tenant_identity::TenantIdentityClient>>,
    cron_metrics: CronMetrics,
    http_metrics: HttpMetrics,
    tenant_routing_metrics: TenantRoutingMetrics,
    /// Data-plane compression settings (EDG-6). Set once before
    /// `build_pipeline` (first call wins); read at pipeline assembly.
    compression_config: std::sync::OnceLock<CompressionConfig>,
    /// Compression byte counters (EDG-6): shared by the pipeline layers and
    /// rendered by `/metrics`.
    compression_metrics: CompressionMetrics,
    operational_events: crate::observability::OperationalStore,
    worker_errors: crate::worker_errors::WorkerErrorLog,
    /// Process-wide stream-detach budget (multiproc backend only): its
    /// `StreamDetachStats` snapshot feeds the `/metrics` block. `None` when
    /// the backend does not run the multiproc detach pipeline — the block
    /// stays absent.
    detach_budget: std::sync::OnceLock<std::sync::Arc<edger_isolation::StreamDetachBudget>>,
}

/// Shared application state for health/readiness and future pipeline wiring.
#[derive(Clone)]
pub struct ServerState {
    inner: Arc<ServerStateInner>,
}

impl ServerState {
    pub fn new_unready() -> Self {
        Self {
            inner: Arc::new(ServerStateInner {
                ready: AtomicBool::new(false),
                tenant_routing_enabled: AtomicBool::new(false),
                weighted_routing_enabled: AtomicBool::new(false),
                pool: std::sync::RwLock::new(None),
                tenant_identity: std::sync::RwLock::new(None),
                cron_metrics: CronMetrics::default(),
                http_metrics: HttpMetrics::default(),
                tenant_routing_metrics: TenantRoutingMetrics::default(),
                compression_config: std::sync::OnceLock::new(),
                compression_metrics: CompressionMetrics::default(),
                operational_events: crate::observability::OperationalStore::default(),
                worker_errors: crate::worker_errors::WorkerErrorLog::default(),
                detach_budget: std::sync::OnceLock::new(),
            }),
        }
    }

    pub fn mark_ready(&self, pool: WorkerPool) {
        *self.inner.pool.write().expect("pool lock") = Some(pool);
        self.inner.ready.store(true, Ordering::SeqCst);
    }

    /// Register the process-wide stream-detach budget (multiproc backend
    /// only). Setting it twice is a no-op: the first budget wins.
    pub fn set_stream_detach_budget(
        &self,
        budget: std::sync::Arc<edger_isolation::StreamDetachBudget>,
    ) {
        let _ = self.inner.detach_budget.set(budget);
    }

    /// Snapshot of the stream-detach counters, or `None` when the backend
    /// does not run the multiproc detach pipeline (the `/metrics` block is
    /// then omitted).
    pub fn stream_detach_stats(&self) -> Option<edger_isolation::StreamDetachStats> {
        self.inner.detach_budget.get().map(|budget| budget.stats())
    }

    pub fn set_tenant_identity_client(&self, client: crate::tenant_identity::TenantIdentityClient) {
        *self
            .inner
            .tenant_identity
            .write()
            .expect("tenant identity lock") = Some(client);
    }

    pub fn enable_tenant_routing(&self) {
        self.inner
            .tenant_routing_enabled
            .store(true, Ordering::SeqCst);
    }

    pub fn tenant_routing_enabled(&self) -> bool {
        self.inner.tenant_routing_enabled.load(Ordering::SeqCst)
    }

    pub fn enable_weighted_routing(&self) {
        self.inner
            .weighted_routing_enabled
            .store(true, Ordering::SeqCst);
    }

    pub fn weighted_routing_enabled(&self) -> bool {
        self.inner.weighted_routing_enabled.load(Ordering::SeqCst)
    }

    pub fn tenant_identity_client(&self) -> Option<crate::tenant_identity::TenantIdentityClient> {
        self.inner
            .tenant_identity
            .read()
            .expect("tenant identity lock")
            .clone()
    }

    pub fn is_ready(&self) -> bool {
        self.inner.ready.load(Ordering::SeqCst)
            && self.inner.pool.read().expect("pool lock").is_some()
    }

    pub fn shutdown_pool(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.inner
            .pool
            .read()
            .expect("pool lock")
            .as_ref()
            .and_then(|pool| pool.shutdown())
    }

    pub fn pool_metrics(&self) -> Option<edger_worker::PoolMetrics> {
        self.inner
            .pool
            .read()
            .expect("pool lock")
            .as_ref()
            .map(WorkerPool::get_metrics)
    }

    pub fn cron_metrics(&self) -> CronMetrics {
        self.inner.cron_metrics.clone()
    }

    pub fn http_metrics(&self) -> HttpMetrics {
        self.inner.http_metrics.clone()
    }

    /// Set the data-plane compression settings (EDG-6). Must be called
    /// BEFORE `build_pipeline`: the first call wins, later calls are
    /// ignored (the pipeline reads the snapshot once at assembly).
    pub fn set_compression_config(&self, config: CompressionConfig) {
        let _ = self.inner.compression_config.set(config);
    }

    /// The effective data-plane compression settings (EDG-6); the default
    /// config when `set_compression_config` was never called.
    pub fn compression_config(&self) -> CompressionConfig {
        self.inner
            .compression_config
            .get()
            .copied()
            .unwrap_or_default()
    }

    pub fn compression_metrics(&self) -> CompressionMetrics {
        self.inner.compression_metrics.clone()
    }

    pub fn tenant_routing_metrics(&self) -> TenantRoutingMetrics {
        self.inner.tenant_routing_metrics.clone()
    }

    pub fn worker_errors(&self) -> crate::worker_errors::WorkerErrorLog {
        self.inner.worker_errors.clone()
    }

    pub fn operational_events(&self) -> crate::observability::OperationalStore {
        self.inner.operational_events.clone()
    }
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

async fn live() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "live" })))
}

async fn ready(State(state): State<ServerState>) -> impl IntoResponse {
    if state.is_ready() {
        (StatusCode::OK, Json(json!({ "status": "ready" })))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready" })),
        )
    }
}

async fn metrics(State(state): State<ServerState>) -> impl IntoResponse {
    let metrics = state.pool_metrics().unwrap_or_default();
    let mut body = pool_metrics_prometheus(&metrics);
    body.push_str(&cron_metrics_prometheus(&state.cron_metrics()));
    body.push_str(&http_metrics_prometheus(&state.http_metrics()));
    body.push_str(&crate::metrics::compression_metrics_prometheus(
        &state.compression_metrics(),
    ));
    body.push_str(&tenant_routing_metrics_prometheus(
        &state.tenant_routing_metrics(),
    ));
    if let Some(stats) = state.stream_detach_stats() {
        body.push_str(&stream_detach_metrics_prometheus(&stats));
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

pub fn request_id_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

pub async fn request_id_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let mut req = req;
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    if let Ok(value) = HeaderValue::from_str(&request_id) {
        req.headers_mut().insert("x-request-id", value.clone());
        let mut response = next.run(req).await;
        response.headers_mut().insert("x-request-id", value);
        response
    } else {
        next.run(req).await
    }
}

pub async fn request_metrics_middleware(
    State(state): State<ServerState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = req.method().as_str().to_string();
    let started = Instant::now();
    let response = next.run(req).await;
    state
        .http_metrics()
        .record(&method, response.status().as_u16(), started.elapsed());
    response
}

/// Build the axum router with health/readiness routes and tracing middleware.
pub fn router(state: ServerState) -> Router {
    let metrics_state = state.clone();
    Router::new()
        .route("/health", get(health))
        .route("/healthz", get(health))
        .route("/livez", get(live))
        .route("/metrics", get(metrics))
        .route("/ready", get(ready))
        .route("/readyz", get(ready))
        .layer(middleware::from_fn_with_state(
            metrics_state,
            request_metrics_middleware,
        ))
        .layer(middleware::from_fn(request_id_middleware))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Bind and serve until the shutdown signal resolves. `into_make_service_with_connect_info` expõe
/// o IP REAL da conexão (peer do listener) aos handlers como `ConnectInfo` —
/// é a única fonte de IP confiável: headers de cliente (`X-Forwarded-For`,
/// `X-Real-IP`) nunca são usados para rate limit de credencial.
pub async fn serve<S>(config: ServerConfig, app: Router, shutdown_signal: S) -> anyhow::Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    info!(%config.addr, "edger listening");
    let make_service = app.into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, make_service)
        .with_graceful_shutdown(shutdown_signal)
        .await?;
    Ok(())
}

/// Parse `PORT` env (default 3000) for Buntime-compatible binding.
pub fn port_from_env() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000)
}

/// Parse the `EDGER_BIND` value into a listening IP (default `0.0.0.0`).
pub fn parse_bind_ip(value: Option<&str>) -> Result<std::net::IpAddr, String> {
    match value {
        Some(raw) if !raw.trim().is_empty() => raw.trim().parse().map_err(|_| {
            format!("EDGER_BIND must be an IP address such as 127.0.0.1 or 0.0.0.0, got {raw:?}")
        }),
        _ => Ok(IpAddr::from([0, 0, 0, 0])),
    }
}

/// Parse the `EDGER_BIND` value (raw env bytes) into a listening IP (default `0.0.0.0`).
/// Fails when the variable is set to bytes that are not valid UTF-8 text.
pub fn parse_bind_os(value: Option<&std::ffi::OsStr>) -> Result<IpAddr, String> {
    match value {
        None => parse_bind_ip(None),
        Some(v) => match v.to_str() {
            Some(s) => parse_bind_ip(Some(s)),
            None => Err(format!(
                "EDGER_BIND must be valid UTF-8 text with an IP address such as 127.0.0.1 or 0.0.0.0, got {v:?}"
            )),
        },
    }
}

/// Read the listening IP from the `EDGER_BIND` env (default `0.0.0.0`).
pub fn bind_ip_from_env() -> Result<IpAddr, String> {
    parse_bind_os(std::env::var_os("EDGER_BIND").as_deref())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn unready_state_is_not_ready() {
        let state = ServerState::new_unready();
        assert!(!state.is_ready());
    }

    #[test]
    fn parse_bind_ip_defaults_to_wildcard() {
        let wildcard = IpAddr::from([0, 0, 0, 0]);
        assert_eq!(parse_bind_ip(None).unwrap(), wildcard);
        assert_eq!(parse_bind_ip(Some("")).unwrap(), wildcard);
        assert_eq!(parse_bind_ip(Some("  ")).unwrap(), wildcard);
    }

    #[test]
    fn parse_bind_ip_accepts_ipv4_and_ipv6() {
        assert_eq!(
            parse_bind_ip(Some("127.0.0.1")).unwrap(),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            parse_bind_ip(Some("::1")).unwrap(),
            "::1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            parse_bind_ip(Some(" 0.0.0.0 ")).unwrap(),
            IpAddr::from([0, 0, 0, 0])
        );
    }

    #[test]
    fn parse_bind_ip_rejects_hostnames() {
        let err = parse_bind_ip(Some("localhost")).unwrap_err();
        assert!(err.contains("EDGER_BIND"), "got: {err}");
        assert!(err.contains("localhost"), "got: {err}");
    }

    #[test]
    fn parse_bind_os_maps_none_to_wildcard() {
        assert_eq!(parse_bind_os(None).unwrap(), IpAddr::from([0, 0, 0, 0]));
    }

    #[test]
    fn parse_bind_os_accepts_utf8_ip() {
        assert_eq!(
            parse_bind_os(Some(std::ffi::OsStr::new("127.0.0.1"))).unwrap(),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn parse_bind_ip_accepts_ipv6_unspecified() {
        assert_eq!(parse_bind_ip(Some("::")).unwrap(), IpAddr::from([0u16; 8]));
    }

    #[test]
    #[cfg(unix)]
    fn parse_bind_os_rejects_non_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let err = parse_bind_os(Some(std::ffi::OsStr::from_bytes(b"\xff"))).unwrap_err();
        assert!(err.contains("EDGER_BIND"), "got: {err}");
        assert!(err.contains("UTF-8"), "got: {err}");
    }

    #[test]
    fn from_bind_builds_socket_addr_from_ip_and_port() {
        let config = ServerConfig::from_bind("127.0.0.1".parse::<IpAddr>().unwrap(), 19080);
        assert_eq!(config.addr.to_string(), "127.0.0.1:19080");
    }
}
