//! Normalized worker configuration and parsers.

use crate::execution::ExecutionKind;
use crate::manifest::WorkerManifest;

/// Runtime-normalized worker configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerConfig {
    pub enabled: bool,
    pub entrypoint: Option<String>,
    pub env: std::collections::HashMap<String, String>,
    pub env_prefix: Vec<String>,
    pub ttl_ms: u64,
    pub timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub max_requests: u32,
    pub max_body_size_bytes: Option<u64>,
    pub low_memory: bool,
    pub auto_install: bool,
    pub inject_base: bool,
    pub visibility: String,
    pub public_routes: Option<crate::manifest::PublicRoutesConfig>,
    pub cron: Vec<crate::manifest::CronJob>,
    pub kind: Option<ExecutionKind>,
}

/// Parse duration string or numeric seconds to milliseconds (Buntime `parseDurationToMs`).
pub fn parse_duration_to_ms(value: &serde_yaml::Value) -> Option<u64> {
    match value {
        serde_yaml::Value::Number(n) => n.as_u64().map(|s| s * 1000),
        serde_yaml::Value::String(s) => parse_duration_string_to_ms(s),
        _ => None,
    }
}

/// Parse duration text like `30s`, `5m`, `1h` to milliseconds.
pub fn parse_duration_string_to_ms(input: &str) -> Option<u64> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Ok(secs) = input.parse::<u64>() {
        return Some(secs * 1000);
    }
    let (num_part, unit) = input.split_at(input.len().saturating_sub(1));
    let num: u64 = num_part.parse().ok()?;
    match unit {
        "s" => Some(num * 1000),
        "m" => Some(num * 60 * 1000),
        "h" => Some(num * 60 * 60 * 1000),
        _ => {
            if let Some(stripped) = input.strip_suffix("ms") {
                stripped.parse().ok()
            } else {
                None
            }
        }
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
        if let Some(parsed) = ExecutionKind::from_manifest_kind(kind) {
            return parsed;
        }
    }
    if let Some(ref entry) = manifest.entrypoint {
        if entry.ends_with(".html") {
            return ExecutionKind::StaticSpa {
                inject_base: manifest.inject_base.unwrap_or(true),
            };
        }
        if entry.ends_with(".wasm") {
            return ExecutionKind::WasmModule {
                entry: Some(entry.clone()),
            };
        }
    }
    ExecutionKind::FetchHandler
}

/// Normalize manifest into runtime `WorkerConfig`.
pub fn parse_worker_config(manifest: &WorkerManifest) -> WorkerConfig {
    let ttl_ms = manifest
        .ttl
        .as_ref()
        .and_then(parse_duration_to_ms)
        .unwrap_or(0);

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

    let max_body_size_bytes = manifest
        .max_body_size
        .as_ref()
        .and_then(|s| parse_size_to_bytes(s));

    WorkerConfig {
        enabled: manifest.enabled.unwrap_or(true),
        entrypoint: manifest.entrypoint.clone(),
        env: manifest.env.clone().unwrap_or_default(),
        env_prefix: manifest.env_prefix.clone(),
        ttl_ms,
        timeout_ms,
        idle_timeout_ms,
        max_requests: manifest.max_requests.unwrap_or(0),
        max_body_size_bytes,
        low_memory: manifest.low_memory.unwrap_or(false),
        auto_install: manifest.auto_install.unwrap_or(false),
        inject_base: manifest.inject_base.unwrap_or(true),
        visibility: manifest
            .visibility
            .clone()
            .unwrap_or_else(|| "protected".into()),
        public_routes: manifest.public_routes.clone(),
        cron: manifest.cron.clone().unwrap_or_default(),
        kind: Some(infer_execution_kind(manifest)),
    }
}
