//! Worker reference used by orchestrator resolution.

use std::path::PathBuf;

use uuid::Uuid;

use crate::config::WorkerConfig;
use crate::execution::{normalize_fullstack_adapter, ExecutionKind, SUPPORTED_FULLSTACK_ADAPTERS};
use crate::manifest::WorkerManifest;

/// Resolved worker identity (namespaced + semver).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerRef {
    pub id: Uuid,
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
    pub namespace: Option<String>,
    pub kind: ExecutionKind,
    pub config: WorkerConfig,
}

/// Parse `@scope/name` into namespace + local name.
pub fn parse_namespaced_name(full_name: &str) -> (Option<String>, String) {
    if let Some((scope, local)) = full_name.split_once('/') {
        if scope.starts_with('@') {
            return (Some(scope.to_string()), local.to_string());
        }
    }
    (None, full_name.to_string())
}

/// Create a worker ref from manifest + directory (pure; no filesystem reads).
pub fn create_worker_ref(
    dir: PathBuf,
    manifest: WorkerManifest,
) -> Result<WorkerRef, crate::error::CoreError> {
    if manifest.name.trim().is_empty() {
        return Err(crate::error::CoreError::validation(
            "manifest.name",
            "name is required",
        ));
    }
    validate_worker_manifest(&manifest)?;
    let version = manifest.version.clone().unwrap_or_else(|| "latest".into());
    let config = crate::config::parse_worker_config(&manifest);
    let kind = config.kind.clone().unwrap_or(ExecutionKind::FetchHandler);
    let (namespace, local_name) = parse_namespaced_name(&manifest.name);

    Ok(WorkerRef {
        id: Uuid::new_v4(),
        name: if namespace.is_some() {
            manifest.name.clone()
        } else {
            local_name
        },
        version,
        dir,
        namespace,
        kind,
        config,
    })
}

pub fn validate_worker_manifest(manifest: &WorkerManifest) -> Result<(), crate::error::CoreError> {
    if let Some(check) = manifest.health_check.as_ref() {
        if !check.path.starts_with('/') || check.path.starts_with("//") || check.path.contains("..")
        {
            return Err(crate::error::CoreError::validation(
                "manifest.healthCheck.path",
                "healthCheck.path must be an absolute worker pathname without traversal",
            ));
        }
        let method = check
            .method
            .as_deref()
            .unwrap_or("GET")
            .to_ascii_uppercase();
        if !matches!(method.as_str(), "GET" | "HEAD") {
            return Err(crate::error::CoreError::validation(
                "manifest.healthCheck.method",
                "healthCheck.method must be GET or HEAD",
            ));
        }
        if let Some(timeout) = check.timeout.as_deref() {
            let Some(timeout_ms) = crate::config::parse_duration_string_to_ms(timeout) else {
                return Err(crate::error::CoreError::validation(
                    "manifest.healthCheck.timeout",
                    "healthCheck.timeout must be a duration such as 500ms or 2s",
                ));
            };
            if !(100..=10_000).contains(&timeout_ms) {
                return Err(crate::error::CoreError::validation(
                    "manifest.healthCheck.timeout",
                    "healthCheck.timeout must be between 100ms and 10s",
                ));
            }
        }
    }

    let Some(kind) = manifest.kind.as_deref() else {
        return Ok(());
    };
    if !matches!(
        kind.trim().to_ascii_lowercase().as_str(),
        "ssr" | "fullstack"
    ) {
        return Ok(());
    }

    let adapters = SUPPORTED_FULLSTACK_ADAPTERS.join(", ");
    let Some(adapter) = manifest
        .adapter
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Err(crate::error::CoreError::validation(
            "manifest.adapter",
            format!("adapter is required for kind fullstack (expected one of: {adapters})"),
        ));
    };
    if normalize_fullstack_adapter(adapter).is_none() {
        return Err(crate::error::CoreError::validation(
            "manifest.adapter",
            format!("unsupported adapter {adapter:?} (expected one of: {adapters})"),
        ));
    }

    let has_ssr_entrypoint = manifest
        .ssr_entrypoint
        .as_deref()
        .or(manifest.entrypoint.as_deref())
        .map(str::trim)
        .is_some_and(|entry| !entry.is_empty());
    if !has_ssr_entrypoint {
        return Err(crate::error::CoreError::validation(
            "manifest.ssrEntrypoint",
            "ssrEntrypoint is required for kind fullstack (entrypoint is accepted as an alias)",
        ));
    }

    if let Some(base_path) = manifest.base_path.as_deref().map(str::trim) {
        if !base_path.is_empty()
            && !base_path.eq_ignore_ascii_case("auto")
            && !base_path.starts_with('/')
        {
            return Err(crate::error::CoreError::validation(
                "manifest.basePath",
                "basePath must be \"auto\" or an absolute path starting with /",
            ));
        }
    }

    Ok(())
}
