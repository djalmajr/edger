//! Deno isolate backend (`--features deno`).
//!
//! The current backend uses the Deno CLI as a process-isolated execution bridge.
//! It keeps the Rust orchestrator and worker pool on the production path while
//! the embedded `deno_core` facade is still pending.

mod bundle;
mod cli;
mod facade;

pub use bundle::{BundleFormat, ModuleBundle, ModuleBundler, StubBundler};
pub use cli::DenoCliRunner;
pub use facade::DenoFacade;

use async_trait::async_trait;
use bytes::Bytes;
use std::fs;
use std::path::{Component, Path, PathBuf};

use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};

fn not_impl(method: &str) -> IsolationError {
    IsolationError::new(
        "NOT_IMPLEMENTED",
        format!("DenoIsolate::{method} pending implementation"),
    )
}

/// JS/TS isolate backed by the Deno CLI bridge.
pub struct DenoIsolate {
    facade: DenoFacade,
    bundler: StubBundler,
    runner: DenoCliRunner,
}

impl DenoIsolate {
    pub fn new(facade: DenoFacade) -> Self {
        Self {
            facade,
            bundler: StubBundler,
            runner: DenoCliRunner::default(),
        }
    }

    pub fn facade(&self) -> &DenoFacade {
        &self.facade
    }

    pub fn bundler(&self) -> &StubBundler {
        &self.bundler
    }

    pub fn runner(&self) -> &DenoCliRunner {
        &self.runner
    }
}

#[async_trait]
impl Isolate for DenoIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.runner.execute_fetch(req, config)
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.runner.execute_fetch(req, config)
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        serve_static_spa(path, base_href, config)
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_wasm"))
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

fn serve_static_spa(
    request_path: &str,
    base_href: Option<&str>,
    config: &WorkerConfig,
) -> Result<SerializedResponse, IsolationError> {
    let entrypoint = resolve_spa_entrypoint(config)?;
    let base_dir = entrypoint
        .parent()
        .ok_or_else(|| {
            IsolationError::new(
                "SPA_ENTRYPOINT_INVALID",
                "SPA entrypoint must have a parent directory",
            )
        })?
        .to_path_buf();
    let requested = resolve_static_request_path(&base_dir, &entrypoint, request_path)?;
    let file_path = if requested.is_file() {
        requested
    } else {
        entrypoint
    };
    let mut body = fs::read(&file_path).map_err(|err| {
        IsolationError::new(
            "SPA_READ_FAILED",
            format!("failed to read {}: {err}", file_path.display()),
        )
    })?;
    let content_type = content_type_for(&file_path);

    if content_type.starts_with("text/html") {
        if let Some(base) = base_href {
            body = inject_base_href(&String::from_utf8_lossy(&body), base).into_bytes();
        }
    }

    Ok(SerializedResponse {
        status: 200,
        headers: vec![("content-type".into(), content_type.into())],
        body: Some(Bytes::from(body)),
    })
}

fn resolve_spa_entrypoint(config: &WorkerConfig) -> Result<PathBuf, IsolationError> {
    let worker_dir = config.worker_dir.as_ref().ok_or_else(|| {
        IsolationError::new("SPA_WORKER_DIR_MISSING", "worker_dir is required for SPA")
    })?;
    let base = worker_dir.canonicalize().map_err(|err| {
        IsolationError::new(
            "SPA_WORKER_DIR_INVALID",
            format!("invalid worker_dir: {err}"),
        )
    })?;
    let entry = config.entrypoint.as_deref().unwrap_or("index.html");
    if entry.contains("..") {
        return Err(IsolationError::new(
            "SPA_ENTRYPOINT_DENIED",
            "entrypoint must stay inside worker_dir",
        ));
    }
    let entrypoint = base.join(entry).canonicalize().map_err(|err| {
        IsolationError::new(
            "SPA_ENTRYPOINT_INVALID",
            format!("invalid SPA entrypoint: {err}"),
        )
    })?;
    if !entrypoint.starts_with(&base) {
        return Err(IsolationError::new(
            "SPA_ENTRYPOINT_DENIED",
            "entrypoint must stay inside worker_dir",
        ));
    }
    Ok(entrypoint)
}

fn resolve_static_request_path(
    base_dir: &Path,
    entrypoint: &Path,
    request_path: &str,
) -> Result<PathBuf, IsolationError> {
    let requested = request_path.trim_start_matches('/');
    if requested.is_empty() {
        return Ok(entrypoint.to_path_buf());
    }
    let relative = Path::new(requested);
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(IsolationError::new(
            "SPA_PATH_DENIED",
            "static path must stay inside SPA directory",
        ));
    }
    let candidate = base_dir.join(relative);
    if candidate.exists() {
        let canonical = candidate.canonicalize().map_err(|err| {
            IsolationError::new("SPA_PATH_INVALID", format!("invalid static path: {err}"))
        })?;
        if !canonical.starts_with(base_dir) {
            return Err(IsolationError::new(
                "SPA_PATH_DENIED",
                "static path must stay inside SPA directory",
            ));
        }
        Ok(canonical)
    } else {
        Ok(entrypoint.to_path_buf())
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()).unwrap_or("") {
        "css" => "text/css; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "ico" => "image/x-icon",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn inject_base_href(html: &str, base_href: &str) -> String {
    let escaped = escape_html_attr(base_href);
    if html.contains("<head>") {
        html.replace("<head>", &format!(r#"<head><base href="{escaped}" />"#))
    } else {
        format!(r#"<base href="{escaped}" />{html}"#)
    }
}

fn escape_html_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use edger_core::{parse_worker_config, WorkerManifest};

    fn spa_config(root: &Path) -> WorkerConfig {
        let manifest = WorkerManifest {
            name: "todos".into(),
            entrypoint: Some("index.html".into()),
            inject_base: Some(true),
            ..WorkerManifest::default()
        };
        let mut config = parse_worker_config(&manifest);
        config.worker_dir = Some(root.to_path_buf());
        config
    }

    #[tokio::test]
    async fn static_spa_serves_index_and_assets() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("index.html"),
            r#"<!doctype html><html><head></head><body></body></html>"#,
        )
        .unwrap();
        fs::write(root.path().join("index.css"), "body{}").unwrap();
        let config = spa_config(root.path());

        let html = serve_static_spa("/", Some("/todos/"), &config).unwrap();
        assert_eq!(html.status, 200);
        assert!(String::from_utf8_lossy(html.body.unwrap().as_ref())
            .contains(r#"<base href="/todos/" />"#));

        let css = serve_static_spa("/index.css", Some("/todos/"), &config).unwrap();
        assert_eq!(
            css.headers,
            vec![("content-type".into(), "text/css; charset=utf-8".into())]
        );
        assert_eq!(css.body.unwrap().as_ref(), b"body{}");
    }

    #[tokio::test]
    async fn static_spa_rejects_parent_paths() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), "<html></html>").unwrap();
        let config = spa_config(root.path());

        let err = serve_static_spa("/../secret", None, &config).unwrap_err();
        assert_eq!(err.code, "SPA_PATH_DENIED");
    }
}
