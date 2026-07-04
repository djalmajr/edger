//! Static SPA file serving shared by JS backends (bridge v1 and multiproc).
//!
//! Pure Rust: reads files inside the worker dir, blocks path traversal, and
//! injects `<base href>` into HTML when requested. No JS engine involved — a
//! StaticSpa worker never needs a Deno process.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use bytes::Bytes;
use edger_core::{is_sensitive_env_key, IsolationError, SerializedResponse, WorkerConfig};

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
        entrypoint.clone()
    };
    let mut body = fs::read(&file_path).map_err(|err| {
        IsolationError::new(
            "SPA_READ_FAILED",
            format!("failed to read {}: {err}", file_path.display()),
        )
    })?;
    let content_type = content_type_for(&file_path);

    if content_type.starts_with("text/html") && file_path == entrypoint {
        body = transform_entry_html(body, base_href, config);
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

pub(crate) fn transform_entry_html(
    body: Vec<u8>,
    base_href: Option<&str>,
    config: &WorkerConfig,
) -> Vec<u8> {
    let public_env = public_runtime_env(config);
    if base_href.is_none() && public_env.is_empty() {
        return body;
    }

    let mut html = String::from_utf8_lossy(&body).into_owned();
    if let Some(base) = base_href {
        html = rewrite_base_href(&html, base);
    }
    if !public_env.is_empty() {
        html = inject_public_env_script(&html, &public_env);
    }
    html.into_bytes()
}

fn public_runtime_env(config: &WorkerConfig) -> BTreeMap<String, String> {
    config
        .public_env
        .iter()
        .filter_map(|key| {
            let key = key.trim();
            if key.is_empty() || is_sensitive_env_key(key) {
                return None;
            }
            config
                .env
                .get(key)
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect()
}

fn rewrite_base_href(html: &str, base_href: &str) -> String {
    let escaped = escape_html_attr(base_href);
    let base_tag = format!(r#"<base href="{escaped}" />"#);
    if let Some((start, end)) = find_html_tag(html, "base") {
        let mut next = String::with_capacity(html.len() + base_tag.len());
        next.push_str(&html[..start]);
        next.push_str(&base_tag);
        next.push_str(&html[end..]);
        next
    } else {
        insert_after_opening_head(html, &base_tag)
    }
}

fn inject_public_env_script(html: &str, public_env: &BTreeMap<String, String>) -> String {
    let json =
        serde_json::to_string(public_env).expect("string map JSON serialization cannot fail");
    let json = escape_inline_script_json(&json);
    let script = format!("<script>window.__env__={json};</script>");
    insert_before_closing_head(html, &script)
}

fn insert_after_opening_head(html: &str, fragment: &str) -> String {
    if let Some((_, end)) = find_html_tag(html, "head") {
        let mut next = String::with_capacity(html.len() + fragment.len());
        next.push_str(&html[..end]);
        next.push_str(fragment);
        next.push_str(&html[end..]);
        next
    } else {
        format!("{fragment}{html}")
    }
}

fn insert_before_closing_head(html: &str, fragment: &str) -> String {
    if let Some(index) = find_ascii_case_insensitive(html, "</head>") {
        let mut next = String::with_capacity(html.len() + fragment.len());
        next.push_str(&html[..index]);
        next.push_str(fragment);
        next.push_str(&html[index..]);
        next
    } else {
        format!("{fragment}{html}")
    }
}

fn find_html_tag(html: &str, tag: &str) -> Option<(usize, usize)> {
    let needle = format!("<{tag}");
    let mut offset = 0;
    while let Some(relative_start) = find_ascii_case_insensitive(&html[offset..], &needle) {
        let start = offset + relative_start;
        let after_name = start + needle.len();
        let boundary_matches = match html[after_name..].chars().next() {
            Some(ch) => ch.is_ascii_whitespace() || ch == '>' || ch == '/',
            None => true,
        };
        if boundary_matches {
            let end = html[after_name..].find('>')? + after_name + 1;
            return Some((start, end));
        }
        offset = after_name;
    }
    None
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn escape_inline_script_json(value: &str) -> String {
    value
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
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
    use std::collections::HashMap;
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
        // Guards against applying entry HTML rewrites to non-HTML assets.
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("index.html"),
            r#"<!doctype html><html><head><base href="/old/" /></head><body></body></html>"#,
        )
        .unwrap();
        fs::write(root.path().join("index.css"), "body{}").unwrap();
        let config = spa_config(root.path());

        let html = serve_static_spa("/", Some("/todos/"), &config).unwrap();
        assert_eq!(html.status, 200);
        let html_body = String::from_utf8_lossy(html.body.unwrap().as_ref()).into_owned();
        assert!(html_body.contains(r#"<base href="/todos/" />"#));
        assert!(!html_body.contains(r#"<base href="/old/" />"#));

        let css = serve_static_spa("/index.css", Some("/todos/"), &config).unwrap();
        assert_eq!(
            css.headers,
            vec![("content-type".into(), "text/css; charset=utf-8".into())]
        );
        assert_eq!(css.body.unwrap().as_ref(), b"body{}");
    }

    #[test]
    fn static_spa_injects_declared_public_env_and_filters_sensitive_keys() {
        // Guards against serializing manifest env without the publicEnv allowlist.
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("index.html"),
            r#"<!doctype html><html><head></head><body></body></html>"#,
        )
        .unwrap();
        let mut config = spa_config(root.path());
        config.env = HashMap::from([
            ("PUBLIC_API_URL".into(), "https://api.example.test".into()),
            ("PUBLIC_FLAG".into(), "enabled".into()),
            ("OPENAI_API_KEY".into(), "sk-secret".into()),
            ("ADMIN_PASSWORD".into(), "password-secret".into()),
        ]);
        config.public_env = vec![
            "PUBLIC_API_URL".into(),
            "PUBLIC_FLAG".into(),
            "OPENAI_API_KEY".into(),
            "ADMIN_PASSWORD".into(),
        ];

        let html = serve_static_spa("/", None, &config).unwrap();
        let body = String::from_utf8_lossy(html.body.unwrap().as_ref()).into_owned();

        assert!(body.contains("<script>window.__env__="));
        assert!(body.contains(r#""PUBLIC_API_URL":"https://api.example.test""#));
        assert!(body.contains(r#""PUBLIC_FLAG":"enabled""#));
        assert!(!body.contains("OPENAI_API_KEY"));
        assert!(!body.contains("ADMIN_PASSWORD"));
        assert!(!body.contains("sk-secret"));
        assert!(!body.contains("password-secret"));
    }

    #[test]
    fn static_spa_does_not_inject_runtime_env_without_public_env() {
        // Guards against treating every manifest env key as browser-visible.
        let root = tempfile::tempdir().unwrap();
        let original = r#"<!doctype html><html><head></head><body>plain</body></html>"#;
        fs::write(root.path().join("index.html"), original).unwrap();
        let mut config = spa_config(root.path());
        config.env = HashMap::from([("PUBLIC_FLAG".into(), "enabled".into())]);

        let html = serve_static_spa("/", None, &config).unwrap();

        assert_eq!(html.body.unwrap().as_ref(), original.as_bytes());
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
