//! Native cron scheduler for manifest `cron[]` jobs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use edger_core::{
    parse_duration_string_to_ms, CoreError, CronJob, WorkerRef, INTERNAL_REQUEST_HEADER,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tower::ServiceExt;
use uuid::Uuid;

use crate::manifest_index_stub::ManifestIndex;

const DEFAULT_METHOD: &str = "GET";
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default)]
pub struct CronMetrics {
    inner: Arc<CronMetricsInner>,
}

#[derive(Debug, Default)]
struct CronMetricsInner {
    executions_total: AtomicU64,
    failures_total: AtomicU64,
}

impl CronMetrics {
    pub fn record_execution(&self) {
        self.inner.executions_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        self.inner.failures_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn executions_total(&self) -> u64 {
        self.inner.executions_total.load(Ordering::Relaxed)
    }

    pub fn failures_total(&self) -> u64 {
        self.inner.failures_total.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug)]
pub struct CronSchedulerConfig {
    root_api_key: Option<String>,
}

impl CronSchedulerConfig {
    pub fn new(root_api_key: Option<String>) -> Self {
        Self { root_api_key }
    }
}

#[derive(Clone, Debug)]
pub struct CronJobRegistration {
    pub worker: WorkerRef,
    pub job: CronJob,
    pub interval: Duration,
}

impl CronJobRegistration {
    fn new(worker: WorkerRef, job: CronJob) -> Result<Self, CoreError> {
        validate_job(&worker, &job)?;
        let interval = parse_schedule_interval(&job.schedule)?;
        Ok(Self {
            worker,
            job,
            interval,
        })
    }

    pub fn method(&self) -> &str {
        self.job.method.as_deref().unwrap_or(DEFAULT_METHOD)
    }

    pub fn route_path(&self) -> String {
        worker_route_path(&self.worker, &self.job.path)
    }
}

pub struct CronScheduler {
    shutdown_tx: watch::Sender<bool>,
    handles: Vec<JoinHandle<()>>,
}

impl CronScheduler {
    pub fn start(
        config: CronSchedulerConfig,
        registrations: Vec<CronJobRegistration>,
        app: Router,
        metrics: CronMetrics,
    ) -> Result<Self, CoreError> {
        if !registrations.is_empty() && config.root_api_key.is_none() {
            return Err(CoreError::new(
                "CRON_AUTH_MISSING",
                "ROOT_API_KEY is required when manifest cron jobs are enabled",
            ));
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut handles = Vec::with_capacity(registrations.len());

        for registration in registrations {
            let app = app.clone();
            let root_api_key = config.root_api_key.clone().unwrap_or_default();
            let metrics = metrics.clone();
            let shutdown_rx = shutdown_rx.clone();
            handles.push(tokio::spawn(run_job(
                registration,
                app,
                root_api_key,
                metrics,
                shutdown_rx,
            )));
        }

        Ok(Self {
            shutdown_tx,
            handles,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        for handle in self.handles.drain(..) {
            let mut handle = handle;
            tokio::select! {
                _ = &mut handle => {}
                _ = tokio::time::sleep(SHUTDOWN_JOIN_TIMEOUT) => {
                    handle.abort();
                    let _ = handle.await;
                }
            }
        }
    }
}

pub fn collect_cron_registrations(
    index: &ManifestIndex,
) -> Result<Vec<CronJobRegistration>, CoreError> {
    let mut registrations = Vec::new();
    for (worker, jobs) in index.enabled_cron_jobs() {
        for job in jobs {
            registrations.push(CronJobRegistration::new(worker.clone(), job)?);
        }
    }
    Ok(registrations)
}

fn validate_job(worker: &WorkerRef, job: &CronJob) -> Result<(), CoreError> {
    if job.schedule.trim().is_empty() {
        let field = format!("{}.cron.schedule", worker.name);
        return Err(CoreError::validation(&field, "schedule is required"));
    }
    if !job.path.starts_with('/') {
        let field = format!("{}.cron.path", worker.name);
        return Err(CoreError::validation(&field, "path must start with /"));
    }
    let method = job.method.as_deref().unwrap_or(DEFAULT_METHOD);
    Method::from_bytes(method.as_bytes()).map_err(|_| {
        let field = format!("{}.cron.method", worker.name);
        CoreError::validation(&field, format!("invalid HTTP method: {method}"))
    })?;
    Ok(())
}

fn parse_schedule_interval(schedule: &str) -> Result<Duration, CoreError> {
    let schedule = schedule.trim();
    if let Some(duration) = schedule.strip_prefix("@every ") {
        let Some(ms) = parse_duration_string_to_ms(duration) else {
            return Err(invalid_schedule(schedule));
        };
        if ms == 0 {
            return Err(invalid_schedule(schedule));
        }
        return Ok(Duration::from_millis(ms));
    }

    let parts = schedule.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 5 {
        return Err(invalid_schedule(schedule));
    }
    if parts[1..].iter().any(|part| *part != "*") {
        return Err(invalid_schedule(schedule));
    }

    let minutes = match parts[0] {
        "*" => 1,
        value if value.starts_with("*/") => value
            .trim_start_matches("*/")
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid_schedule(schedule))?,
        value => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value <= 59)
            .map(|_| 60)
            .ok_or_else(|| invalid_schedule(schedule))?,
    };

    Ok(Duration::from_secs(minutes * 60))
}

fn invalid_schedule(schedule: &str) -> CoreError {
    CoreError::new(
        "CRON_SCHEDULE_INVALID",
        format!("unsupported cron schedule: {schedule}"),
    )
}

async fn run_job(
    registration: CronJobRegistration,
    app: Router,
    root_api_key: String,
    metrics: CronMetrics,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(registration.interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval.tick().await;

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                break;
            }
            _ = interval.tick() => {
                if let Err(err) = dispatch_cron_request(app.clone(), &registration, &root_api_key).await {
                    metrics.record_failure();
                    tracing::warn!(
                        worker = %registration.worker.name,
                        path = %registration.job.path,
                        code = %err.code,
                        "cron dispatch failed"
                    );
                } else {
                    metrics.record_execution();
                }
            }
        }
    }
}

async fn dispatch_cron_request(
    app: Router,
    registration: &CronJobRegistration,
    root_api_key: &str,
) -> Result<(), CoreError> {
    let uri = registration.route_path();
    let request = Request::builder()
        .method(registration.method())
        .uri(uri)
        .header(INTERNAL_REQUEST_HEADER, "true")
        .header(header::AUTHORIZATION, format!("Bearer {root_api_key}"))
        .header("x-request-id", format!("cron-{}", Uuid::new_v4()))
        .body(Body::empty())
        .map_err(|err| CoreError::new("CRON_REQUEST_INVALID", err.to_string()))?;

    let response = app
        .oneshot(request)
        .await
        .map_err(|err| CoreError::new("CRON_DISPATCH_ERROR", err.to_string()))?;

    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        Ok(())
    } else {
        Err(CoreError::new(
            "CRON_DISPATCH_STATUS",
            format!("cron request returned {}", response.status()),
        ))
    }
}

fn worker_route_path(worker: &WorkerRef, job_path: &str) -> String {
    let mut route = format!("/{}@{}", worker.name, worker.version);
    if job_path != "/" {
        route.push('/');
        route.push_str(job_path.trim_start_matches('/'));
    }
    route
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker() -> WorkerRef {
        edger_core::create_worker_ref(
            "/workers/demo".into(),
            edger_core::WorkerManifest {
                name: "demo".into(),
                version: Some("1.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn schedule_parser_accepts_short_test_interval_and_minute_cron() {
        assert_eq!(
            parse_schedule_interval("@every 50ms").unwrap(),
            Duration::from_millis(50)
        );
        assert_eq!(
            parse_schedule_interval("*/2 * * * *").unwrap(),
            Duration::from_secs(120)
        );
    }

    #[test]
    fn schedule_parser_rejects_unknown_cron_shapes() {
        let err = parse_schedule_interval("0 0 * * *").unwrap_err();

        assert_eq!(err.code, "CRON_SCHEDULE_INVALID");
        assert!(err.message.contains("0 0 * * *"));
    }

    #[test]
    fn registration_builds_versioned_worker_route() {
        let registration = CronJobRegistration::new(
            worker(),
            CronJob {
                schedule: "@every 1s".into(),
                path: "/tick".into(),
                method: Some("POST".into()),
            },
        )
        .unwrap();

        assert_eq!(registration.method(), "POST");
        assert_eq!(registration.route_path(), "/demo@1.0.0/tick");
    }
}
