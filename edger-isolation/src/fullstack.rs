//! Fullstack adapter dispatch glue.
//!
//! The adapter layer only handles manifest-declared static assets and request
//! shape. SSR still runs through the existing fetch backend.

use std::fs;
use std::path::{Component, Path, PathBuf};

use bytes::Bytes;
use edger_core::{
    FullstackBasePath, Isolate, IsolationError, SerializedRequest, SerializedResponse,
    WorkerConfig, WorkerResponse,
};

pub async fn dispatch_fullstack_buffered<I: Isolate + ?Sized>(
    isolate: &mut I,
    req: SerializedRequest,
    config: &WorkerConfig,
) -> Result<SerializedResponse, IsolationError> {
    if let Some(asset) = try_serve_fullstack_asset(&req, config)? {
        return Ok(asset);
    }
    let (req, config) = prepare_fullstack_request(req, config)?;
    isolate.execute_fetch(req, &config).await
}

pub async fn dispatch_fullstack_stream<I: Isolate + ?Sized>(
    isolate: &mut I,
    req: SerializedRequest,
    config: &WorkerConfig,
) -> Result<WorkerResponse, IsolationError> {
    if let Some(asset) = try_serve_fullstack_asset(&req, config)? {
        return Ok(WorkerResponse::Buffered(asset));
    }
    let (req, config) = prepare_fullstack_request(req, config)?;
    isolate.execute_fetch_stream(req, &config).await
}

pub fn try_serve_fullstack_asset(
    req: &SerializedRequest,
    config: &WorkerConfig,
) -> Result<Option<SerializedResponse>, IsolationError> {
    let Some(fullstack) = config.fullstack.as_ref() else {
        return Ok(None);
    };
    let Some(client_dir) = fullstack.client_dir.as_deref() else {
        return Ok(None);
    };
    if fullstack.asset_prefixes.is_empty() {
        return Ok(None);
    }

    let path = request_path(&req.uri);
    if fullstack.adapter == "tanstack" && is_tanstack_server_path(path) {
        return Ok(None);
    }
    if !matches_asset_prefix(path, &fullstack.asset_prefixes) {
        return Ok(None);
    }

    let decoded = match percent_decode(path) {
        Ok(decoded) => decoded,
        Err(_) => return Ok(Some(text_response(400, "malformed path"))),
    };
    if !decoded.starts_with('/') || decoded.contains('\0') {
        return Ok(Some(text_response(400, "malformed path")));
    }

    let relative = decoded.trim_start_matches('/');
    if path_has_forbidden_components(relative) {
        return Ok(Some(text_response(404, "not found")));
    }

    let client_root = resolve_client_root(config, client_dir)?;
    let candidate = client_root.join(relative);
    let file_path = match candidate.canonicalize() {
        Ok(path) if path.starts_with(&client_root) && path.is_file() => path,
        _ => return Ok(Some(text_response(404, "not found"))),
    };
    let mut body = fs::read(&file_path).map_err(|err| {
        IsolationError::new(
            "FULLSTACK_ASSET_READ_FAILED",
            format!("failed to read {}: {err}", file_path.display()),
        )
    })?;
    let content_type = crate::static_spa::content_type_for(&file_path);
    if content_type.starts_with("text/html") && is_client_index_html(&file_path, &client_root) {
        let base_path = resolve_base_path(req, &fullstack.base_path);
        let entry_base_href = base_href(&base_path);
        body = crate::static_spa::transform_entry_html(body, Some(&entry_base_href), config);
    }

    Ok(Some(SerializedResponse {
        status: 200,
        headers: vec![
            ("content-type".into(), content_type.into()),
            ("cache-control".into(), cache_control_for(path).into()),
        ],
        body: Some(Bytes::from(body)),
    }))
}

pub fn prepare_fullstack_request(
    mut req: SerializedRequest,
    config: &WorkerConfig,
) -> Result<(SerializedRequest, WorkerConfig), IsolationError> {
    let fullstack = config.fullstack.as_ref().ok_or_else(|| {
        IsolationError::new(
            "FULLSTACK_CONFIG_MISSING",
            "fullstack config is required for kind fullstack",
        )
    })?;
    let ssr_entrypoint = fullstack.ssr_entrypoint.as_deref().ok_or_else(|| {
        IsolationError::new(
            "FULLSTACK_ENTRYPOINT_MISSING",
            "ssrEntrypoint is required for kind fullstack",
        )
    })?;

    let base_path = resolve_base_path(&req, &fullstack.base_path);
    set_header(&mut req.headers, "x-base", &base_path);
    req.base_href = Some(base_href(&base_path));

    match fullstack.adapter.as_str() {
        "tanstack" | "sveltekit" => {
            req.uri = prepend_base_path(&req.uri, &base_path);
        }
        "hono" => {}
        adapter => {
            return Err(IsolationError::new(
                "FULLSTACK_ADAPTER_INVALID",
                format!("unsupported fullstack adapter {adapter:?}"),
            ));
        }
    }

    let mut config = config.clone();
    config.entrypoint = Some(ssr_entrypoint.to_string());
    Ok((req, config))
}

fn resolve_client_root(config: &WorkerConfig, client_dir: &str) -> Result<PathBuf, IsolationError> {
    let worker_dir = config.worker_dir.as_ref().ok_or_else(|| {
        IsolationError::new(
            "FULLSTACK_WORKER_DIR_MISSING",
            "worker_dir is required for fullstack assets",
        )
    })?;
    let worker_root = worker_dir.canonicalize().map_err(|err| {
        IsolationError::new(
            "FULLSTACK_WORKER_DIR_INVALID",
            format!("invalid worker_dir: {err}"),
        )
    })?;
    let relative = Path::new(client_dir);
    if path_has_forbidden_components(client_dir) || relative.is_absolute() {
        return Err(IsolationError::new(
            "FULLSTACK_CLIENT_DIR_DENIED",
            "clientDir must stay inside worker_dir",
        ));
    }
    let client_root = worker_root.join(relative).canonicalize().map_err(|err| {
        IsolationError::new(
            "FULLSTACK_CLIENT_DIR_INVALID",
            format!("invalid clientDir: {err}"),
        )
    })?;
    if !client_root.starts_with(&worker_root) {
        return Err(IsolationError::new(
            "FULLSTACK_CLIENT_DIR_DENIED",
            "clientDir must stay inside worker_dir",
        ));
    }
    Ok(client_root)
}

fn is_tanstack_server_path(path: &str) -> bool {
    path == "/api"
        || path.starts_with("/api/")
        || path == "/_serverFn"
        || path.starts_with("/_serverFn/")
}

fn matches_asset_prefix(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        let prefix = prefix.trim_end_matches('/');
        if prefix.is_empty() {
            return path.starts_with('/');
        }
        path == prefix || path.starts_with(&format!("{prefix}/"))
    })
}

fn is_client_index_html(file_path: &Path, client_root: &Path) -> bool {
    file_path.parent() == Some(client_root)
        && file_path.file_name().and_then(|name| name.to_str()) == Some("index.html")
}

fn path_has_forbidden_components(path: &str) -> bool {
    Path::new(path).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

fn request_path(uri: &str) -> &str {
    let without_query = uri.split_once('?').map_or(uri, |(path, _)| path);
    if let Some((_, after_scheme)) = without_query.split_once("://") {
        if let Some(index) = after_scheme.find('/') {
            &after_scheme[index..]
        } else {
            "/"
        }
    } else if without_query.is_empty() {
        "/"
    } else {
        without_query
    }
}

fn percent_decode(input: &str) -> Result<String, ()> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(());
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| ())
}

fn hex_value(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
    }
}

fn resolve_base_path(req: &SerializedRequest, base_path: &FullstackBasePath) -> String {
    match base_path {
        FullstackBasePath::Fixed(path) => normalize_base_path(path),
        FullstackBasePath::Auto => req
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-base"))
            .map(|(_, value)| normalize_base_path(value))
            .or_else(|| {
                req.base_href
                    .as_deref()
                    .map(|base| normalize_base_path(base.trim_end_matches('/')))
            })
            .unwrap_or_else(|| "/".into()),
    }
}

fn normalize_base_path(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".into();
    }
    format!("/{}", trimmed.trim_matches('/'))
}

fn base_href(base_path: &str) -> String {
    if base_path == "/" {
        "/".into()
    } else {
        format!("{}/", base_path.trim_end_matches('/'))
    }
}

fn prepend_base_path(uri: &str, base_path: &str) -> String {
    if base_path == "/" {
        return uri.to_string();
    }
    let (path, query) = uri
        .split_once('?')
        .map_or((uri, None), |(path, query)| (path, Some(query)));
    let next_path = if path == base_path || path.starts_with(&format!("{base_path}/")) {
        path.to_string()
    } else if path == "/" || path.is_empty() {
        format!("{base_path}/")
    } else if path.starts_with('/') {
        format!("{base_path}{path}")
    } else {
        format!("{base_path}/{path}")
    };
    if let Some(query) = query {
        format!("{next_path}?{query}")
    } else {
        next_path
    }
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some((_, existing)) = headers
        .iter_mut()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
    {
        *existing = value.to_string();
    } else {
        headers.push((name.to_string(), value.to_string()));
    }
}

fn cache_control_for(path: &str) -> &'static str {
    if path == "/assets" || path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

fn text_response(status: u16, text: &'static str) -> SerializedResponse {
    SerializedResponse {
        status,
        headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
        body: Some(Bytes::from_static(text.as_bytes())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edger_core::{parse_worker_config, WorkerManifest};
    use std::collections::HashMap;

    fn config(root: &Path) -> WorkerConfig {
        let manifest = WorkerManifest {
            name: "tanstack-demo".into(),
            adapter: Some("tanstack".into()),
            client_dir: Some("client".into()),
            kind: Some("fullstack".into()),
            ssr_entrypoint: Some("server/server.js".into()),
            ..WorkerManifest::default()
        };
        let mut config = parse_worker_config(&manifest);
        config.worker_dir = Some(root.to_path_buf());
        config
    }

    fn config_from_manifest(root: &Path, manifest: WorkerManifest) -> WorkerConfig {
        let mut config = parse_worker_config(&manifest);
        config.worker_dir = Some(root.to_path_buf());
        config
    }

    fn req(uri: &str) -> SerializedRequest {
        SerializedRequest {
            method: "GET".into(),
            uri: uri.into(),
            headers: vec![("x-base".into(), "/tanstack-demo".into())],
            body: None,
            request_id: "req".into(),
            base_href: Some("/tanstack-demo/".into()),
        }
    }

    #[test]
    fn fullstack_asset_serves_file_inside_client_dir() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("client/assets")).unwrap();
        fs::write(root.path().join("client/assets/app.css"), "body{}").unwrap();
        let config = config(root.path());

        let res = try_serve_fullstack_asset(&req("/assets/app.css"), &config)
            .unwrap()
            .unwrap();

        assert_eq!(res.status, 200);
        assert_eq!(res.body.unwrap().as_ref(), b"body{}");
    }

    #[test]
    fn fullstack_index_html_injects_public_env_and_auto_base() {
        // Guards against skipping the fullstack client index transform path.
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("client")).unwrap();
        fs::write(
            root.path().join("client/index.html"),
            r#"<!doctype html><html><head><base href="/old/" /></head><body></body></html>"#,
        )
        .unwrap();
        let manifest = WorkerManifest {
            name: "tanstack-demo".into(),
            adapter: Some("tanstack".into()),
            asset_prefixes: vec!["/index.html".into()],
            client_dir: Some("client".into()),
            env: Some(HashMap::from([
                ("PUBLIC_API_URL".into(), "https://api.example.test".into()),
                ("OPENAI_API_KEY".into(), "sk-secret".into()),
            ])),
            kind: Some("fullstack".into()),
            public_env: vec!["PUBLIC_API_URL".into(), "OPENAI_API_KEY".into()],
            ssr_entrypoint: Some("server/server.js".into()),
            ..WorkerManifest::default()
        };
        let config = config_from_manifest(root.path(), manifest);

        let res = try_serve_fullstack_asset(&req("/index.html"), &config)
            .unwrap()
            .unwrap();
        let body = String::from_utf8_lossy(res.body.unwrap().as_ref()).into_owned();

        assert!(body.contains(r#"<base href="/tanstack-demo/" />"#));
        assert!(!body.contains(r#"<base href="/old/" />"#));
        assert!(body.contains(r#""PUBLIC_API_URL":"https://api.example.test""#));
        assert!(!body.contains("OPENAI_API_KEY"));
        assert!(!body.contains("sk-secret"));
    }

    #[test]
    fn fullstack_index_html_uses_fixed_base_path() {
        // Guards against resolving fullstack asset base href only from x-base.
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("client")).unwrap();
        fs::write(
            root.path().join("client/index.html"),
            r#"<!doctype html><html><head></head><body></body></html>"#,
        )
        .unwrap();
        let manifest = WorkerManifest {
            name: "tanstack-demo".into(),
            adapter: Some("tanstack".into()),
            asset_prefixes: vec!["/index.html".into()],
            base_path: Some("/fixed-app".into()),
            client_dir: Some("client".into()),
            kind: Some("fullstack".into()),
            ssr_entrypoint: Some("server/server.js".into()),
            ..WorkerManifest::default()
        };
        let config = config_from_manifest(root.path(), manifest);

        let res = try_serve_fullstack_asset(&req("/index.html"), &config)
            .unwrap()
            .unwrap();
        let body = String::from_utf8_lossy(res.body.unwrap().as_ref()).into_owned();

        assert!(body.contains(r#"<base href="/fixed-app/" />"#));
        assert!(!body.contains(r#"<base href="/tanstack-demo/" />"#));
        assert!(!body.contains("window.__env__"));
    }

    #[test]
    fn fullstack_asset_rejects_malformed_and_traversal_paths() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("client/assets")).unwrap();
        let config = config(root.path());

        let malformed = try_serve_fullstack_asset(&req("/assets/%E0%A4%A"), &config)
            .unwrap()
            .unwrap();
        assert_eq!(malformed.status, 400);

        let traversal = try_serve_fullstack_asset(&req("/assets/%2e%2e/server.js"), &config)
            .unwrap()
            .unwrap();
        assert_eq!(traversal.status, 404);
    }

    #[test]
    fn fullstack_request_restores_base_for_tanstack_ssr() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("client")).unwrap();
        let config = config(root.path());

        let (req, config) = prepare_fullstack_request(req("/about?tab=1"), &config).unwrap();

        assert_eq!(req.uri, "/tanstack-demo/about?tab=1");
        assert_eq!(req.base_href.as_deref(), Some("/tanstack-demo/"));
        assert_eq!(config.entrypoint.as_deref(), Some("server/server.js"));
    }
}
