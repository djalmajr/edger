//! Worker manifest types (human-editable manifest.yaml form).

use serde::{Deserialize, Serialize};

/// Process lifecycle isolation policy for a worker.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerIsolation {
    /// Reuse a warm process according to ttl/maxRequests.
    #[default]
    Persistent,
    /// Recycle the process after exactly one request.
    Oneshot,
}

/// Data-plane exposure policy for a worker.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorkerVisibility {
    /// Routable through the open worker data plane.
    #[default]
    Public,
    /// Routable only through authenticated internal/control-plane dispatch.
    Internal,
}

/// Deno module-cache isolation mode for persistent JS/TS workers.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DenoCacheMode {
    /// Dedicated DENO_DIR per worker. Safer for multi-tenant data planes.
    #[default]
    #[serde(alias = "isolated", alias = "perWorker", alias = "per_worker")]
    PerWorker,
    /// Use the host/global DENO_DIR behavior. Faster warm sharing, weaker isolation.
    Shared,
}

/// Scheduled job fired by runtime cron (Buntime `cron[]`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CronJob {
    pub schedule: String,
    pub path: String,
    #[serde(default)]
    pub method: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerHealthCheckMode {
    #[default]
    Manual,
    OnDeploy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkerHealthCheck {
    pub path: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub mode: WorkerHealthCheckMode,
    #[serde(default)]
    pub timeout: Option<String>,
}

/// Opt-in process warmup (EDG-15): after a process created by the
/// `minProcesses` prewarm or the floor replenishment is spawned, the pool
/// sends ONE synthetic `GET` to it (always `GET` — there is no method
/// field) before the process goes `Idle`, so the first user request finds
/// the code already executed once.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkerWarmup {
    /// Absolute worker pathname the synthetic request targets.
    pub path: String,
    /// Duration of the warmup dispatch (same format as `healthCheck.timeout`);
    /// absent/unparseable values normalize to the 10 s default.
    #[serde(default)]
    pub timeout: Option<String>,
}

/// Opt-in egress guard policy (EDG-38): the worker reaches user-chosen
/// hosts only through the EdgeR egress guard. Present only with
/// `enabled: true` in the normalized config; with `enabled: false` (or
/// absent) every other field is ignored.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EgressManifest {
    /// Switch the guard on for this worker. `true` requires `allowNet` to be
    /// declared (it may be empty).
    pub enabled: Option<bool>,
    /// Destination ports allowed by the guard (default `[443]` — HTTPS).
    pub ports: Option<Vec<u16>>,
    /// Time to establish the outbound connection (duration string, same
    /// format as `ttl`; default `5s`).
    pub connect_timeout: Option<String>,
    /// Time until the response headers arrive (default `30s`).
    pub headers_timeout: Option<String>,
    /// Time without body bytes before the guard cuts the stream (default
    /// `30s` — the SSE case).
    pub idle_timeout: Option<String>,
    /// Total time for the whole response, head to end (default `5m`).
    pub total_timeout: Option<String>,
    /// Response body cap, in the `maxBodySize` size format (default
    /// `100mb` = 100 MiB).
    pub max_response_bytes: Option<String>,
    /// Simultaneous outbound calls per worker version (default `8`).
    pub max_concurrent: Option<u32>,
}

/// Raw value of a duration manifest field (`ttl`, `streamTimeout`,
/// `shutdownGrace`, `cooldown`, `queueTimeout`).
///
/// The manifest accepts any scalar or collection in these fields; only
/// non-negative integers (seconds) and duration strings normalize to
/// milliseconds (`parse_duration_to_ms`), and every other form is accepted at
/// parse time and yields `None` (the field's default). The variants mirror
/// the shapes the historical `serde_yaml::Value` fields carried, so the JSON
/// a manifest serializes to is unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DurationValue {
    /// Any number (integer or float); negative and non-integer values are
    /// ignored during normalization.
    Number(serde_json::Number),
    /// Any string (duration text such as `30s`, or an ignored scalar).
    String(String),
    /// A boolean; accepted, ignored during normalization.
    Bool(bool),
    /// An explicit `~`/`null`; accepted, ignored during normalization.
    Null,
    /// A sequence; accepted, ignored during normalization.
    Sequence(Vec<DurationValue>),
    /// A mapping with string keys (sorted, as the historical
    /// `serde_yaml::Value::Mapping` serialized); accepted, ignored during
    /// normalization. Mappings with non-string keys are a parse error.
    Mapping(std::collections::BTreeMap<String, DurationValue>),
}

/// Human-editable worker manifest (from manifest.yaml / package.json fallback).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkerManifest {
    #[serde(default)]
    pub name: String,
    pub version: Option<String>,
    pub enabled: Option<bool>,
    #[serde(default)]
    pub visibility: WorkerVisibility,
    pub entrypoint: Option<String>,
    /// Command run once per deployed version before the worker serves (migrations, etc.).
    pub release: Option<String>,
    pub health_check: Option<WorkerHealthCheck>,
    /// Opt-in warmup of prewarmed/replenished processes (EDG-15).
    pub warmup: Option<WorkerWarmup>,
    pub env: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub env_prefix: Vec<String>,
    #[serde(default, alias = "public_env")]
    pub public_env: Vec<String>,
    #[serde(default, alias = "allow_net")]
    pub allow_net: Option<Vec<String>>,
    /// Opt-in egress guard policy (EDG-38); `None` or `enabled: false`
    /// leaves the worker exactly as before.
    pub egress: Option<EgressManifest>,
    /// App secret names (EDG-38): the values live in the EdgeR vault, never
    /// in the manifest; the names are injected into the worker's `env` at
    /// spawn.
    pub secrets: Option<Vec<String>>,
    pub ttl: Option<DurationValue>,
    pub stream_timeout: Option<DurationValue>,
    pub timeout: Option<String>,
    pub idle_timeout: Option<String>,
    #[serde(default, alias = "shutdown_grace")]
    pub shutdown_grace: Option<DurationValue>,
    pub max_requests: Option<u32>,
    pub concurrency: Option<usize>,
    pub min_processes: Option<usize>,
    pub max_processes: Option<usize>,
    #[serde(default, alias = "circuit_breaker_failures")]
    pub circuit_breaker_failures: Option<u32>,
    pub cooldown: Option<DurationValue>,
    pub isolation: Option<WorkerIsolation>,
    pub queue_limit: Option<usize>,
    pub queue_timeout: Option<DurationValue>,
    pub max_body_size: Option<String>,
    pub low_memory: Option<bool>,
    #[serde(default, alias = "memory_mb")]
    pub memory_mb: Option<u32>,
    #[serde(default, alias = "rss_soft_mb")]
    pub rss_soft_mb: Option<u32>,
    #[serde(default, alias = "cpu_soft_ms")]
    pub cpu_soft_ms: Option<u64>,
    #[serde(default, alias = "cpu_hard_ms")]
    pub cpu_hard_ms: Option<u64>,
    #[serde(default, alias = "rate_limit_rps")]
    pub rate_limit_rps: Option<u32>,
    pub auto_install: Option<bool>,
    #[serde(default, alias = "deno_cache_mode")]
    pub deno_cache_mode: Option<DenoCacheMode>,
    #[serde(default, alias = "node_http_proxy")]
    pub node_http_proxy: bool,
    pub inject_base: Option<bool>,
    pub adapter: Option<String>,
    pub ssr_entrypoint: Option<String>,
    pub client_dir: Option<String>,
    #[serde(default)]
    pub asset_prefixes: Vec<String>,
    pub base_path: Option<String>,
    pub cron: Option<Vec<CronJob>>,
    pub kind: Option<String>,
    pub base: Option<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Opt-in to keep the name route (`/<name>/…`) open for a worker that
    /// declares `hosts` (EDG-27). Default `false`: a version that declares
    /// `hosts` is served only on its own hosts, and the name route answers
    /// `404 NOT_FOUND` in the same shape as an unknown worker. Has no effect
    /// when `hosts` is empty.
    #[serde(default)]
    pub name_route: bool,
    pub dependencies: Option<Vec<String>>,
}
