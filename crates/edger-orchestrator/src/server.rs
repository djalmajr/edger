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

use crate::cron::CronMetrics;
use crate::metrics::{
    cron_metrics_prometheus, http_metrics_prometheus, pool_metrics_prometheus, HttpMetrics,
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
    pool: std::sync::RwLock<Option<WorkerPool>>,
    cron_metrics: CronMetrics,
    http_metrics: HttpMetrics,
    operational_events: crate::observability::OperationalStore,
    worker_errors: crate::worker_errors::WorkerErrorLog,
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
                pool: std::sync::RwLock::new(None),
                cron_metrics: CronMetrics::default(),
                http_metrics: HttpMetrics::default(),
                operational_events: crate::observability::OperationalStore::default(),
                worker_errors: crate::worker_errors::WorkerErrorLog::default(),
            }),
        }
    }

    pub fn mark_ready(&self, pool: WorkerPool) {
        *self.inner.pool.write().expect("pool lock") = Some(pool);
        self.inner.ready.store(true, Ordering::SeqCst);
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

/// Bind and serve until the shutdown signal resolves.
pub async fn serve<S>(config: ServerConfig, app: Router, shutdown_signal: S) -> anyhow::Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    info!(%config.addr, "edger listening");
    axum::serve(listener, app)
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
