use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};
use edger_core::{WorkerManifest, WorkerRef};
use edger_orchestrator::{
    build_pipeline, ControlAuth, ManifestIndex, OrchestratorState, ServerState,
};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool};
use tokio::task::JoinSet;
use tower::ServiceExt;

#[derive(Clone)]
struct PerfFactory {
    delay: Duration,
}

impl IsolateFactory for PerfFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        Box::new(PerfIsolate { delay: self.delay })
    }
}

struct PerfIsolate {
    delay: Duration,
}

#[async_trait]
impl Isolate for PerfIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: Some(Bytes::from_static(b"ok")),
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: Some(Bytes::from_static(b"<html></html>")),
        })
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ConcurrentScenario {
    concurrency: usize,
    delay: Duration,
    max_processes: usize,
    queue_limit: usize,
    queue_timeout: &'static str,
    requests: usize,
    scenario: &'static str,
}

struct ConcurrentScenarioReport {
    active_processes: usize,
    active_workers: usize,
    ok: usize,
    p50: Duration,
    p95: Duration,
    queued: u64,
    rejected: u64,
    requests: usize,
    throughput_rps: f64,
    timed_out: u64,
    total_processes: usize,
    wait_ms_p95: u64,
}

fn perf_state(manifest: WorkerManifest, factory: Arc<dyn IsolateFactory>) -> OrchestratorState {
    let mut index = ManifestIndex::new();
    index
        .insert(PathBuf::from("/workers/perf-echo"), manifest)
        .unwrap();

    let server = ServerState::new_unready();
    let pool = WorkerPool::with_factory(PoolConfig::default(), factory);
    server.mark_ready(pool.clone());

    OrchestratorState {
        server,
        pool,
        index,
        auth: ControlAuth::with_static_key("test-root"),
    }
}

fn perf_manifest(max_processes: usize, queue_limit: usize, queue_timeout: &str) -> WorkerManifest {
    WorkerManifest {
        name: "perf-echo".into(),
        version: Some("1.0.0".into()),
        ttl: Some(serde_yaml::Value::String("30s".into())),
        max_processes: Some(max_processes),
        queue_limit: Some(queue_limit),
        queue_timeout: Some(serde_yaml::Value::String(queue_timeout.into())),
        ..Default::default()
    }
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    let index = ((sorted.len() * percentile).saturating_sub(1) / 100).min(sorted.len() - 1);
    sorted[index]
}

fn perf_request() -> Request<Body> {
    Request::builder()
        .uri("/perf-echo")
        .header("authorization", "Bearer test-root")
        .body(Body::empty())
        .unwrap()
}

async fn run_concurrent_scenario(scenario: ConcurrentScenario) -> ConcurrentScenarioReport {
    let state = perf_state(
        perf_manifest(
            scenario.max_processes,
            scenario.queue_limit,
            scenario.queue_timeout,
        ),
        Arc::new(PerfFactory {
            delay: scenario.delay,
        }),
    );
    let pool = state.pool.clone();
    let app = build_pipeline(state);
    let started_all = Instant::now();
    let mut durations = Vec::with_capacity(scenario.requests);
    let mut ok = 0;
    let mut rejected_statuses = 0;

    for _ in (0..scenario.requests).step_by(scenario.concurrency) {
        let mut batch = JoinSet::new();
        let batch_size = scenario
            .concurrency
            .min(scenario.requests.saturating_sub(durations.len()));

        for _ in 0..batch_size {
            let app = app.clone();
            batch.spawn(async move {
                let started = Instant::now();
                let status = app.oneshot(perf_request()).await.unwrap().status();
                (started.elapsed(), status)
            });
        }

        while let Some(result) = batch.join_next().await {
            let (duration, status) = result.unwrap();
            durations.push(duration);
            match status {
                StatusCode::OK => ok += 1,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                    rejected_statuses += 1;
                }
                other => panic!("unexpected status in perf harness: {other}"),
            }
        }
    }

    durations.sort_unstable();
    let p50 = percentile(&durations, 50);
    let p95 = percentile(&durations, 95);
    let elapsed = started_all.elapsed();
    let metrics = pool.get_metrics();
    let group = metrics
        .worker_groups
        .iter()
        .find(|group| group.name == "perf-echo");
    let rejected = metrics.worker_queue_rejected + metrics.worker_queue_timeout;
    assert_eq!(
        ok + rejected_statuses,
        scenario.requests,
        "every request must finish as an accepted or capacity-limited response"
    );
    assert_eq!(
        rejected_statuses as u64, rejected,
        "HTTP capacity responses must match pool queue rejection counters"
    );

    ConcurrentScenarioReport {
        active_processes: group
            .map(|group| group.active_processes)
            .unwrap_or_default(),
        active_workers: metrics.active_workers,
        ok,
        p50,
        p95,
        queued: metrics.worker_queue_enqueued,
        rejected: metrics.worker_queue_rejected,
        requests: scenario.requests,
        throughput_rps: scenario.requests as f64 / elapsed.as_secs_f64(),
        timed_out: metrics.worker_queue_timeout,
        total_processes: group.map(|group| group.total_processes).unwrap_or_default(),
        wait_ms_p95: group.map(|group| group.wait_ms_p95).unwrap_or_default(),
    }
}

fn print_concurrent_report(scenario: ConcurrentScenario, report: ConcurrentScenarioReport) {
    println!(
        "PERF scenario={} requests={} concurrency={} maxProcesses={} p50_ms={} p95_ms={} throughput_rps={:.2} queued={} wait_ms_p95={} rejected={} timeouts={} ok={} active_processes={} total_processes={} active_workers={}",
        scenario.scenario,
        report.requests,
        scenario.concurrency,
        scenario.max_processes,
        report.p50.as_millis(),
        report.p95.as_millis(),
        report.throughput_rps,
        report.queued,
        report.wait_ms_p95,
        report.rejected,
        report.timed_out,
        report.ok,
        report.active_processes,
        report.total_processes,
        report.active_workers
    );
}

#[tokio::test]
#[ignore = "performance harness; run explicitly with cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture"]
async fn persistent_worker_warm_hit_baseline() {
    let state = perf_state(
        perf_manifest(1, 8, "1s"),
        Arc::new(PerfFactory {
            delay: Duration::ZERO,
        }),
    );
    let pool = state.pool.clone();
    let app = build_pipeline(state);
    let mut durations = Vec::with_capacity(50);

    for _ in 0..50 {
        let started = Instant::now();
        let response = app.clone().oneshot(perf_request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        durations.push(started.elapsed());
    }

    durations.sort_unstable();
    let p50 = percentile(&durations, 50);
    let p95 = percentile(&durations, 95);
    let metrics = pool.get_metrics();

    println!(
        "perf_harness scenario=persistent_worker_warm_hit requests=50 p50_us={} p95_us={} cache_hits={} cache_misses={} active_workers={}",
        p50.as_micros(),
        p95.as_micros(),
        metrics.cache_hits,
        metrics.cache_misses,
        metrics.active_workers
    );

    assert_eq!(metrics.cache_misses, 1);
    assert_eq!(metrics.cache_hits, 49);
    assert_eq!(metrics.active_workers, 1);
}

#[tokio::test]
#[ignore = "performance harness; run explicitly with cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture"]
async fn concurrent_worker_pool_1_vs_n_scale_harness() {
    let scenarios = [
        ConcurrentScenario {
            concurrency: 8,
            delay: Duration::from_millis(40),
            max_processes: 1,
            queue_limit: 64,
            queue_timeout: "5s",
            requests: 32,
            scenario: "maxproc_1_queue",
        },
        ConcurrentScenario {
            concurrency: 8,
            delay: Duration::from_millis(40),
            max_processes: 4,
            queue_limit: 64,
            queue_timeout: "5s",
            requests: 32,
            scenario: "maxproc_N_queue",
        },
        ConcurrentScenario {
            concurrency: 8,
            delay: Duration::from_millis(40),
            max_processes: 1,
            queue_limit: 0,
            queue_timeout: "5s",
            requests: 32,
            scenario: "maxproc_1_no_queue",
        },
        ConcurrentScenario {
            concurrency: 8,
            delay: Duration::from_millis(40),
            max_processes: 4,
            queue_limit: 0,
            queue_timeout: "5s",
            requests: 32,
            scenario: "maxproc_N_no_queue",
        },
    ];

    for scenario in scenarios {
        let report = run_concurrent_scenario(scenario).await;
        if scenario.queue_limit == 0 && scenario.concurrency > scenario.max_processes {
            assert!(
                report.rejected > 0,
                "no-queue overload scenario must surface queue rejections"
            );
        }
        if scenario.queue_limit > 0 {
            assert_eq!(
                report.ok, scenario.requests,
                "queued scenario must complete all requests"
            );
        }
        assert!(
            report.total_processes <= scenario.max_processes,
            "pool must not create more processes than maxProcesses"
        );
        print_concurrent_report(scenario, report);
    }
}
