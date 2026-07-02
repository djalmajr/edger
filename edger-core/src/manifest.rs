//! Worker manifest types (human-editable manifest.yaml form).

use serde::{Deserialize, Serialize};

use crate::bindings::BindingManifest;

/// Public route bypass configuration (Buntime `publicRoutes`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PublicRoutesConfig {
    #[serde(default)]
    pub routes: Vec<String>,
    #[serde(default)]
    pub exact: bool,
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
    pub ttl: Option<serde_yaml::Value>,
    pub timeout: Option<String>,
    pub idle_timeout: Option<String>,
    pub max_requests: Option<u32>,
    pub max_body_size: Option<String>,
    pub low_memory: Option<bool>,
    pub auto_install: Option<bool>,
    pub inject_base: Option<bool>,
    pub visibility: Option<String>,
    pub public_routes: Option<PublicRoutesConfig>,
    pub cron: Option<Vec<CronJob>>,
    pub kind: Option<String>,
    pub base: Option<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    pub dependencies: Option<Vec<String>>,
    #[serde(default)]
    pub shell_excludes: Vec<String>,
    #[serde(default)]
    pub bindings: Vec<BindingManifest>,
}
