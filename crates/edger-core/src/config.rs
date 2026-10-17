//! Normalized worker configuration and parsers.

use crate::execution::{normalize_fullstack_adapter, ExecutionKind};
use crate::manifest::{
    DenoCacheMode, DurationValue, WorkerHealthCheckMode, WorkerIsolation, WorkerManifest,
    WorkerVisibility,
};

/// Runtime-normalized worker configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    pub enabled: bool,
    pub visibility: WorkerVisibility,
    /// Host aliases this version declares for vhost routing (EDG-27),
    /// filtered to the entries the vhost normalization would keep. A worker
    /// whose list holds no routable alias behaves like one without `hosts`.
    pub hosts: Vec<String>,
    /// Keep the name route (`/<name>/…`) open even when this version declares
    /// `hosts` (manifest `nameRoute`, default `false`; EDG-27). Has no effect
    /// when `hosts` is empty.
    pub name_route: bool,
    /// Worker directory on disk (set by pool at fetch time; pure path metadata).
    pub worker_dir: Option<std::path::PathBuf>,
    pub entrypoint: Option<String>,
    /// Release command (migrations etc.) run once per version before serving.
    pub release_command: Option<String>,
    pub health_check: Option<WorkerHealthCheckConfig>,
    /// Opt-in warmup of prewarmed/replenished processes (EDG-15): one
    /// synthetic `GET` right after the spawn, before the process goes Idle.
    pub warmup: Option<WorkerWarmupConfig>,
    pub env: std::collections::HashMap<String, String>,
    pub env_prefix: Vec<String>,
    pub public_env: Vec<String>,
    pub allow_net: Option<Vec<String>>,
    pub ttl_ms: u64,
    /// Optional maximum response-stream body duration; `Some(0)` disables it.
    pub stream_max_duration_ms: Option<u64>,
    pub timeout_ms: u64,
    pub idle_timeout_ms: u64,
    /// Grace budget (ms) for the beforeunload/waitUntil drain on graceful shutdown.
    pub shutdown_grace_ms: u64,
    pub max_requests: u32,
    pub concurrency: usize,
    pub min_processes: usize,
    pub max_processes: usize,
    /// Consecutive spawn/ready failures before opening the worker circuit.
    pub circuit_breaker_failures: u32,
    /// Circuit-breaker failure window and cooldown duration.
    pub cooldown_ms: u64,
    pub isolation: WorkerIsolation,
    /// Maximum persistent-worker requests waiting after all processes are busy.
    pub queue_limit: usize,
    /// Maximum time a persistent-worker request can wait for a process slot.
    pub queue_timeout_ms: u64,
    pub max_body_size_bytes: Option<u64>,
    pub low_memory: bool,
    /// Hard RSS cap (MB); process killed above it. Falls back to a low/normal default.
    pub memory_mb: Option<u32>,
    /// Soft RSS threshold (MB) for preventive recycle. Below `memory_mb`.
    pub rss_soft_mb: Option<u32>,
    /// Soft CPU-time budget (ms) for preventive recycle.
    pub cpu_soft_ms: Option<u64>,
    /// Hard CPU-time budget (ms); process killed above it.
    pub cpu_hard_ms: Option<u64>,
    /// Per-worker admission ceiling in requests/second (0/None = unlimited).
    pub rate_limit_rps: Option<u32>,
    pub auto_install: bool,
    pub deno_cache_mode: DenoCacheMode,
    /// Bind a genuine node:http server to a private Unix socket for frameworks
    /// that require complete IncomingMessage/ServerResponse semantics.
    pub node_http_proxy: bool,
    pub inject_base: bool,
    pub cron: Vec<crate::manifest::CronJob>,
    pub kind: Option<ExecutionKind>,
    pub fullstack: Option<FullstackConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerHealthCheckConfig {
    pub path: String,
    pub method: String,
    pub mode: WorkerHealthCheckMode,
    pub timeout_ms: u64,
}

/// Normalized opt-in process warmup (EDG-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerWarmupConfig {
    pub path: String,
    /// Wall-clock budget of the warmup dispatch; applied by the isolate.
    /// Absent or unparseable `timeout` normalizes to the 10 s default.
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullstackConfig {
    pub adapter: String,
    pub ssr_entrypoint: Option<String>,
    pub client_dir: Option<String>,
    pub asset_prefixes: Vec<String>,
    pub base_path: FullstackBasePath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullstackBasePath {
    Auto,
    Fixed(String),
}

/// Parse a duration manifest field to milliseconds (Buntime `parseDurationToMs`):
/// a non-negative integer is seconds, a string goes through
/// `parse_duration_string_to_ms`. Floats, booleans, nulls, negative numbers,
/// sequences and mappings are accepted by the manifest and yield `None`
/// (the field's default applies).
pub fn parse_duration_to_ms(value: &DurationValue) -> Option<u64> {
    match value {
        // (EDG-36) checked: a value whose millisecond form does not fit
        // `u64` is invalid (`None`), not a wrap (release) or a panic (debug).
        DurationValue::Number(n) => n.as_u64().and_then(|secs| secs.checked_mul(1000)),
        DurationValue::String(s) => parse_duration_string_to_ms(s),
        _ => None,
    }
}

/// Parse duration text like `30s`, `5m`, `1h` to milliseconds. A value whose
/// millisecond form does not fit `u64` yields `None` (invalid), not a
/// wrapped value or a panic (EDG-36).
pub fn parse_duration_string_to_ms(input: &str) -> Option<u64> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Ok(secs) = input.parse::<u64>() {
        return secs.checked_mul(1000);
    }
    if let Some(stripped) = input.strip_suffix("ms") {
        return stripped.parse().ok();
    }
    let (num_part, unit) = input.split_at(input.len().saturating_sub(1));
    let num: u64 = num_part.parse().ok()?;
    // (EDG-36) every multiplication is checked: an overflow yields `None`
    // (the field's default applies), never a wrap or a panic.
    match unit {
        "s" => num.checked_mul(1000),
        "m" => num
            .checked_mul(60)
            .and_then(|minutes| minutes.checked_mul(1000)),
        "h" => num
            .checked_mul(60)
            .and_then(|hours| hours.checked_mul(60))
            .and_then(|minutes| minutes.checked_mul(1000)),
        _ => None,
    }
}

/// Parse size string like `10mb` to bytes (Buntime `parseSizeToBytes`).
pub fn parse_size_to_bytes(input: &str) -> Option<u64> {
    let input = input.trim().to_ascii_lowercase();
    if input.is_empty() {
        return None;
    }
    if let Ok(n) = input.parse::<u64>() {
        return Some(n);
    }
    const UNITS: [(&str, u64); 4] = [
        ("kb", 1024),
        ("mb", 1024 * 1024),
        ("gb", 1024 * 1024 * 1024),
        ("b", 1),
    ];
    for (suffix, mult) in UNITS {
        if let Some(num) = input.strip_suffix(suffix) {
            let n: u64 = num.trim().parse().ok()?;
            return Some(n * mult);
        }
    }
    None
}

/// Infer execution kind from manifest fields (Buntime getEntrypoint + wrapper rules).
pub fn infer_execution_kind(manifest: &WorkerManifest) -> ExecutionKind {
    if let Some(ref kind) = manifest.kind {
        if let Some(parsed) =
            ExecutionKind::from_manifest_kind_with_adapter(kind, manifest.adapter.as_deref())
        {
            return match parsed {
                ExecutionKind::WasmModule { entry: None } => ExecutionKind::WasmModule {
                    entry: manifest.entrypoint.clone(),
                },
                ExecutionKind::StaticSpa { .. } => ExecutionKind::StaticSpa {
                    inject_base: manifest.inject_base.unwrap_or(true),
                },
                other => other,
            };
        }
    }
    if let Some(ref entry) = manifest.entrypoint {
        if entry.ends_with(".html") {
            return ExecutionKind::StaticSpa {
                inject_base: manifest.inject_base.unwrap_or(true),
            };
        }
        if entry.ends_with(".wasm") || entry.ends_with(".wat") {
            return ExecutionKind::WasmModule {
                entry: Some(entry.clone()),
            };
        }
    }
    ExecutionKind::FetchHandler
}

pub fn fullstack_config_from_manifest(manifest: &WorkerManifest) -> Option<FullstackConfig> {
    let kind = manifest.kind.as_deref()?;
    let is_fullstack = matches!(
        kind.trim().to_ascii_lowercase().as_str(),
        "ssr" | "fullstack"
    );
    if !is_fullstack {
        return None;
    }

    let adapter = manifest
        .adapter
        .as_deref()
        .and_then(normalize_fullstack_adapter)
        .unwrap_or("")
        .to_string();
    let asset_prefixes = if manifest.asset_prefixes.is_empty() {
        default_fullstack_asset_prefixes(&adapter, manifest.client_dir.is_some())
    } else {
        manifest
            .asset_prefixes
            .iter()
            .filter_map(|prefix| normalize_asset_prefix(prefix))
            .collect()
    };

    Some(FullstackConfig {
        adapter,
        ssr_entrypoint: manifest
            .ssr_entrypoint
            .clone()
            .or_else(|| manifest.entrypoint.clone()),
        client_dir: manifest.client_dir.clone(),
        asset_prefixes,
        base_path: normalize_fullstack_base_path(manifest.base_path.as_deref()),
    })
}

fn default_fullstack_asset_prefixes(adapter: &str, has_client_dir: bool) -> Vec<String> {
    match adapter {
        "astro" if has_client_dir => ["/_astro/", "/favicon.ico", "/robots.txt"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "lume" if has_client_dir => vec!["/".to_string()],
        "nuxt" if has_client_dir => ["/_nuxt/", "/favicon.ico", "/robots.txt"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "solidstart" if has_client_dir => ["/_build/", "/favicon.ico", "/robots.txt"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "remix" if has_client_dir => ["/assets/", "/favicon.ico", "/robots.txt"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "tanstack" => [
            "/assets/",
            "/favicon.ico",
            "/logo192.png",
            "/logo512.png",
            "/manifest.json",
            "/robots.txt",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        "hono" if has_client_dir => ["/assets/", "/favicon.ico", "/manifest.json", "/robots.txt"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn normalize_asset_prefix(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        return None;
    }
    let prefixed = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    if prefixed == "/" {
        Some(prefixed)
    } else {
        Some(prefixed.trim_end_matches('/').to_string())
    }
}

fn normalize_fullstack_base_path(value: Option<&str>) -> FullstackBasePath {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return FullstackBasePath::Auto;
    };
    if value.eq_ignore_ascii_case("auto") {
        return FullstackBasePath::Auto;
    }
    let normalized = if value == "/" {
        "/".to_string()
    } else {
        format!("/{}", value.trim_matches('/'))
    };
    FullstackBasePath::Fixed(normalized)
}

/// Explicit JS/TS and SPA workers default to a sliding TTL (persistent) instead
/// of ephemeral. A StaticSpa is a pure file server; a FetchHandler/RoutesTable
/// is backed by a persistent Deno process (Epic 15) whose whole value is being
/// reused across requests. Bare legacy manifests without `ttl`, `kind`, or an
/// `entrypoint` stay ephemeral because the parser cannot prove they should use
/// the warm process path.
const WARM_WORKER_DEFAULT_TTL_MS: u64 = 300_000;
/// Default warmup (EDG-15) dispatch budget when the manifest `warmup`
/// timeout is absent or does not parse.
const DEFAULT_WARMUP_TIMEOUT_MS: u64 = 10_000;
/// Small non-zero queue depth preserves 18.A's default "wait briefly" behavior
/// while bounding memory and surfacing overload under sustained saturation.
const DEFAULT_WORKER_QUEUE_LIMIT: usize = 8;
/// Short persistent-worker queue wait before reporting capacity timeout.
const DEFAULT_WORKER_QUEUE_TIMEOUT_MS: u64 = 1_000;
const DEFAULT_CIRCUIT_BREAKER_FAILURES: u32 = 3;
const DEFAULT_CIRCUIT_BREAKER_COOLDOWN_MS: u64 = 30_000;
/// Default request body cap when a worker manifest does not override it.
pub const DEFAULT_MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;

pub fn effective_max_body_size_bytes(config: &WorkerConfig) -> u64 {
    config.max_body_size_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES)
}

pub fn effective_max_body_size_bytes_usize(config: &WorkerConfig) -> usize {
    usize::try_from(effective_max_body_size_bytes(config)).unwrap_or(usize::MAX)
}

fn normalize_allow_net(hosts: &[String]) -> Vec<String> {
    hosts
        .iter()
        .flat_map(|host| host.split(','))
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_string)
        .collect()
}

/// Host aliases a manifest declares for vhost routing, filtered to the
/// entries the index's vhost normalization would keep (EDG-27): an entry
/// that normalizes to empty (blank, or only a port / trailing DNS dot) owns
/// no host and is dropped, so `hosts: [""]` behaves like no `hosts` at all.
fn declared_host_aliases(manifest: &WorkerManifest) -> Vec<String> {
    manifest
        .hosts
        .iter()
        .filter(|host| !normalized_host_is_empty(host.as_str()))
        .cloned()
        .collect()
}

/// Emptiness mirror of the vhost alias normalization (the port is stripped
/// before the trailing DNS dot): `""`, `"."` and `".:443"` own no host.
fn normalized_host_is_empty(host: &str) -> bool {
    let trimmed = host.trim();
    let without_port = match trimmed.rsplit_once(':') {
        Some((name, port)) if !name.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()) => {
            name
        }
        _ => trimmed,
    };
    without_port.trim_end_matches('.').is_empty()
}

/// Normalize manifest into runtime `WorkerConfig`.
pub fn parse_worker_config(manifest: &WorkerManifest) -> WorkerConfig {
    let kind = infer_execution_kind(manifest);
    let has_explicit_runtime = manifest.kind.is_some() || manifest.entrypoint.is_some();
    let default_ttl_ms = if has_explicit_runtime
        && matches!(
            kind,
            ExecutionKind::StaticSpa { .. }
                | ExecutionKind::FetchHandler
                | ExecutionKind::RoutesTable
                | ExecutionKind::Fullstack { .. }
        ) {
        WARM_WORKER_DEFAULT_TTL_MS
    } else {
        0
    };
    let ttl_ms = manifest
        .ttl
        .as_ref()
        .and_then(parse_duration_to_ms)
        .unwrap_or(default_ttl_ms);
    let stream_max_duration_ms = manifest
        .stream_timeout
        .as_ref()
        .and_then(parse_duration_to_ms);

    let timeout_ms = manifest
        .timeout
        .as_ref()
        .and_then(|s| parse_duration_string_to_ms(s))
        .unwrap_or(30_000);

    let idle_timeout_ms = manifest
        .idle_timeout
        .as_ref()
        .and_then(|s| parse_duration_string_to_ms(s))
        .unwrap_or(60_000);

    // Grace budget for the graceful shutdown drain (beforeunload + waitUntil).
    // Default 0 = fire beforeunload but do not wait for async waitUntil work
    // (opt-in via manifest `shutdownGrace`), keeping recycle latency unchanged.
    let shutdown_grace_ms = manifest
        .shutdown_grace
        .as_ref()
        .and_then(parse_duration_to_ms)
        .unwrap_or(0);

    let max_body_size_bytes = manifest
        .max_body_size
        .as_ref()
        .and_then(|s| parse_size_to_bytes(s));

    let max_processes = manifest
        .max_processes
        .or(manifest.concurrency)
        .unwrap_or(1)
        .max(1);
    let concurrency = manifest
        .concurrency
        .unwrap_or(max_processes)
        .clamp(1, max_processes);
    let min_processes = manifest.min_processes.unwrap_or(0).min(max_processes);
    let queue_timeout_ms = manifest
        .queue_timeout
        .as_ref()
        .and_then(parse_duration_to_ms)
        .unwrap_or(DEFAULT_WORKER_QUEUE_TIMEOUT_MS);
    let cooldown_ms = manifest
        .cooldown
        .as_ref()
        .and_then(parse_duration_to_ms)
        .unwrap_or(DEFAULT_CIRCUIT_BREAKER_COOLDOWN_MS);
    let isolation = manifest.isolation.unwrap_or_default();
    let max_requests = if isolation == WorkerIsolation::Oneshot {
        1
    } else {
        manifest.max_requests.unwrap_or(0)
    };

    let fullstack = fullstack_config_from_manifest(manifest);
    // Fullstack workers declare their SSR module via `ssrEntrypoint`, not the
    // top-level `entrypoint`. The process backend spawns eagerly (Supervisor::spawn
    // → Isolate::prepare) using this raw config BEFORE the per-request fullstack
    // transform (`prepare_fullstack_request`) wires the entrypoint, so mirror the
    // SSR entrypoint here — otherwise the Deno process spawns with no entrypoint and
    // fails resolution with UDS_ENTRYPOINT_MISSING before ever serving a request.
    let entrypoint = manifest
        .entrypoint
        .clone()
        .or_else(|| fullstack.as_ref().and_then(|f| f.ssr_entrypoint.clone()));

    WorkerConfig {
        enabled: manifest.enabled.unwrap_or(true),
        visibility: manifest.visibility,
        hosts: declared_host_aliases(manifest),
        name_route: manifest.name_route,
        worker_dir: None,
        entrypoint,
        release_command: manifest.release.clone(),
        health_check: manifest
            .health_check
            .as_ref()
            .map(|check| WorkerHealthCheckConfig {
                path: check.path.clone(),
                method: check
                    .method
                    .clone()
                    .unwrap_or_else(|| "GET".into())
                    .to_ascii_uppercase(),
                mode: check.mode,
                timeout_ms: check
                    .timeout
                    .as_deref()
                    .and_then(parse_duration_string_to_ms)
                    .unwrap_or(2_000),
            }),
        warmup: manifest.warmup.as_ref().map(|warmup| WorkerWarmupConfig {
            path: warmup.path.clone(),
            timeout_ms: warmup
                .timeout
                .as_deref()
                .and_then(parse_duration_string_to_ms)
                .unwrap_or(DEFAULT_WARMUP_TIMEOUT_MS),
        }),
        env: manifest.env.clone().unwrap_or_default(),
        env_prefix: manifest.env_prefix.clone(),
        public_env: manifest.public_env.clone(),
        allow_net: manifest
            .allow_net
            .as_deref()
            .map(normalize_allow_net)
            .or_else(|| manifest.allow_net.as_ref().map(|_| Vec::new())),
        ttl_ms,
        stream_max_duration_ms,
        timeout_ms,
        idle_timeout_ms,
        shutdown_grace_ms,
        max_requests,
        concurrency,
        min_processes,
        max_processes,
        circuit_breaker_failures: manifest
            .circuit_breaker_failures
            .unwrap_or(DEFAULT_CIRCUIT_BREAKER_FAILURES),
        cooldown_ms,
        isolation,
        queue_limit: manifest.queue_limit.unwrap_or(DEFAULT_WORKER_QUEUE_LIMIT),
        queue_timeout_ms,
        max_body_size_bytes,
        low_memory: manifest.low_memory.unwrap_or(false),
        memory_mb: manifest.memory_mb,
        rss_soft_mb: manifest.rss_soft_mb,
        cpu_soft_ms: manifest.cpu_soft_ms,
        cpu_hard_ms: manifest.cpu_hard_ms,
        rate_limit_rps: manifest.rate_limit_rps,
        auto_install: manifest.auto_install.unwrap_or(false),
        deno_cache_mode: manifest.deno_cache_mode.unwrap_or_default(),
        node_http_proxy: manifest.node_http_proxy,
        inject_base: manifest.inject_base.unwrap_or(true),
        cron: manifest.cron.clone().unwrap_or_default(),
        kind: Some(kind),
        fullstack,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (EDG-36) a duration string whose millisecond value overflows `u64`
    /// is invalid (`None`) — not a panic (debug) nor a wrapped value
    /// (release). Each input overflows on its own unit path.
    #[test]
    fn duration_strings_beyond_u64_millis_are_invalid() {
        // 18446744073709552 s (and bare seconds) = u64::MAX / 1000 + 1 s:
        // `secs * 1000` overflows.
        assert_eq!(parse_duration_string_to_ms("18446744073709552s"), None);
        assert_eq!(parse_duration_string_to_ms("18446744073709552"), None);
        // 307445734561826 * 60 already overflows `u64` (the `m` path).
        assert_eq!(parse_duration_string_to_ms("307445734561826m"), None);
        // 5124095576030432 * 60 overflows on the first `h` step.
        assert_eq!(parse_duration_string_to_ms("5124095576030432h"), None);
    }

    /// (EDG-36) the boundary values that still fit keep parsing to the
    /// exact millisecond value (no off-by-one at the `u64` edge).
    #[test]
    fn duration_strings_at_the_u64_boundary_still_parse() {
        let secs = u64::MAX / 1000;
        let expected = secs * 1000;
        assert_eq!(
            parse_duration_string_to_ms(&format!("{secs}s")),
            Some(expected)
        );
        assert_eq!(
            parse_duration_string_to_ms(&format!("{secs}")),
            Some(expected)
        );
        let minutes = u64::MAX / 60 / 1000;
        assert_eq!(
            parse_duration_string_to_ms(&format!("{minutes}m")),
            Some(minutes * 60 * 1000)
        );
        let hours = u64::MAX / 60 / 60 / 1000;
        assert_eq!(
            parse_duration_string_to_ms(&format!("{hours}h")),
            Some(hours * 60 * 60 * 1000)
        );
    }

    /// (EDG-36) the `DurationValue::Number` seconds form is checked too:
    /// `u64::MAX / 1000 + 1` seconds is invalid; the boundary fits.
    #[test]
    fn duration_numbers_beyond_u64_millis_are_invalid() {
        let overflow = DurationValue::Number(serde_json::Number::from(u64::MAX / 1000 + 1));
        assert_eq!(parse_duration_to_ms(&overflow), None);
        let edge = u64::MAX / 1000;
        let boundary = DurationValue::Number(serde_json::Number::from(edge));
        assert_eq!(parse_duration_to_ms(&boundary), Some(edge * 1000));
    }
}
