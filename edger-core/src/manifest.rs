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

/// Human-editable worker manifest (from manifest.yaml / package.json fallback).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkerManifest {
    #[serde(default)]
    pub name: String,
    pub version: Option<String>,
    pub enabled: Option<bool>,
    pub entrypoint: Option<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub env_prefix: Vec<String>,
    #[serde(default, alias = "public_env")]
    pub public_env: Vec<String>,
    #[serde(default, alias = "allow_net")]
    pub allow_net: Option<Vec<String>>,
    pub ttl: Option<serde_yaml::Value>,
    pub timeout: Option<String>,
    pub idle_timeout: Option<String>,
    pub max_requests: Option<u32>,
    pub concurrency: Option<usize>,
    pub min_processes: Option<usize>,
    pub max_processes: Option<usize>,
    #[serde(default, alias = "circuit_breaker_failures")]
    pub circuit_breaker_failures: Option<u32>,
    pub cooldown: Option<serde_yaml::Value>,
    pub isolation: Option<WorkerIsolation>,
    pub queue_limit: Option<usize>,
    pub queue_timeout: Option<serde_yaml::Value>,
    pub max_body_size: Option<String>,
    pub low_memory: Option<bool>,
    pub auto_install: Option<bool>,
    #[serde(default, alias = "deno_cache_mode")]
    pub deno_cache_mode: Option<DenoCacheMode>,
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
    pub dependencies: Option<Vec<String>>,
}
