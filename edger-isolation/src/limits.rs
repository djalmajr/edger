//! Resource limits: wall-clock timeout + per-process CPU-time and RSS caps.
//!
//! CPU/RSS enforcement samples the child process periodically. On Linux it
//! reads `/proc`; on other platforms the sampler is a no-op (limits are a
//! Linux-runtime concern — the dev host just compiles and unit-tests the
//! decision logic with a mock sampler). Hard breaches kill the process (the
//! pool then respawns it); soft breaches are surfaced for preventive recycle.

use std::time::Duration;

use edger_core::{ExecutionKind, Isolate, SerializedRequest, SerializedResponse, WorkerConfig};

use crate::kinds::dispatch_execution;
use crate::wire::validate_request;

/// Default hard RSS cap when a worker declares `lowMemory` / normal mode.
const DEFAULT_LOW_MEMORY_MB: u32 = 128;
const DEFAULT_MEMORY_MB: u32 = 512;

/// Configurable resource limits applied to a worker process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Hard RSS cap (MB). Process is killed above this.
    pub memory_mb: Option<u32>,
    /// Soft RSS threshold (MB) for preventive recycle. Below `memory_mb`.
    pub rss_soft_mb: Option<u32>,
    /// Soft CPU-time budget (ms). Marks the worker for preventive recycle.
    pub cpu_soft_ms: Option<u64>,
    /// Hard CPU-time budget (ms). Process is killed above this.
    pub cpu_hard_ms: Option<u64>,
    /// Legacy field kept for wire compatibility (defaults to wall timeout).
    pub cpu_time_ms: Option<u64>,
    pub wall_timeout_ms: u64,
    pub low_memory: bool,
}

impl ResourceLimits {
    pub fn from_config(config: &WorkerConfig) -> Self {
        let memory_mb = config.memory_mb.or(Some(if config.low_memory {
            DEFAULT_LOW_MEMORY_MB
        } else {
            DEFAULT_MEMORY_MB
        }));
        Self {
            memory_mb,
            rss_soft_mb: config.rss_soft_mb,
            cpu_soft_ms: config.cpu_soft_ms,
            cpu_hard_ms: config.cpu_hard_ms,
            cpu_time_ms: Some(config.timeout_ms),
            wall_timeout_ms: config.timeout_ms,
            low_memory: config.low_memory,
        }
    }

    /// Whether any CPU/RSS enforcement is configured (skip the monitor if not).
    pub fn has_process_caps(&self) -> bool {
        self.cpu_soft_ms.is_some()
            || self.cpu_hard_ms.is_some()
            || self.rss_soft_mb.is_some()
            || self.memory_mb.is_some()
    }
}

/// A single point-in-time sample of a process's resource usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessUsage {
    /// Total CPU time (user + system) in milliseconds.
    pub cpu_ms: u64,
    /// Resident set size in bytes.
    pub rss_bytes: u64,
}

/// Abstraction over "read a process's CPU/RSS", so the monitor is testable
/// without a real OS process.
pub trait ProcessSampler: Send + Sync {
    fn sample(&self, pid: u32) -> Option<ProcessUsage>;
}

/// Reads `/proc/<pid>/stat` + `/proc/<pid>/statm` on Linux; `None` elsewhere.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcFsSampler;

impl ProcessSampler for ProcFsSampler {
    fn sample(&self, pid: u32) -> Option<ProcessUsage> {
        #[cfg(target_os = "linux")]
        {
            proc_sample_linux(pid)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn proc_sample_linux(pid: u32) -> Option<ProcessUsage> {
    // Standard Linux constants; avoids a libc call for a coarse cap.
    const CLK_TCK: u64 = 100;
    const PAGE_SIZE: u64 = 4096;

    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field (2nd) can contain spaces/parens; split after the last ')'.
    let after = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    // After ')' index 0 = state (field 3). utime = field 14 -> index 11,
    // stime = field 15 -> index 12.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let cpu_ms = (utime + stime).saturating_mul(1000) / CLK_TCK.max(1);

    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    // Field 2 = resident pages.
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let rss_bytes = resident_pages.saturating_mul(PAGE_SIZE);

    Some(ProcessUsage { cpu_ms, rss_bytes })
}

/// Outcome of evaluating a sample against the configured limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitBreach {
    None,
    SoftCpu,
    HardCpu,
    SoftRss,
    HardRss,
}

impl LimitBreach {
    pub fn is_hard(self) -> bool {
        matches!(self, LimitBreach::HardCpu | LimitBreach::HardRss)
    }
    pub fn is_soft(self) -> bool {
        matches!(self, LimitBreach::SoftCpu | LimitBreach::SoftRss)
    }
}

/// Pure decision: given a usage sample and limits, classify the breach.
/// Hard limits take precedence over soft; CPU is checked before RSS.
pub fn evaluate_limits(usage: ProcessUsage, limits: &ResourceLimits) -> LimitBreach {
    if let Some(hard) = limits.cpu_hard_ms {
        if usage.cpu_ms >= hard {
            return LimitBreach::HardCpu;
        }
    }
    if let Some(mb) = limits.memory_mb {
        if usage.rss_bytes >= (mb as u64) * 1024 * 1024 {
            return LimitBreach::HardRss;
        }
    }
    if let Some(soft) = limits.cpu_soft_ms {
        if usage.cpu_ms >= soft {
            return LimitBreach::SoftCpu;
        }
    }
    if let Some(mb) = limits.rss_soft_mb {
        if usage.rss_bytes >= (mb as u64) * 1024 * 1024 {
            return LimitBreach::SoftRss;
        }
    }
    LimitBreach::None
}

/// Periodically sample `pid` and invoke `on_hard` once a hard limit is crossed
/// (then stop). Soft breaches are reported via `on_soft`. Generic over the
/// sampler so tests inject deterministic usage without a real process.
pub async fn monitor_process<S, H, T>(
    pid: u32,
    limits: ResourceLimits,
    sampler: S,
    interval: Duration,
    mut on_soft: T,
    mut on_hard: H,
) where
    S: ProcessSampler,
    H: FnMut(LimitBreach),
    T: FnMut(LimitBreach),
{
    let mut soft_reported = false;
    loop {
        tokio::time::sleep(interval).await;
        let Some(usage) = sampler.sample(pid) else {
            // No sampler support (non-Linux) — nothing to enforce.
            return;
        };
        match evaluate_limits(usage, &limits) {
            LimitBreach::None => {}
            breach if breach.is_hard() => {
                on_hard(breach);
                return;
            }
            soft => {
                if !soft_reported {
                    soft_reported = true;
                    on_soft(soft);
                }
            }
        }
    }
}

/// RAII guard placeholder for pre-dispatch accounting (validation lives here).
pub struct LimitGuard {
    #[allow(dead_code)]
    limits: ResourceLimits,
}

impl LimitGuard {
    pub fn new(limits: ResourceLimits) -> Self {
        Self { limits }
    }

    pub fn check_memory(&self) -> Result<(), edger_core::IsolationError> {
        Ok(())
    }
}

/// `CpuTimer` retained as a lightweight marker for callers/tests.
#[derive(Default)]
pub struct CpuTimer;

impl CpuTimer {
    pub fn new() -> Self {
        Self
    }
}

/// Execute dispatch with validation + wall-clock timeout.
pub async fn execute_with_limits<I: Isolate + ?Sized>(
    isolate: &mut I,
    kind: ExecutionKind,
    req: SerializedRequest,
    config: &WorkerConfig,
    limits: &ResourceLimits,
) -> Result<SerializedResponse, edger_core::IsolationError> {
    validate_request(&req, config)?;
    let _guard = LimitGuard::new(limits.clone());
    _guard.check_memory()?;

    let timeout = std::time::Duration::from_millis(limits.wall_timeout_ms);
    match tokio::time::timeout(timeout, dispatch_execution(isolate, kind, req, config)).await {
        Ok(inner) => inner,
        Err(_) => Err(edger_core::IsolationError::new(
            "TIMEOUT",
            format!("exceeded {}ms", limits.wall_timeout_ms),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(
        cpu_soft: Option<u64>,
        cpu_hard: Option<u64>,
        rss_soft: Option<u32>,
        mem: Option<u32>,
    ) -> ResourceLimits {
        ResourceLimits {
            memory_mb: mem,
            rss_soft_mb: rss_soft,
            cpu_soft_ms: cpu_soft,
            cpu_hard_ms: cpu_hard,
            cpu_time_ms: None,
            wall_timeout_ms: 30_000,
            low_memory: false,
        }
    }

    #[test]
    fn evaluate_none_below_all_thresholds() {
        let l = limits(Some(1000), Some(2000), Some(64), Some(128));
        let u = ProcessUsage {
            cpu_ms: 10,
            rss_bytes: 1024,
        };
        assert_eq!(evaluate_limits(u, &l), LimitBreach::None);
    }

    #[test]
    fn evaluate_hard_cpu_takes_precedence() {
        let l = limits(Some(1000), Some(2000), Some(64), Some(128));
        let u = ProcessUsage {
            cpu_ms: 2500,
            rss_bytes: 200 * 1024 * 1024,
        };
        assert_eq!(evaluate_limits(u, &l), LimitBreach::HardCpu);
    }

    #[test]
    fn evaluate_hard_rss() {
        let l = limits(None, None, None, Some(128));
        let u = ProcessUsage {
            cpu_ms: 0,
            rss_bytes: 129 * 1024 * 1024,
        };
        assert_eq!(evaluate_limits(u, &l), LimitBreach::HardRss);
    }

    #[test]
    fn evaluate_soft_cpu_when_below_hard() {
        let l = limits(Some(1000), Some(2000), None, None);
        let u = ProcessUsage {
            cpu_ms: 1200,
            rss_bytes: 0,
        };
        assert_eq!(evaluate_limits(u, &l), LimitBreach::SoftCpu);
    }

    #[test]
    fn evaluate_soft_rss_when_below_hard() {
        let l = limits(None, None, Some(64), Some(128));
        let u = ProcessUsage {
            cpu_ms: 0,
            rss_bytes: 70 * 1024 * 1024,
        };
        assert_eq!(evaluate_limits(u, &l), LimitBreach::SoftRss);
    }

    struct MockSampler {
        usages: std::sync::Mutex<std::collections::VecDeque<ProcessUsage>>,
    }
    impl ProcessSampler for MockSampler {
        fn sample(&self, _pid: u32) -> Option<ProcessUsage> {
            self.usages.lock().unwrap().pop_front()
        }
    }

    #[tokio::test]
    async fn monitor_fires_hard_and_stops() {
        let usages = vec![
            ProcessUsage {
                cpu_ms: 100,
                rss_bytes: 0,
            }, // none
            ProcessUsage {
                cpu_ms: 1200,
                rss_bytes: 0,
            }, // soft
            ProcessUsage {
                cpu_ms: 2500,
                rss_bytes: 0,
            }, // hard
        ];
        let sampler = MockSampler {
            usages: std::sync::Mutex::new(usages.into_iter().collect()),
        };
        let l = limits(Some(1000), Some(2000), None, None);
        let soft = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hard = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s2, h2) = (soft.clone(), hard.clone());
        monitor_process(
            42,
            l,
            sampler,
            Duration::from_millis(1),
            move |_| {
                s2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            move |b| {
                assert!(b.is_hard());
                h2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        )
        .await;
        assert_eq!(soft.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(hard.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn procfs_sampler_is_noop_off_linux() {
        // On the macOS dev host this must return None (no enforcement, compiles).
        #[cfg(not(target_os = "linux"))]
        assert!(ProcFsSampler.sample(std::process::id()).is_none());
        // On Linux it should read the current process without panicking.
        #[cfg(target_os = "linux")]
        {
            let _ = ProcFsSampler.sample(std::process::id());
        }
    }
}
