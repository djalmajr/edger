//! Static SPA file serving shared by JS backends (bridge v1 and multiproc).
//!
//! Pure Rust: reads files inside the worker dir, blocks path traversal, and
//! injects `<base href>` into HTML when requested. No JS engine involved — a
//! StaticSpa worker never needs a Deno process.

use std::fs;
use std::path::{Component, Path, PathBuf};

use bytes::Bytes;
use edger_core::{IsolationError, SerializedResponse, WorkerConfig};

pub fn serve_static_spa(
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

pub(crate) fn content_type_for(path: &Path) -> &'static str {
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
    use std::fs;
    use std::path::Path;

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

    #[test]
    fn static_spa_serves_index_and_assets() {
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

    #[test]
    fn static_spa_rejects_parent_paths() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("index.html"), "<html></html>").unwrap();
        let config = spa_config(root.path());

        let err = serve_static_spa("/../secret", None, &config).unwrap_err();
        assert_eq!(err.code, "SPA_PATH_DENIED");
    }
}
