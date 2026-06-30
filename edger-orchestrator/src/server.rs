//! HTTP server — health/readiness probes and request tracing (story 05.01).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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

use crate::metrics::pool_metrics_prometheus;

/// Listener configuration (addr from `PORT` env in the binary).
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub addr: SocketAddr,
}

impl ServerConfig {
    pub fn from_port(port: u16) -> Self {
        Self {
            addr: SocketAddr::from(([0, 0, 0, 0], port)),
        }
    }
}

struct ServerStateInner {
    ready: AtomicBool,
    pool: std::sync::RwLock<Option<WorkerPool>>,
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

    pub fn shutdown_pool(&self) {
        if let Some(pool) = self.inner.pool.read().expect("pool lock").as_ref() {
            pool.shutdown();
        }
    }

    pub fn pool_metrics(&self) -> Option<edger_worker::PoolMetrics> {
        self.inner
            .pool
            .read()
            .expect("pool lock")
            .as_ref()
            .map(WorkerPool::get_metrics)
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
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        pool_metrics_prometheus(&metrics),
    )
}

pub fn request_id_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

pub async fn request_id_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let mut response = next.run(req).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

/// Build the axum router with health/readiness routes and tracing middleware.
pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/healthz", get(health))
        .route("/livez", get(live))
        .route("/metrics", get(metrics))
        .route("/ready", get(ready))
        .route("/readyz", get(ready))
        .layer(middleware::from_fn(request_id_middleware))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Bind and serve until the listener stops (graceful shutdown wired in the binary).
pub async fn serve(config: ServerConfig, app: Router) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    info!(%config.addr, "edger listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Parse `PORT` env (default 3000) for Buntime-compatible binding.
pub fn port_from_env() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn unready_state_is_not_ready() {
        let state = ServerState::new_unready();
        assert!(!state.is_ready());
    }
}
