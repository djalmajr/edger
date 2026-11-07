//! Fullstack `wasm` adapter (EDG-53): SPA assets from `clientDir` plus a WASM
//! API backend behind `wasmBackend.apiPrefix`.
//!
//! Routing is decided on the RAW request pathname:
//! - `apiPrefix` and `apiPrefix/...` (per segment, `/apix` is not API) go to
//!   `Isolate::execute_wasm` with the prefix stripped; the escaped suffix, the
//!   query, the method, headers, body and the authority of an absolute uri are
//!   forwarded untouched. A failing/404 API answer is never replaced by HTML.
//! - a path outside the prefix whose DECODED form falls inside it (`/%61pi/x`,
//!   `/api%2Fx`) is refused with 400: it is never served as SPA.
//! - everything else is a static request: only `GET`/`HEAD` are served (any
//!   other method is 405) from `clientDir`, falling back to its `index.html`.
//!
//! The static checks run per request, before any file is read; the
//! canonicalization of the roots and of the requested file leaves a
//! check-then-read window that this module does not close.

use std::path::Path;

use bytes::Bytes;
use edger_core::execution::WASM_FULLSTACK_ADAPTER;
use edger_core::{
    ExecutionKind, FullstackConfig, Isolate, IsolationError, SerializedRequest, SerializedResponse,
    WorkerConfig,
};

use super::{
    absolute_uri_parts, base_href, path_has_dotfile_components, path_has_forbidden_components,
    percent_decode, resolve_base_path, text_response,
};

/// The fullstack config when (and only when) the adapter is `wasm`.
pub(super) fn wasm_fullstack(config: &WorkerConfig) -> Option<&FullstackConfig> {
    config
        .fullstack
        .as_ref()
        .filter(|fullstack| fullstack.adapter == WASM_FULLSTACK_ADAPTER)
}

enum Route {
    /// API request: the uri to hand to the WASM handler.
    Api(String),
    /// Already answered (static asset, SPA fallback, 400, 404, 405).
    Served(SerializedResponse),
}

pub(super) fn try_serve(
    req: &SerializedRequest,
    config: &WorkerConfig,
    fullstack: &FullstackConfig,
) -> Result<Option<SerializedResponse>, IsolationError> {
    match route(req, config, fullstack)? {
        Route::Api(_) => Ok(None),
        Route::Served(response) => Ok(Some(response)),
    }
}

pub(super) async fn dispatch<I: Isolate + ?Sized>(
    isolate: &mut I,
    mut req: SerializedRequest,
    config: &WorkerConfig,
    fullstack: &FullstackConfig,
) -> Result<SerializedResponse, IsolationError> {
    match route(&req, config, fullstack)? {
        Route::Served(response) => Ok(response),
        Route::Api(uri) => {
            let backend = backend(fullstack)?;
            req.uri = uri;
            // Every other field (env, secrets, egress, limits, worker_dir) is
            // kept as received: no capability is added for the module.
            let mut api_config = config.clone();
            api_config.entrypoint = Some(backend.entrypoint.clone());
            api_config.kind = Some(ExecutionKind::WasmModule {
                entry: Some(backend.entrypoint.clone()),
            });
            isolate.execute_wasm(req, &api_config).await
        }
    }
}

fn backend(
    fullstack: &FullstackConfig,
) -> Result<&edger_core::config::WasmBackendConfig, IsolationError> {
    fullstack.wasm_backend.as_ref().ok_or_else(|| {
        IsolationError::new(
            "FULLSTACK_WASM_BACKEND_MISSING",
            "wasmBackend is required for the wasm adapter",
        )
    })
}

fn route(
    req: &SerializedRequest,
    config: &WorkerConfig,
    fullstack: &FullstackConfig,
) -> Result<Route, IsolationError> {
    let backend = backend(fullstack)?;

    let head_len = req.uri.find('?').unwrap_or(req.uri.len());
    let (head, query) = req.uri.split_at(head_len);
    let (authority, pathname) = match absolute_uri_parts(head) {
        Some((authority, path)) => (Some(authority), path),
        None if head.is_empty() => (None, "/"),
        None => (None, head),
    };

    if let Some(stripped) = backend.strip_api_prefix(pathname) {
        let authority = authority.unwrap_or_default();
        return Ok(Route::Api(format!("{authority}{stripped}{query}")));
    }

    let decoded = match percent_decode(pathname) {
        Ok(decoded) => decoded,
        Err(()) => return Ok(Route::Served(text_response(400, "malformed path"))),
    };
    if !decoded.starts_with('/')
        || decoded.contains('\0')
        || decoded.contains('\\')
        || decoded.split('/').any(|segment| segment == "..")
        || path_has_forbidden_components(decoded.trim_start_matches('/'))
        // Encoded alias of the API prefix: refused, never served as SPA.
        || backend.is_api_path(&decoded)
    {
        return Ok(Route::Served(text_response(400, "malformed path")));
    }

    if req.method != "GET" && req.method != "HEAD" {
        return Ok(Route::Served(method_not_allowed()));
    }

    let relative = decoded.trim_start_matches('/');
    if path_has_dotfile_components(Path::new(relative)) {
        return Ok(Route::Served(text_response(404, "not found")));
    }

    let mut response = serve_static(req, config, fullstack, backend, relative)?;
    if req.method == "HEAD" {
        response.body = None;
    }
    Ok(Route::Served(response))
}

fn method_not_allowed() -> SerializedResponse {
    SerializedResponse {
        status: 405,
        headers: vec![
            ("content-type".into(), "text/plain; charset=utf-8".into()),
            ("allow".into(), "GET, HEAD".into()),
        ],
        body: Some(Bytes::from_static(b"method not allowed")),
    }
}

fn static_error(code: &'static str, message: &'static str) -> IsolationError {
    IsolationError::new(code, message)
}

fn serve_static(
    req: &SerializedRequest,
    config: &WorkerConfig,
    fullstack: &FullstackConfig,
    backend: &edger_core::config::WasmBackendConfig,
    relative: &str,
) -> Result<SerializedResponse, IsolationError> {
    let client_dir = fullstack.client_dir.as_deref().ok_or_else(|| {
        static_error(
            "FULLSTACK_CLIENT_DIR_MISSING",
            "clientDir is required for the wasm adapter",
        )
    })?;
    let worker_dir = config.worker_dir.as_ref().ok_or_else(|| {
        static_error(
            "FULLSTACK_WORKER_DIR_MISSING",
            "worker_dir is required for fullstack assets",
        )
    })?;
    let worker_root = worker_dir.canonicalize().map_err(|_| {
        static_error(
            "FULLSTACK_WORKER_DIR_INVALID",
            "worker_dir is not accessible",
        )
    })?;
    if Path::new(client_dir).is_absolute() || path_has_forbidden_components(client_dir) {
        return Err(static_error(
            "FULLSTACK_CLIENT_DIR_DENIED",
            "clientDir must stay inside worker_dir",
        ));
    }
    let client_root = worker_root.join(client_dir).canonicalize().map_err(|_| {
        static_error(
            "FULLSTACK_CLIENT_DIR_INVALID",
            "clientDir is not accessible",
        )
    })?;
    // Strictly inside: a clientDir equal to the worker root would publish the
    // backend module and the manifest.
    if !client_root.starts_with(&worker_root) || client_root == worker_root || !client_root.is_dir()
    {
        return Err(static_error(
            "FULLSTACK_CLIENT_DIR_DENIED",
            "clientDir must stay inside worker_dir",
        ));
    }
    // The canonical index.html must be a file directly in the public root: a
    // symlink to another directory would move the root the SPA helper serves.
    let index = client_root.join("index.html").canonicalize().map_err(|_| {
        static_error(
            "FULLSTACK_INDEX_INVALID",
            "clientDir must contain a readable index.html",
        )
    })?;
    // Same exclusions as any other asset: no hidden component and not the
    // canonical backend module (an index symlink would otherwise publish both
    // through the fallback).
    let index_hidden = index
        .strip_prefix(&client_root)
        .map(path_has_dotfile_components)
        .unwrap_or(true);
    let index_is_backend = worker_root
        .join(&backend.entrypoint)
        .canonicalize()
        .is_ok_and(|entry| entry == index);
    if !index.is_file()
        || index.parent() != Some(client_root.as_path())
        || index_hidden
        || index_is_backend
    {
        return Err(static_error(
            "FULLSTACK_INDEX_DENIED",
            "index.html must be a file inside clientDir",
        ));
    }

    let mut spa_path = String::from("/");
    if !relative.is_empty() {
        if let Ok(canonical) = client_root.join(relative).canonicalize() {
            if !canonical.starts_with(&client_root) {
                return Ok(text_response(404, "not found"));
            }
            if canonical.is_file() {
                let hidden = canonical
                    .strip_prefix(&client_root)
                    .map(path_has_dotfile_components)
                    .unwrap_or(true);
                let is_backend = worker_root
                    .join(&backend.entrypoint)
                    .canonicalize()
                    .is_ok_and(|entry| entry == canonical);
                if hidden || is_backend {
                    return Ok(text_response(404, "not found"));
                }
                spa_path = format!("/{relative}");
            }
        }
    }

    let mut spa_config = config.clone();
    spa_config.worker_dir = Some(client_root);
    spa_config.entrypoint = Some("index.html".into());
    let base = resolve_base_path(req, &fullstack.base_path);
    crate::static_spa::serve_static_spa_encoded(
        &spa_path,
        Some(&base_href(&base)),
        crate::static_spa::accept_encoding_header(&req.headers),
        &spa_config,
    )
    .map_err(|_| {
        static_error(
            "FULLSTACK_ASSET_READ_FAILED",
            "failed to serve static asset",
        )
    })
}
