//! Worker reference used by orchestrator resolution.

use std::path::PathBuf;

use uuid::Uuid;

use crate::config::WorkerConfig;
use crate::execution::ExecutionKind;
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
