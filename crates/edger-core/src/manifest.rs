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

/// Host-held SSE policy (EDG-55): the guest authorizes and publishes in short
/// executions and the host keeps the browser connection. Validation lives in
/// `validate_worker_manifest`; every default is resolved in `SseSettings`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SseManifest {
    /// Maximum simultaneous subscribers of this app (default 256, capped by
    /// the host ceiling).
    pub max_connections: Option<u32>,
    /// Worker-relative routes that accept `publish` directives (0 to 16).
    #[serde(default)]
    pub publish: Vec<String>,
    /// Worker-relative routes that open a subscription (1 to 16).
    #[serde(default)]
    pub subscribe: Vec<String>,
    /// Subscription lifetime before the client is told to reconnect; same
    /// forms as the worker `ttl` (default `5m`, from `5s` to `1h`).
    pub ttl: Option<DurationValue>,
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

/// Explicit WASM backend of a fullstack SPA (EDG-53). Both fields are
/// required by validation (`validate_worker_manifest`); they parse as
/// `Option` so a missing field is a static validation error naming the
/// field instead of a serde message.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmBackendManifest {
    /// Relative, portable path of the `.wasm`/`.wat` module inside the version.
    pub entrypoint: Option<String>,
    /// Canonical absolute API prefix (`/api`, `/rpc/v1`); never `/`.
    pub api_prefix: Option<String>,
}

/// Pure check (no filesystem access) for a relative, portable manifest path:
/// non-empty, no leading `/`, no `//` or trailing `/`, no `.`/`..` component,
/// no Windows drive prefix, backslash, NUL or control character.
pub fn is_portable_relative_path(value: &str) -> bool {
    if value.trim().is_empty() || value.starts_with('/') {
        return false;
    }
    if value.chars().any(|c| c == '\\' || c.is_control()) {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    value
        .split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

/// Pure check for a WASM backend entrypoint: portable relative path ending
/// in `.wasm` or `.wat` (the extensions the existing WASM backend accepts).
pub fn is_valid_wasm_entrypoint(value: &str) -> bool {
    is_portable_relative_path(value) && (value.ends_with(".wasm") || value.ends_with(".wat"))
}

/// Pure check for a canonical API prefix: absolute, non-root, no trailing or
/// repeated `/`, no `.`/`..` component, no whitespace/control character and
/// none of `%`, `?`, `#`, `\`.
pub fn is_valid_api_prefix(value: &str) -> bool {
    if !value.starts_with('/') || value.len() < 2 || value.ends_with('/') {
        return false;
    }
    if value
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '%' | '?' | '#' | '\\'))
    {
        return false;
    }
    value[1..]
        .split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
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
    /// Host-held SSE policy (EDG-55); `None` leaves the worker as before.
    pub sse: Option<SseManifest>,
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
    /// Opt-in WASM backend for `kind: fullstack` + `adapter: wasm`
    /// (EDG-53): explicit nested object, never inferred from the top-level
    /// `entrypoint`/`ssrEntrypoint`.
    pub wasm_backend: Option<WasmBackendManifest>,
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

#[cfg(test)]
mod wasm_backend_tests {
    use super::*;

    #[test]
    fn portable_relative_paths_accept_plain_and_nested() {
        for ok in ["dist", "backend/index.wasm", "a/b/c", "dist.v2/x", "a.b"] {
            assert!(is_portable_relative_path(ok), "{ok}");
        }
    }

    #[test]
    fn portable_relative_paths_refuse_hostile_forms() {
        for bad in [
            "", "   ", "/abs", "a//b", "a/", ".", "..", "./a", "a/./b", "../a", "a/../b", "a/..",
            "C:/x", "c:x", "a\\b", "a\0b", "a\nb", "a\tb", "a\u{7f}b",
        ] {
            assert!(!is_portable_relative_path(bad), "{bad:?}");
        }
    }

    #[test]
    fn wasm_entrypoint_requires_wasm_or_wat_extension() {
        assert!(is_valid_wasm_entrypoint("backend/index.wasm"));
        assert!(is_valid_wasm_entrypoint("backend/index.wat"));
        assert!(!is_valid_wasm_entrypoint("backend/index.js"));
        assert!(!is_valid_wasm_entrypoint("backend/index.wasm/"));
        assert!(!is_valid_wasm_entrypoint("../index.wasm"));
        assert!(!is_valid_wasm_entrypoint("/index.wasm"));
        assert!(!is_valid_wasm_entrypoint("index"));
    }

    #[test]
    fn api_prefix_accepts_canonical_and_refuses_hostile_forms() {
        for ok in ["/api", "/rpc/v1", "/a-b_c.d"] {
            assert!(is_valid_api_prefix(ok), "{ok}");
        }
        for bad in [
            "",
            "/",
            "api",
            "/api/",
            "//api",
            "/api//v1",
            "/.",
            "/..",
            "/api/.",
            "/api/../x",
            "/a b",
            "/a\tb",
            "/a\0b",
            "/a%2fb",
            "/a?b",
            "/a#b",
            "/a\\b",
            "/a\u{a0}b",
            " /api",
        ] {
            assert!(!is_valid_api_prefix(bad), "{bad:?}");
        }
    }
}
