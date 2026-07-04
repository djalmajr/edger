//! Native cron scheduler for manifest `cron[]` jobs.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use chrono::{DateTime, Utc};
use edger_core::{
    parse_duration_string_to_ms, CoreError, CronJob, WorkerRef, INTERNAL_REQUEST_HEADER,
};
use saffron::Cron;
use tokio::sync::watch;
use tokio::task::JoinHandle;
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
    schedule: CronSchedule,
}

impl CronJobRegistration {
    fn new(worker: WorkerRef, job: CronJob) -> Result<Self, CoreError> {
        validate_job(&worker, &job)?;
        let schedule = CronSchedule::parse(&job.schedule)?;
        Ok(Self {
            worker,
            job,
            schedule,
        })
    }

    pub fn method(&self) -> &str {
        self.job.method.as_deref().unwrap_or(DEFAULT_METHOD)
    }

    pub fn route_path(&self) -> String {
        worker_route_path(&self.worker, &self.job.path)
    }
}

#[derive(Clone, Debug)]
enum CronSchedule {
    Every(Duration),
    WallClock { expression: Cron, source: String },
}

impl CronSchedule {
    fn parse(schedule: &str) -> Result<Self, CoreError> {
        let schedule = schedule.trim();
        if let Some(duration) = schedule.strip_prefix("@every ") {
            let Some(ms) = parse_duration_string_to_ms(duration) else {
                return Err(invalid_schedule(schedule));
            };
            if ms == 0 {
                return Err(invalid_schedule(schedule));
            }
            return Ok(Self::Every(Duration::from_millis(ms)));
        }

        let normalized = normalize_standard_cron(schedule)?;
        let expression = normalized
            .parse::<Cron>()
            .map_err(|_| invalid_schedule(schedule))?;
        if !expression.any() || expression.next_after(Utc::now()).is_none() {
            return Err(invalid_schedule(schedule));
        }

        Ok(Self::WallClock {
            expression,
            source: schedule.to_owned(),
        })
    }

    fn delay_after(&self, now: DateTime<Utc>) -> Result<Duration, CoreError> {
        match self {
            Self::Every(interval) => Ok(*interval),
            Self::WallClock { .. } => self
                .next_fire_after(now)?
                .signed_duration_since(now)
                .to_std()
                .map_err(|_| CoreError::new("CRON_SCHEDULE_INVALID", "cron fired in the past")),
        }
    }

    fn next_fire_after(&self, now: DateTime<Utc>) -> Result<DateTime<Utc>, CoreError> {
        match self {
            Self::Every(interval) => {
                let delta = chrono::Duration::from_std(*interval).map_err(|_| {
                    CoreError::new("CRON_SCHEDULE_INVALID", "cron interval is too large")
                })?;
                Ok(now + delta)
            }
            Self::WallClock { expression, source } => expression
                .next_after(now)
                .ok_or_else(|| invalid_schedule(source)),
        }
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
            tracing::warn!(
                "manifest cron jobs are enabled without ROOT_API_KEY or EDGER_ROOT_KEY_FILE; jobs will dispatch without Authorization and x-edger-internal will be discarded by the pipeline"
            );
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut handles = Vec::with_capacity(registrations.len());

        for registration in registrations {
            let app = app.clone();
            let root_api_key = config.root_api_key.clone();
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

fn normalize_standard_cron(schedule: &str) -> Result<String, CoreError> {
    let parts = schedule.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 5 {
        return Err(invalid_schedule(schedule));
    }

    let day_of_week =
        normalize_day_of_week_field(parts[4]).ok_or_else(|| invalid_schedule(schedule))?;
    Ok(format!(
        "{} {} {} {} {}",
        parts[0], parts[1], parts[2], parts[3], day_of_week
    ))
}

fn normalize_day_of_week_field(field: &str) -> Option<String> {
    field
        .split(',')
        .map(normalize_day_of_week_part)
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.join(","))
}

fn normalize_day_of_week_part(part: &str) -> Option<String> {
    if part.is_empty() || part == "*" || part.starts_with("*/") {
        return Some(part.to_owned());
    }
    if part.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return Some(part.to_owned());
    }

    let (base, step) = part.split_once('/').unwrap_or((part, ""));
    if step.is_empty() {
        return normalize_day_of_week_base(base);
    }

    let step = step.parse::<usize>().ok().filter(|value| *value > 0)?;
    if let Some((start, end)) = parse_day_of_week_range(base) {
        let values = (start..=end)
            .step_by(step)
            .filter_map(map_standard_day_of_week)
            .collect::<BTreeSet<_>>();
        return Some(join_day_of_week_values(values));
    }

    parse_day_of_week_number(base)
        .and_then(|value| map_standard_day_of_week(value).map(|mapped| format!("{mapped}/{step}")))
}

fn normalize_day_of_week_base(base: &str) -> Option<String> {
    if let Some((start, end)) = parse_day_of_week_range(base) {
        let values = (start..=end)
            .filter_map(map_standard_day_of_week)
            .collect::<BTreeSet<_>>();
        return Some(join_day_of_week_values(values));
    }

    parse_day_of_week_number(base)
        .and_then(map_standard_day_of_week)
        .map(|value| value.to_string())
}

fn parse_day_of_week_range(base: &str) -> Option<(u32, u32)> {
    let (start, end) = base.split_once('-')?;
    let start = parse_day_of_week_number(start)?;
    let end = parse_day_of_week_number(end)?;
    (start <= end).then_some((start, end))
}

fn parse_day_of_week_number(value: &str) -> Option<u32> {
    value.parse::<u32>().ok().filter(|value| *value <= 7)
}

fn map_standard_day_of_week(value: u32) -> Option<u32> {
    match value {
        0 | 7 => Some(1),
        1..=6 => Some(value + 1),
        _ => None,
    }
}

fn join_day_of_week_values(values: BTreeSet<u32>) -> String {
    values
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",")
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
    root_api_key: Option<String>,
    metrics: CronMetrics,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        let delay = match registration.schedule.delay_after(Utc::now()) {
            Ok(delay) => delay,
            Err(err) => {
                metrics.record_failure();
                tracing::warn!(
                    worker = %registration.worker.name,
                    path = %registration.job.path,
                    code = %err.code,
                    "cron schedule failed"
                );
                break;
            }
        };

        tokio::select! {
            _ = shutdown_rx.changed() => {
                break;
            }
            _ = tokio::time::sleep(delay) => {
                if let Err(err) =
                    dispatch_cron_request(app.clone(), &registration, root_api_key.as_deref()).await
                {
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
    root_api_key: Option<&str>,
) -> Result<(), CoreError> {
    let uri = registration.route_path();
    let mut request_builder = Request::builder()
        .method(registration.method())
        .uri(uri)
        .header(INTERNAL_REQUEST_HEADER, "true")
        .header("x-request-id", format!("cron-{}", Uuid::new_v4()));
    if let Some(root_api_key) = root_api_key {
        request_builder =
            request_builder.header(header::AUTHORIZATION, format!("Bearer {root_api_key}"));
    }
    let request = request_builder
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

    use chrono::TimeZone;

    fn datetime(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
            .unwrap()
    }

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
    fn schedule_parser_accepts_every_interval() {
        let schedule = CronSchedule::parse("@every 50ms").unwrap();

        assert!(matches!(
            schedule,
            CronSchedule::Every(d) if d == Duration::from_millis(50)
        ));
    }

    #[test]
    fn schedule_parser_accepts_standard_midnight_cron() {
        let schedule = CronSchedule::parse("0 0 * * *").unwrap();
        let next = schedule
            .next_fire_after(datetime(2024, 1, 1, 23, 59, 30))
            .unwrap();

        assert_eq!(next, datetime(2024, 1, 2, 0, 0, 0));
    }

    #[test]
    fn schedule_parser_accepts_standard_monday_day_of_week() {
        let schedule = CronSchedule::parse("0 9 * * 1").unwrap();
        let next = schedule
            .next_fire_after(datetime(2024, 1, 7, 8, 0, 0))
            .unwrap();

        assert_eq!(next, datetime(2024, 1, 8, 9, 0, 0));
    }

    #[test]
    fn schedule_parser_accepts_five_minute_step_cron() {
        let schedule = CronSchedule::parse("*/5 * * * *").unwrap();
        let next = schedule
            .next_fire_after(datetime(2024, 1, 1, 0, 2, 30))
            .unwrap();

        assert_eq!(next, datetime(2024, 1, 1, 0, 5, 0));
    }

    #[test]
    fn schedule_parser_rejects_invalid_cron_expression() {
        let err = CronSchedule::parse("not a cron").unwrap_err();

        assert_eq!(err.code, "CRON_SCHEDULE_INVALID");
        assert!(err.message.contains("not a cron"));
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
