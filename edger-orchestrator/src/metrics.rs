//! Prometheus text exposition for edger runtime metrics.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use edger_worker::metrics::{WorkerGroupMetrics, WorkerHealthMetrics, WorkerProcessMetrics};
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
    pub active_processes: usize,
    pub app: String,
    pub health: MetricsWorkerHealthStats,
    pub id: String,
    pub idle_processes: usize,
    pub max_processes: usize,
    pub name: String,
    pub namespace: Option<String>,
    pub processes: Vec<MetricsWorkerProcessStats>,
    pub queued: u64,
    pub recycle: MetricsWorkerRecycleStats,
    pub rejected_total: u64,
    pub request_duration_ms_last: u64,
    pub request_duration_ms_p95: u64,
    pub request_total: u64,
    pub requests: u32,
    pub state: &'static str,
    pub terminating_processes: usize,
    pub timeout_total: u64,
    pub total_processes: usize,
    pub unhealthy: bool,
    pub uptime_seconds: u64,
    pub version: String,
    pub wait_ms: u64,
    pub wait_ms_p50: u64,
    pub wait_ms_p95: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsWorkerHealthStats {
    pub consecutive_failures: u64,
    pub failure_count: u64,
    pub last_failure_at_ms: Option<u64>,
    pub last_failure_code: Option<String>,
    pub last_success_at_ms: Option<u64>,
    pub observed_at_ms: Option<u64>,
    pub sample_count: u64,
    pub status: &'static str,
    pub success_count: u64,
    pub window_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsWorkerProcessStats {
    pub requests: u32,
    pub state: &'static str,
    pub unhealthy: bool,
    pub uptime_seconds: u64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsWorkerRecycleStats {
    pub error: u64,
    pub max_requests: u64,
    pub oom_shutdown: u64,
    pub ttl: u64,
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
        workers: metrics_worker_stats(metrics, workers),
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
    push_worker_group_metrics(&mut out, &metrics.worker_groups);
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

fn metrics_worker_stats(metrics: &PoolMetrics, workers: &[WorkerStats]) -> Vec<MetricsWorkerStats> {
    if metrics.worker_groups.is_empty() {
        return workers
            .iter()
            .map(metrics_worker_stats_from_instance)
            .collect();
    }

    metrics
        .worker_groups
        .iter()
        .map(|group| metrics_worker_stats_from_group(group, workers))
        .collect()
}

fn metrics_worker_stats_from_group(
    group: &WorkerGroupMetrics,
    workers: &[WorkerStats],
) -> MetricsWorkerStats {
    let matching_worker = workers.iter().find(|worker| {
        worker.name == group.name
            && worker.version == group.version
            && worker.namespace == group.namespace
    });
    let requests = group
        .processes
        .iter()
        .map(|process| process.request_count)
        .sum();
    let uptime_seconds = group
        .processes
        .iter()
        .map(|process| process.uptime_seconds)
        .max()
        .unwrap_or(0);
    MetricsWorkerStats {
        active_processes: group.active_processes,
        app: format!("{}@{}", group.name, group.version),
        health: metrics_health_stats(&group.health),
        id: matching_worker
            .map(|worker| worker.worker_id.to_string())
            .unwrap_or_default(),
        idle_processes: group.idle_processes,
        max_processes: group.max_processes,
        name: group.name.clone(),
        namespace: group.namespace.clone(),
        processes: group.processes.iter().map(metrics_process_stats).collect(),
        queued: group.queued,
        recycle: MetricsWorkerRecycleStats {
            error: group.recycle_error_total,
            max_requests: group.recycle_max_requests_total,
            oom_shutdown: group.recycle_oom_shutdown_total,
            ttl: group.recycle_ttl_total,
        },
        rejected_total: group.rejected_total,
        request_duration_ms_last: group.request_duration_ms_last,
        request_duration_ms_p95: group.request_duration_ms_p95,
        request_total: group.request_total,
        requests,
        state: worker_group_state_label(group),
        terminating_processes: group.terminating_processes,
        timeout_total: group.timeout_total,
        total_processes: group.total_processes,
        unhealthy: group.processes.iter().any(|process| process.unhealthy),
        uptime_seconds,
        version: group.version.clone(),
        wait_ms: group.wait_ms_last,
        wait_ms_p50: group.wait_ms_p50,
        wait_ms_p95: group.wait_ms_p95,
    }
}

fn metrics_worker_stats_from_instance(worker: &WorkerStats) -> MetricsWorkerStats {
    let process = WorkerProcessMetrics {
        request_count: worker.request_count,
        state: worker.state,
        unhealthy: worker.unhealthy,
        uptime_seconds: worker.uptime_seconds,
    };
    MetricsWorkerStats {
        active_processes: usize::from(worker.state == WorkerState::Active),
        app: worker.app.clone(),
        health: metrics_health_stats(&WorkerHealthMetrics::default()),
        id: worker.worker_id.to_string(),
        idle_processes: usize::from(worker.state == WorkerState::Idle),
        max_processes: 1,
        name: worker.name.clone(),
        namespace: worker.namespace.clone(),
        processes: vec![metrics_process_stats(&process)],
        queued: 0,
        recycle: MetricsWorkerRecycleStats::default(),
        rejected_total: 0,
        request_duration_ms_last: 0,
        request_duration_ms_p95: 0,
        request_total: worker.request_count as u64,
        requests: worker.request_count,
        state: worker_state_label(worker.state),
        terminating_processes: usize::from(worker.state == WorkerState::Terminating),
        timeout_total: 0,
        total_processes: 1,
        unhealthy: worker.unhealthy,
        uptime_seconds: worker.uptime_seconds,
        version: worker.version.clone(),
        wait_ms: 0,
        wait_ms_p50: 0,
        wait_ms_p95: 0,
    }
}

fn metrics_health_stats(health: &WorkerHealthMetrics) -> MetricsWorkerHealthStats {
    MetricsWorkerHealthStats {
        consecutive_failures: health.consecutive_failures,
        failure_count: health.failure_count,
        last_failure_at_ms: health.last_failure_at_ms,
        last_failure_code: health.last_failure_code.clone(),
        last_success_at_ms: health.last_success_at_ms,
        observed_at_ms: health.observed_at_ms,
        sample_count: health.sample_count,
        status: health.status.label(),
        success_count: health.success_count,
        window_ms: health.window_ms,
    }
}

fn metrics_process_stats(process: &WorkerProcessMetrics) -> MetricsWorkerProcessStats {
    MetricsWorkerProcessStats {
        requests: process.request_count,
        state: worker_state_label(process.state),
        unhealthy: process.unhealthy,
        uptime_seconds: process.uptime_seconds,
    }
}

fn worker_group_state_label(group: &WorkerGroupMetrics) -> &'static str {
    if group.active_processes > 0 {
        "active"
    } else if group.idle_processes > 0 && group.idle_processes == group.total_processes {
        "idle"
    } else if group.total_processes == 0 {
        "absent"
    } else if group
        .processes
        .iter()
        .any(|process| process.state == WorkerState::Terminating)
    {
        "terminating"
    } else {
        "ready"
    }
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

fn push_worker_group_metrics(out: &mut String, groups: &[WorkerGroupMetrics]) {
    if groups.is_empty() {
        return;
    }

    push_metric_header(
        out,
        "edger_worker_processes",
        "gauge",
        "Worker processes by state for each worker group",
    );
    for group in groups {
        push_worker_sample(
            out,
            "edger_worker_processes",
            group,
            &[("state", "total")],
            group.total_processes as u64,
        );
        push_worker_sample(
            out,
            "edger_worker_processes",
            group,
            &[("state", "active")],
            group.active_processes as u64,
        );
        push_worker_sample(
            out,
            "edger_worker_processes",
            group,
            &[("state", "idle")],
            group.idle_processes as u64,
        );
        push_worker_sample(
            out,
            "edger_worker_processes",
            group,
            &[("state", "terminating")],
            group.terminating_processes as u64,
        );
    }
    out.push('\n');

    push_worker_metric(
        out,
        "edger_worker_queue_depth",
        "gauge",
        "Persistent-worker requests currently waiting for this worker group",
        groups,
        |group| group.queued,
    );
    push_worker_metric(
        out,
        "edger_worker_queue_enqueued_total",
        "counter",
        "Persistent-worker requests admitted into this worker group's wait queue",
        groups,
        |group| group.enqueued_total,
    );
    push_worker_metric(
        out,
        "edger_worker_queue_rejected_total",
        "counter",
        "Persistent-worker requests rejected because this worker group's queue was full",
        groups,
        |group| group.rejected_total,
    );
    push_worker_metric(
        out,
        "edger_worker_queue_timeout_total",
        "counter",
        "Persistent-worker requests that timed out waiting for this worker group",
        groups,
        |group| group.timeout_total,
    );
    push_worker_metric(
        out,
        "edger_worker_queue_wait_ms_last",
        "gauge",
        "Last persistent-worker queue wait for this worker group in milliseconds",
        groups,
        |group| group.wait_ms_last,
    );
    push_worker_metric(
        out,
        "edger_worker_queue_wait_ms_p50",
        "gauge",
        "Median persistent-worker queue wait from the recent in-process sample window",
        groups,
        |group| group.wait_ms_p50,
    );
    push_worker_metric(
        out,
        "edger_worker_queue_wait_ms_p95",
        "gauge",
        "95th percentile persistent-worker queue wait from the recent in-process sample window",
        groups,
        |group| group.wait_ms_p95,
    );

    push_metric_header(
        out,
        "edger_worker_recycle_total",
        "counter",
        "Worker processes recycled by cause for each worker group",
    );
    for group in groups {
        push_worker_sample(
            out,
            "edger_worker_recycle_total",
            group,
            &[("cause", "ttl")],
            group.recycle_ttl_total,
        );
        push_worker_sample(
            out,
            "edger_worker_recycle_total",
            group,
            &[("cause", "max_requests")],
            group.recycle_max_requests_total,
        );
        push_worker_sample(
            out,
            "edger_worker_recycle_total",
            group,
            &[("cause", "error")],
            group.recycle_error_total,
        );
        push_worker_sample(
            out,
            "edger_worker_recycle_total",
            group,
            &[("cause", "oom_shutdown")],
            group.recycle_oom_shutdown_total,
        );
    }
    out.push('\n');
}

fn push_worker_metric<F>(
    out: &mut String,
    name: &str,
    kind: &str,
    help: &str,
    groups: &[WorkerGroupMetrics],
    value: F,
) where
    F: Fn(&WorkerGroupMetrics) -> u64,
{
    push_metric_header(out, name, kind, help);
    for group in groups {
        push_worker_sample(out, name, group, &[], value(group));
    }
    out.push('\n');
}

fn push_worker_sample(
    out: &mut String,
    name: &str,
    group: &WorkerGroupMetrics,
    extra_labels: &[(&str, &str)],
    value: u64,
) {
    out.push_str(name);
    out.push_str("{worker=\"");
    out.push_str(&escape_label_value(&group.name));
    out.push_str("\",version=\"");
    out.push_str(&escape_label_value(&group.version));
    out.push_str("\",namespace=\"");
    out.push_str(&escape_label_value(
        group.namespace.as_deref().unwrap_or(""),
    ));
    out.push('"');
    for (label, label_value) in extra_labels {
        out.push(',');
        out.push_str(label);
        out.push_str("=\"");
        out.push_str(&escape_label_value(label_value));
        out.push('"');
    }
    out.push_str("} ");
    out.push_str(&value.to_string());
    out.push('\n');
}

fn push_metric_header(out: &mut String, name: &str, kind: &str, help: &str) {
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
}

fn push_metric(out: &mut String, name: &str, kind: &str, help: &str, value: u64) {
    push_metric_header(out, name, kind, help);
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
            worker_groups: Vec::new(),
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
                worker_groups: Vec::new(),
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
