use std::fmt;
use std::fs;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};
use edger_core::AdminWorkerInfo;
use edger_orchestrator::load_manifests_from_dirs;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::contracts::EDGER_SCHEMA_VERSION;

#[derive(Clone)]
pub struct McpContext {
    workspace_root: PathBuf,
    control_plane: Option<ControlPlaneTarget>,
}

#[derive(Clone)]
struct ControlPlaneTarget {
    url: String,
    root_key: String,
}

impl fmt::Debug for McpContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpContext")
            .field("workspace_root", &self.workspace_root)
            .field("control_plane_configured", &self.control_plane.is_some())
            .finish()
    }
}

impl McpContext {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Result<Self> {
        let root = workspace_root.into();
        let workspace_root = canonicalize_existing_dir(&root)?;
        Ok(Self {
            workspace_root,
            control_plane: None,
        })
    }

    pub fn with_control_plane(
        workspace_root: impl Into<PathBuf>,
        url: impl Into<String>,
        root_key: impl Into<String>,
    ) -> Result<Self> {
        let mut context = Self::new(workspace_root)?;
        let url = validate_control_plane_url(&url.into())?;
        let root_key = root_key.into();
        if root_key.trim().is_empty() {
            return Err(anyhow!("EdgeR root key must not be empty"));
        }
        context.control_plane = Some(ControlPlaneTarget { url, root_key });
        Ok(context)
    }

    pub fn from_env() -> Result<Self> {
        let root = std::env::var_os("EDGER_MCP_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir()?);
        match (
            std::env::var("EDGER_URL").ok(),
            std::env::var("EDGER_ROOT_KEY").ok(),
        ) {
            (None, None) => Self::new(root),
            (Some(url), Some(root_key)) => Self::with_control_plane(root, url, root_key),
            _ => Err(anyhow!(
                "EDGER_URL and EDGER_ROOT_KEY must be configured together"
            )),
        }
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub(crate) fn resolve_workspace_root(&self, requested: Option<&str>) -> Result<PathBuf> {
        let Some(requested) = requested else {
            return Ok(self.workspace_root.clone());
        };
        let requested = PathBuf::from(requested);
        let candidate = if requested.is_absolute() {
            requested
        } else {
            self.workspace_root.join(requested)
        };
        let canonical = canonicalize_existing_dir(&candidate)?;
        if !canonical.starts_with(&self.workspace_root) {
            return Err(anyhow!("workspaceRoot escapes configured workspace"));
        }
        Ok(canonical)
    }

    pub(crate) fn resolve_existing_file(
        &self,
        workspace_root: Option<&str>,
        requested: &str,
    ) -> Result<PathBuf> {
        let workspace_root = self.resolve_workspace_root(workspace_root)?;
        let requested = PathBuf::from(requested);
        let candidate = if requested.is_absolute() {
            requested
        } else {
            workspace_root.join(requested)
        };
        let canonical = candidate
            .canonicalize()
            .map_err(|error| anyhow!("file does not exist: {}: {error}", candidate.display()))?;
        if !canonical.starts_with(&workspace_root) {
            return Err(anyhow!("file path escapes configured workspace"));
        }
        if !canonical.is_file() {
            return Err(anyhow!("path is not a file: {}", canonical.display()));
        }
        Ok(canonical)
    }

    pub(crate) fn control_plane(&self) -> Result<(&str, &str)> {
        self.control_plane
            .as_ref()
            .map(|target| (target.url.as_str(), target.root_key.as_str()))
            .ok_or_else(|| {
                anyhow!("EdgeR control plane is not configured; set EDGER_URL and EDGER_ROOT_KEY")
            })
    }
}

fn validate_control_plane_url(raw: &str) -> Result<String> {
    let mut url = Url::parse(raw).map_err(|error| anyhow!("invalid EDGER_URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(anyhow!("EDGER_URL must use http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(anyhow!("EDGER_URL must not contain credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(anyhow!("EDGER_URL must not contain a query or fragment"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("EDGER_URL must include a host"))?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() != "https" && !loopback {
        return Err(anyhow!("non-loopback EDGER_URL must use https"));
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url.to_string())
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerDiscoveryArgs {
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub worker_dirs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectWorkerArgs {
    pub name: String,
    pub version: Option<String>,
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub worker_dirs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteWorkerFileArgs {
    pub path: String,
    pub content: String,
    pub workspace_root: Option<String>,
    #[serde(default = "default_true")]
    pub dry_run: bool,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SafeWorkerInfo {
    pub kind: String,
    pub name: String,
    pub namespace: Option<String>,
    pub origin: String,
    pub plugin_base: Option<String>,
    pub source: String,
    pub status: String,
    pub version: String,
    pub visibility: edger_core::WorkerVisibility,
}

pub fn list_workers(ctx: &McpContext, args: WorkerDiscoveryArgs) -> Result<serde_json::Value> {
    let workspace_root = ctx.resolve_workspace_root(args.workspace_root.as_deref())?;
    let worker_dirs = resolve_worker_dirs(&workspace_root, &args.worker_dirs)?;
    let index = load_manifests_from_dirs(&worker_dirs)?;
    let workers = index
        .admin_workers()
        .into_iter()
        .map(|worker| safe_worker_info(&workspace_root, worker))
        .collect::<Vec<_>>();

    Ok(json!({
        "schemaVersion": EDGER_SCHEMA_VERSION,
        "workspaceRoot": workspace_root.display().to_string(),
        "workerDirs": worker_dirs
            .iter()
            .map(|dir| display_relative(&workspace_root, dir))
            .collect::<Vec<_>>(),
        "count": workers.len(),
        "workers": workers,
    }))
}

pub fn inspect_worker(ctx: &McpContext, args: InspectWorkerArgs) -> Result<serde_json::Value> {
    let workspace_root = ctx.resolve_workspace_root(args.workspace_root.as_deref())?;
    let worker_dirs = resolve_worker_dirs(&workspace_root, &args.worker_dirs)?;
    let index = load_manifests_from_dirs(&worker_dirs)?;
    let mut workers = index
        .admin_workers()
        .into_iter()
        .filter(|worker| worker.name == args.name)
        .filter(|worker| {
            args.version
                .as_ref()
                .is_none_or(|version| worker.version == *version)
        })
        .collect::<Vec<_>>();
    workers.sort_by(|a, b| a.version.cmp(&b.version));
    let Some(worker) = workers.pop() else {
        return Err(anyhow!("worker not found: {}", args.name));
    };

    Ok(json!({
        "schemaVersion": EDGER_SCHEMA_VERSION,
        "worker": safe_worker_info(&workspace_root, worker),
    }))
}

pub fn write_worker_file(ctx: &McpContext, args: WriteWorkerFileArgs) -> Result<serde_json::Value> {
    let workspace_root = ctx.resolve_workspace_root(args.workspace_root.as_deref())?;
    let target = resolve_authoring_path(&workspace_root, &args.path)?;
    let existed = target.exists();
    if existed && !args.overwrite && !args.dry_run {
        return Err(anyhow!("target exists; pass overwrite=true to replace it"));
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("target has no parent directory"))?;
    if !args.dry_run {
        fs::create_dir_all(parent)?;
        fs::write(&target, args.content.as_bytes())?;
    }

    Ok(json!({
        "schemaVersion": EDGER_SCHEMA_VERSION,
        "dryRun": args.dry_run,
        "changed": !args.dry_run,
        "operation": if existed { "replace" } else { "create" },
        "path": display_relative(&workspace_root, &target),
        "bytes": args.content.len(),
    }))
}

pub fn validate_local(ctx: &McpContext, args: WorkerDiscoveryArgs) -> serde_json::Value {
    match list_workers(ctx, args) {
        Ok(inventory) => json!({
            "schemaVersion": EDGER_SCHEMA_VERSION,
            "status": "passed",
            "checks": [
                {
                    "id": "worker-manifest-discovery",
                    "status": "passed",
                    "workers": inventory["count"],
                }
            ],
            "inventory": inventory,
            "remoteDeploy": false,
        }),
        Err(err) => json!({
            "schemaVersion": EDGER_SCHEMA_VERSION,
            "status": "failed",
            "checks": [
                {
                    "id": "worker-manifest-discovery",
                    "status": "failed",
                    "error": err.to_string(),
                }
            ],
            "remoteDeploy": false,
        }),
    }
}

pub fn prepare_commit(
    ctx: &McpContext,
    workspace_root: Option<String>,
) -> Result<serde_json::Value> {
    let workspace_root = ctx.resolve_workspace_root(workspace_root.as_deref())?;
    let status = run_git(
        &workspace_root,
        &["status", "--short", "--untracked-files=all"],
    )?;
    let changed_files = run_git(&workspace_root, &["diff", "--name-only"])?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let staged_files = run_git(&workspace_root, &["diff", "--cached", "--name-only"])?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let suggested_pr_body = [
        "## Summary",
        "- Updates local edger worker/control-plane files.",
        "",
        "## Validation",
        "- Run local edger MCP validation before opening the PR.",
        "",
        "## Remote deploy",
        "- Not included.",
    ]
    .join("\n");

    Ok(json!({
        "schemaVersion": EDGER_SCHEMA_VERSION,
        "workspaceRoot": workspace_root.display().to_string(),
        "statusShort": status.lines().collect::<Vec<_>>(),
        "changedFiles": changed_files,
        "stagedFiles": staged_files,
        "suggestedCommitMessage": "feat: update edger worker control plane",
        "suggestedPrTitle": "Update edger worker control plane",
        "suggestedPrBody": suggested_pr_body,
        "remoteDeploy": false,
    }))
}

fn safe_worker_info(workspace_root: &Path, worker: AdminWorkerInfo) -> SafeWorkerInfo {
    SafeWorkerInfo {
        kind: format!("{:?}", worker.kind),
        name: worker.name,
        namespace: worker.namespace,
        origin: serde_json::to_value(worker.origin)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "user".into()),
        plugin_base: worker.plugin_base,
        source: display_relative(workspace_root, Path::new(&worker.source)),
        status: worker.status,
        version: worker.version,
        visibility: worker.visibility,
    }
}

fn resolve_worker_dirs(workspace_root: &Path, worker_dirs: &[String]) -> Result<Vec<PathBuf>> {
    let dirs = if worker_dirs.is_empty() {
        if workspace_root.join("workers/examples").is_dir() {
            vec!["workers/examples".to_string()]
        } else {
            vec!["workers".to_string()]
        }
    } else {
        worker_dirs.to_vec()
    };
    dirs.iter()
        .map(|dir| resolve_existing_child_dir(workspace_root, dir))
        .collect()
}

fn resolve_existing_child_dir(workspace_root: &Path, requested: &str) -> Result<PathBuf> {
    let requested = PathBuf::from(requested);
    if requested.is_absolute() {
        return Err(anyhow!("workerDirs must be relative to workspaceRoot"));
    }
    let candidate = workspace_root.join(requested);
    let canonical = canonicalize_existing_dir(&candidate)?;
    if !canonical.starts_with(workspace_root) {
        return Err(anyhow!("workerDir escapes workspaceRoot"));
    }
    Ok(canonical)
}

fn resolve_authoring_path(workspace_root: &Path, requested: &str) -> Result<PathBuf> {
    let requested = PathBuf::from(requested);
    if requested.is_absolute() {
        return Err(anyhow!("path must be relative to workspaceRoot"));
    }
    if requested.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(anyhow!("path must not contain parent traversal"));
    }
    if requested
        .components()
        .next()
        .is_none_or(|component| component.as_os_str() != "workers")
    {
        return Err(anyhow!("path must stay under workers/"));
    }
    Ok(workspace_root.join(requested))
}

fn canonicalize_existing_dir(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .map_err(|err| anyhow!("failed to resolve {}: {err}", path.display()))?;
    if !canonical.is_dir() {
        return Err(anyhow!("not a directory: {}", canonical.display()));
    }
    Ok(canonical)
}

fn display_relative(workspace_root: &Path, path: &Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn run_git(workspace_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace_root)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn default_true() -> bool {
    true
}
