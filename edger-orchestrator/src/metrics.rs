//! Prometheus text exposition for edger runtime metrics.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use edger_worker::{PoolMetrics, WorkerState, WorkerStats};
use serde::Serialize;

use crate::cron::CronMetrics;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsStatsResponse {
    pub pool: MetricsPoolStats,
    pub workers: Vec<MetricsWorkerStats>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsPoolStats {
    pub active_requests: u64,
    pub active_workers: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub ephemeral_inflight: u64,
    pub ephemeral_queued: u64,
    pub ephemeral_rejected: u64,
    pub idle_workers: usize,
    pub request_duration_ms_last: u64,
    pub spawn_latency_ms_last: u64,
    pub spawn_latency_ms_p50: u64,
    pub terminated_total: u64,
    pub total_workers: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsWorkerStats {
    pub app: String,
    pub id: String,
    pub name: String,
    pub namespace: Option<String>,
    pub requests: u32,
    pub state: &'static str,
    pub unhealthy: bool,
    pub uptime_seconds: u64,
    pub version: String,
}

#[derive(Clone, Debug, Default)]
pub struct HttpMetrics {
    inner: Arc<HttpMetricsInner>,
}

#[derive(Debug, Default)]
struct HttpMetricsInner {
    duration_ms_last: AtomicU64,
    requests: Mutex<BTreeMap<(String, u16), u64>>,
}

impl HttpMetrics {
    pub fn record(&self, method: &str, status: u16, duration: Duration) {
        self.inner
            .duration_ms_last
            .store(duration.as_millis() as u64, Ordering::Relaxed);
        let mut requests = self.inner.requests.lock().expect("http metrics lock");
        *requests
            .entry((method.to_ascii_uppercase(), status))
            .or_default() += 1;
    }

    pub fn duration_ms_last(&self) -> u64 {
        self.inner.duration_ms_last.load(Ordering::Relaxed)
    }

    fn request_counts(&self) -> Vec<((String, u16), u64)> {
        self.inner
            .requests
            .lock()
            .expect("http metrics lock")
            .iter()
            .map(|(key, count)| (key.clone(), *count))
            .collect()
    }
}

pub fn metrics_stats_response(
    metrics: &PoolMetrics,
    workers: &[WorkerStats],
) -> MetricsStatsResponse {
    let active_requests =
        (metrics.active_workers as u64).saturating_sub(metrics.idle_workers as u64);
    MetricsStatsResponse {
        pool: MetricsPoolStats {
            active_requests,
            active_workers: metrics.active_workers,
            cache_hits: metrics.cache_hits,
            cache_misses: metrics.cache_misses,
            ephemeral_inflight: metrics.ephemeral_inflight,
            ephemeral_queued: metrics.ephemeral_queued,
            ephemeral_rejected: metrics.ephemeral_rejected,
            idle_workers: metrics.idle_workers,
            request_duration_ms_last: metrics.request_duration_ms_last,
            spawn_latency_ms_last: metrics.spawn_latency_ms_last,
            spawn_latency_ms_p50: metrics.spawn_latency_ms_p50,
            terminated_total: metrics.terminated_total,
            total_workers: metrics.active_workers,
        },
        workers: workers
            .iter()
            .map(|worker| MetricsWorkerStats {
                app: worker.app.clone(),
                id: worker.worker_id.to_string(),
                name: worker.name.clone(),
                namespace: worker.namespace.clone(),
                requests: worker.request_count,
                state: worker_state_label(worker.state),
                unhealthy: worker.unhealthy,
                uptime_seconds: worker.uptime_seconds,
                version: worker.version.clone(),
            })
            .collect(),
    }
}

pub fn pool_metrics_prometheus(metrics: &PoolMetrics) -> String {
    let total_workers = metrics.active_workers as u64;
    let idle_workers = metrics.idle_workers as u64;
    let active_requests = total_workers.saturating_sub(idle_workers);

    let mut out = String::new();
    push_metric(
        &mut out,
        "edger_pool_workers",
        "gauge",
        "Workers currently retained in the pool",
        total_workers,
    );
    push_metric(
        &mut out,
        "edger_pool_idle_workers",
        "gauge",
        "Workers currently idle in the pool",
        idle_workers,
    );
    push_metric(
        &mut out,
        "edger_pool_active_requests",
        "gauge",
        "Workers currently handling requests",
        active_requests,
    );
    push_metric(
        &mut out,
        "edger_pool_cache_hits_total",
        "counter",
        "Worker pool cache hits",
        metrics.cache_hits,
    );
    push_metric(
        &mut out,
        "edger_pool_cache_misses_total",
        "counter",
        "Worker pool cache misses",
        metrics.cache_misses,
    );
    push_metric(
        &mut out,
        "edger_pool_terminated_total",
        "counter",
        "Workers terminated since pool creation",
        metrics.terminated_total,
    );
    push_metric(
        &mut out,
        "edger_pool_spawn_latency_ms_last",
        "gauge",
        "Last observed worker spawn latency in milliseconds",
        metrics.spawn_latency_ms_last,
    );
    push_metric(
        &mut out,
        "edger_pool_spawn_latency_ms_p50",
        "gauge",
        "Median worker spawn latency from the recent in-process sample window",
        metrics.spawn_latency_ms_p50,
    );
    push_metric(
        &mut out,
        "edger_pool_request_duration_ms_last",
        "gauge",
        "Last observed worker request duration in milliseconds",
        metrics.request_duration_ms_last,
    );
    push_metric(
        &mut out,
        "edger_ephemeral_inflight",
        "gauge",
        "Ephemeral worker requests currently executing",
        metrics.ephemeral_inflight,
    );
    push_metric(
        &mut out,
        "edger_ephemeral_queued",
        "gauge",
        "Ephemeral worker requests waiting for a concurrency slot",
        metrics.ephemeral_queued,
    );
    push_metric(
        &mut out,
        "edger_ephemeral_rejected_total",
        "counter",
        "Ephemeral worker requests rejected because the queue was full",
        metrics.ephemeral_rejected,
    );
    out
}

pub fn cron_metrics_prometheus(metrics: &CronMetrics) -> String {
    let mut out = String::new();
    push_metric(
        &mut out,
        "edger_cron_executions_total",
        "counter",
        "Cron job executions completed successfully",
        metrics.executions_total(),
    );
    push_metric(
        &mut out,
        "edger_cron_failures_total",
        "counter",
        "Cron job executions that failed dispatch or returned an error status",
        metrics.failures_total(),
    );
    out
}

pub fn http_metrics_prometheus(metrics: &HttpMetrics) -> String {
    let mut out = String::new();
    out.push_str("# HELP edger_http_requests_total HTTP requests handled by the orchestrator\n");
    out.push_str("# TYPE edger_http_requests_total counter\n");
    for ((method, status), count) in metrics.request_counts() {
        out.push_str("edger_http_requests_total{method=\"");
        out.push_str(&escape_label_value(&method));
        out.push_str("\",status=\"");
        out.push_str(&status.to_string());
        out.push_str("\"} ");
        out.push_str(&count.to_string());
        out.push('\n');
    }
    out.push('\n');
    push_metric(
        &mut out,
        "edger_http_request_duration_ms_last",
        "gauge",
        "Last observed HTTP request duration in milliseconds",
        metrics.duration_ms_last(),
    );
    out
}

fn escape_label_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn worker_state_label(state: WorkerState) -> &'static str {
    match state {
        WorkerState::Creating => "creating",
        WorkerState::Ready => "ready",
        WorkerState::Active => "active",
        WorkerState::Idle => "idle",
        WorkerState::Terminating => "terminating",
        WorkerState::Terminated => "terminated",
        WorkerState::EphemeralTerm => "ephemeralTerm",
    }
}

fn push_metric(out: &mut String, name: &str, kind: &str, help: &str, value: u64) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
    out.push_str(name);
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_snapshot_contains_pool_metrics_without_secret_like_labels() {
        let output = pool_metrics_prometheus(&PoolMetrics {
            active_workers: 2,
            idle_workers: 1,
            cache_hits: 3,
            cache_misses: 4,
            ephemeral_inflight: 0,
            ephemeral_queued: 0,
            ephemeral_rejected: 1,
            request_duration_ms_last: 7,
            spawn_latency_ms_last: 5,
            spawn_latency_ms_p50: 6,
            terminated_total: 8,
            worker_queue_enqueued: 0,
            worker_queue_queued: 0,
            worker_queue_rejected: 0,
            worker_queue_timeout: 0,
            worker_queue_wait_ms_last: 0,
        });

        assert!(output.contains("# TYPE edger_pool_cache_hits_total counter"));
        assert!(output.contains("edger_pool_cache_hits_total 3"));
        assert!(output.contains("edger_pool_active_requests 1"));
        assert!(!output.to_ascii_lowercase().contains("authorization"));
        assert!(!output.to_ascii_lowercase().contains("root_api_key"));
    }

    #[test]
    fn stats_response_contains_workers_without_raw_config() {
        let response = metrics_stats_response(
            &PoolMetrics {
                active_workers: 1,
                idle_workers: 1,
                cache_hits: 2,
                cache_misses: 1,
                ephemeral_inflight: 0,
                ephemeral_queued: 0,
                ephemeral_rejected: 0,
                request_duration_ms_last: 3,
                spawn_latency_ms_last: 4,
                spawn_latency_ms_p50: 5,
                terminated_total: 0,
                worker_queue_enqueued: 0,
                worker_queue_queued: 0,
                worker_queue_rejected: 0,
                worker_queue_timeout: 0,
                worker_queue_wait_ms_last: 0,
            },
            &[WorkerStats {
                app: "echo@1.0.0".into(),
                name: "echo".into(),
                namespace: None,
                request_count: 2,
                state: WorkerState::Idle,
                unhealthy: false,
                uptime_seconds: 7,
                version: "1.0.0".into(),
                worker_id: uuid::Uuid::nil(),
            }],
        );

        let body = serde_json::to_string(&response).unwrap();
        assert!(body.contains("\"app\":\"echo@1.0.0\""));
        assert!(body.contains("\"state\":\"idle\""));
        assert!(!body.to_ascii_lowercase().contains("authorization"));
        assert!(!body.to_ascii_lowercase().contains("root_api_key"));
    }
}
