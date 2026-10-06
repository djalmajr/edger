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
use sha2::{Digest, Sha256};

/// Cache-control for fingerprinted assets shared by every serving path: a
/// weak ETag identifies the entity across identity/br/gzip variants, so the
/// policy is the same for all of them.
pub(crate) const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

pub fn serve_static_spa(
    request_path: &str,
    base_href: Option<&str>,
    config: &WorkerConfig,
) -> Result<SerializedResponse, IsolationError> {
    serve_static_spa_encoded(request_path, base_href, None, config)
}

/// `serve_static_spa` + the request's `Accept-Encoding` (EDG-4): for an
/// immutable asset with a pre-compressed variant on disk, the variant is
/// served with `content-encoding`, the ORIGINAL's content-type and weak
/// ETag (identity and variants share the entity tag, as with real-time
/// compression) and `Vary: accept-encoding`; an identity response of the
/// same asset also carries `vary: accept-encoding`. Without a variant the
/// behavior is exactly the current one (real-time compression applies).
pub fn serve_static_spa_encoded(
    request_path: &str,
    base_href: Option<&str>,
    accept_encoding: Option<&str>,
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

    let cache_control = cache_control_for(&file_path);
    let etag = weak_etag(&body);
    // A direct request for the variant file (e.g. `/assets/x.js.br`) is
    // served as a plain file, exactly as today: the variant logic only fires
    // for the immutable original.
    if cache_control == IMMUTABLE_CACHE_CONTROL {
        if let Some(response) = serve_precompressed_variant(
            &file_path,
            &body,
            content_type,
            cache_control,
            accept_encoding,
        ) {
            return Ok(response);
        }
        if has_precompressed_variant(&file_path) {
            return Ok(SerializedResponse {
                status: 200,
                headers: vec![
                    ("content-type".into(), content_type.into()),
                    ("cache-control".into(), cache_control.into()),
                    ("vary".into(), "accept-encoding".into()),
                    ("etag".into(), etag),
                ],
                body: Some(Bytes::from(body)),
            });
        }
    }
    Ok(SerializedResponse {
        status: 200,
        headers: vec![
            ("content-type".into(), content_type.into()),
            ("cache-control".into(), cache_control.into()),
            ("etag".into(), etag),
        ],
        body: Some(Bytes::from(body)),
    })
}

/// Pick which pre-compressed variant a request negotiates: `br` beats
/// `gzip` at equal quality; `q=0` rejects an encoding; `*` covers any
/// encoding not listed explicitly; a missing header — or one that accepts
/// neither encoding — means identity. A malformed `q` parameter invalidates
/// the whole field (the field is ignored, like `If-None-Match`).
pub(crate) fn negotiate_variant_encoding(
    accept_encoding: Option<&str>,
) -> Option<crate::precompress::VariantEncoding> {
    use crate::precompress::VariantEncoding;
    let header = accept_encoding?;
    let mut br: Option<f32> = None;
    let mut gzip: Option<f32> = None;
    let mut star: Option<f32> = None;
    for element in header.split(',') {
        let element = element.trim();
        if element.is_empty() {
            continue;
        }
        let (token, params) = match element.split_once(';') {
            Some((token, params)) => (token.trim(), params),
            None => (element, ""),
        };
        let quality = parse_accept_encoding_quality(params)?;
        match token.to_ascii_lowercase().as_str() {
            "br" => br = Some(quality),
            "gzip" => gzip = Some(quality),
            "*" => star = Some(quality),
            // `identity` and unknown codings are simply not variants.
            _ => {}
        }
    }
    // A listed coding wins over `*`; `q=0` is a rejection (RFC 9110 §12.5.1).
    let accepted = |quality: Option<f32>| quality.filter(|q| *q > 0.0);
    let br = accepted(br.or(star));
    let gzip = accepted(gzip.or(star));
    match (br, gzip) {
        (Some(brotli_q), Some(gzip_q)) => {
            if gzip_q > brotli_q {
                Some(VariantEncoding::Gzip)
            } else {
                Some(VariantEncoding::Brotli)
            }
        }
        (Some(_), None) => Some(VariantEncoding::Brotli),
        (None, Some(_)) => Some(VariantEncoding::Gzip),
        (None, None) => None,
    }
}

/// Parse the `q` parameter list of one `Accept-Encoding` element. Returns
/// `None` when the parameters are malformed (non-`q` parameter, unparseable
/// or out-of-range value): the whole field must then be ignored. No `q`
/// parameter means the default weight 1.
fn parse_accept_encoding_quality(params: &str) -> Option<f32> {
    let mut quality: Option<f32> = None;
    for param in params.split(';') {
        let param = param.trim();
        if param.is_empty() {
            continue;
        }
        let (name, value) = param.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("q") {
            return None;
        }
        let value = value.trim();
        quality = Some(
            value
                .parse::<f32>()
                .ok()
                .filter(|q| q.is_finite() && (0.0..=1.0).contains(q))?,
        );
    }
    quality.or(Some(1.0))
}

/// True when `path` is a REGULAR file (not a symlink): `symlink_metadata`
/// inspects the path itself, so a link to a file elsewhere reports `false`.
fn variant_is_regular_file(variant: &Path) -> bool {
    fs::symlink_metadata(variant)
        .ok()
        .is_some_and(|meta| meta.is_file())
}

/// True when at least one pre-compressed variant of `path` exists on disk
/// as a regular file. HTML never qualifies: those entries are transformed at
/// runtime, so a variant (or a `Vary` advertising one) must not be
/// advertised for them.
pub(crate) fn has_precompressed_variant(path: &Path) -> bool {
    if content_type_for(path).starts_with("text/html") {
        return false;
    }
    crate::precompress::VariantEncoding::ALL
        .iter()
        .any(|encoding| variant_is_regular_file(&crate::precompress::variant_path(path, *encoding)))
}

/// Build the pre-compressed variant response for an immutable asset, or
/// `None` when the request negotiates no variant, when the asset is HTML
/// (transformed at runtime: a pre-compressed variant would skip the
/// transformation), or when the variant is missing / not a regular file —
/// the identity path then serves, as before.
pub(crate) fn serve_precompressed_variant(
    path: &Path,
    body: &[u8],
    content_type: &str,
    cache_control: &'static str,
    accept_encoding: Option<&str>,
) -> Option<SerializedResponse> {
    if content_type.starts_with("text/html") {
        return None;
    }
    let encoding = negotiate_variant_encoding(accept_encoding)?;
    let variant = crate::precompress::variant_path(path, encoding);
    // The variant must be a REGULAR file in the same (already validated)
    // directory as the original. A symlink — even to a regular file outside
    // the worker — is refused and the identity path serves instead.
    if !variant_is_regular_file(&variant) {
        return None;
    }
    let compressed = fs::read(&variant).ok()?;
    Some(SerializedResponse {
        status: 200,
        headers: vec![
            ("content-type".into(), content_type.into()),
            ("cache-control".into(), cache_control.into()),
            (
                "content-encoding".into(),
                encoding.content_encoding().into(),
            ),
            ("vary".into(), "accept-encoding".into()),
            // Same weak ETag as the identity response: one entity, several
            // codings (RFC 9110 §8.8.1) — 304s match in every coding.
            ("etag".into(), weak_etag(body)),
        ],
        body: Some(Bytes::from(compressed)),
    })
}

/// The request's `Accept-Encoding` value (case-insensitive header lookup),
/// if any.
pub fn accept_encoding_header(headers: &[(String, String)]) -> Option<&str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("accept-encoding"))
        .map(|(_, value)| value.as_str())
}

/// Weak ETag (`W/"<hex>"`) for a static body: the first 16 hex characters
/// of the SHA-256 of the FINAL body bytes (after the `<base href>`
/// injection on the entry HTML). Weak because the compression layer
/// (tower-http) does not rewrite the ETag when it re-encodes the body; a
/// weak validator is the correct one across identity/br/gzip variants
/// (RFC 9110 §8.8.1). The same tag is shared by the pre-compressed variants
/// (EDG-4), which are codings of the same entity.
pub(crate) fn weak_etag(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let hex = format!("{:x}", digest);
    format!(r#"W/"{}""#, &hex[..16])
}

// HTML is the pointer to everything else and must never stick — a stale SPA
// shell keeps running old code long after a deploy. Fingerprinted assets
// (Vite's `assets/name-<hash>` shape) are immutable by construction; both
// gates are required so an un-hashed user file named e.g. `controller.js`
// never gets pinned for a year. Everything else lives short and revalidates.
//
// The same predicate decides which assets receive pre-compressed variants
// at deploy time (`precompress_worker_assets`): it must stay in lockstep
// with the serving rule, or a variant could exist for a non-immutable file.
pub(crate) fn is_immutable_asset(path: &Path) -> bool {
    if content_type_for(path).starts_with("text/html") {
        return false;
    }
    let under_assets = path
        .parent()
        .and_then(|dir| dir.file_name())
        .is_some_and(|name| name == "assets");
    let hashed_stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit('-').next())
        .is_some_and(|tail| {
            // A digit OR case mixing beyond a leading capital marks a hash:
            // Vite emits base64ish tails like "DgsWFCcn" that carry no digit,
            // while words ("controller", "Controller") never mix case inside.
            let mixed_case = tail.chars().skip(1).any(|c| c.is_ascii_uppercase())
                && tail.chars().any(|c| c.is_ascii_lowercase());
            tail.len() >= 8
                && tail.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && (tail.chars().any(|c| c.is_ascii_digit()) || mixed_case)
        });
    under_assets && hashed_stem
}

fn cache_control_for(path: &Path) -> &'static str {
    if content_type_for(path).starts_with("text/html") {
        return "no-cache";
    }
    if is_immutable_asset(path) {
        IMMUTABLE_CACHE_CONTROL
    } else {
        "public, max-age=300"
    }
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
            vec![
                ("content-type".into(), "text/css; charset=utf-8".into()),
                ("cache-control".into(), "public, max-age=300".into()),
                ("etag".into(), weak_etag(b"body{}").into()),
            ]
        );
        assert_eq!(css.body.unwrap().as_ref(), b"body{}");
    }

    #[test]
    fn static_spa_stamps_weak_etags_on_assets_and_entry_html() {
        // ETag is computed on the FINAL bytes: the entry HTML tag must
        // reflect the <base href> injection, and a different base gives a
        // different tag.
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("index.html"),
            r#"<!doctype html><html><head></head><body>app</body></html>"#,
        )
        .unwrap();
        fs::write(root.path().join("index.css"), "body{}").unwrap();
        fs::write(root.path().join("other.css"), "body{color:red}").unwrap();
        let config = spa_config(root.path());

        let css = serve_static_spa("/index.css", Some("/todos/"), &config).unwrap();
        let css_etag = etag_of(&css).to_string();
        assert!(weak_etag_shape_ok(&css_etag));
        assert_eq!(css_etag, weak_etag(b"body{}"));

        // Same file served again: same ETag.
        let css_again = serve_static_spa("/index.css", Some("/todos/"), &config).unwrap();
        assert_eq!(etag_of(&css_again), css_etag);

        // Different content: different ETag.
        let other = serve_static_spa("/other.css", Some("/todos/"), &config).unwrap();
        assert_ne!(etag_of(&other), css_etag);

        // Entry HTML: ETag over the transformed body; a different
        // <base href> changes the body and therefore the ETag.
        let html_todos = serve_static_spa("/", Some("/todos/"), &config).unwrap();
        let html_other = serve_static_spa("/", Some("/other/"), &config).unwrap();
        let body_todos = html_todos.body.clone().unwrap();
        let body_other = html_other.body.clone().unwrap();
        assert!(String::from_utf8_lossy(&body_todos).contains(r#"<base href="/todos/" />"#));
        assert_eq!(etag_of(&html_todos), weak_etag(body_todos.as_ref()));
        assert_eq!(etag_of(&html_other), weak_etag(body_other.as_ref()));
        assert_ne!(etag_of(&html_todos), etag_of(&html_other));
    }

    fn etag_of(response: &SerializedResponse) -> &str {
        response
            .headers
            .iter()
            .find(|(name, _)| name == "etag")
            .map(|(_, value)| value.as_str())
            .expect("etag header missing")
    }

    fn weak_etag_shape_ok(value: &str) -> bool {
        value
            .strip_prefix(r#"W/""#)
            .and_then(|rest| rest.strip_suffix('"'))
            .is_some_and(|inner| {
                inner.len() == 16 && inner.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    }

    #[test]
    fn cache_policy_pins_only_fingerprinted_assets_and_never_html() {
        // A stale SPA shell keeps running old code after a deploy (seen live:
        // the cPanel kept rewriting URLs out of its proxy prefix). HTML never
        // sticks; only Vite-shaped assets/name-<hash> files are immutable —
        // an un-hashed "controller.js" must not be pinned for a year.
        assert_eq!(cache_control_for(Path::new("/w/index.html")), "no-cache");
        assert_eq!(
            cache_control_for(Path::new("/w/assets/app-a1b2c3d4.js")),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control_for(Path::new(
                "/w/assets/noto-sans-latin-wght-normal-Bx2K9zM1.woff2"
            )),
            "public, max-age=31536000, immutable"
        );
        // Hash-like tail but outside assets/ — short-lived.
        assert_eq!(
            cache_control_for(Path::new("/w/app-a1b2c3d4.js")),
            "public, max-age=300"
        );
        // Digit-less Vite hash (seen live: index-DgsWFCcn.js served as
        // max-age=300) — case mixing marks it as a hash.
        assert_eq!(
            cache_control_for(Path::new("/w/assets/index-DgsWFCcn.js")),
            "public, max-age=31536000, immutable"
        );
        // Words never mix case inside — not hashes, never pinned.
        assert_eq!(
            cache_control_for(Path::new("/w/assets/component-controller.js")),
            "public, max-age=300"
        );
        assert_eq!(
            cache_control_for(Path::new("/w/assets/component-Controller.js")),
            "public, max-age=300"
        );
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

    // ---- EDG-4: pre-compressed variant serving ------------------------------

    #[test]
    fn negotiate_variant_encoding_respects_quality_and_star() {
        use crate::precompress::VariantEncoding;
        let cases: &[(&str, Option<VariantEncoding>)] = &[
            ("br", Some(VariantEncoding::Brotli)),
            ("gzip", Some(VariantEncoding::Gzip)),
            ("*", Some(VariantEncoding::Brotli)),
            ("br;q=0, gzip", Some(VariantEncoding::Gzip)),
            ("gzip;q=0, br", Some(VariantEncoding::Brotli)),
            ("br;q=0, gzip;q=0", None),
            ("gzip;q=0", None),
            ("identity", None),
            ("identity;q=0, gzip", Some(VariantEncoding::Gzip)),
            ("gzip;q=0, *", Some(VariantEncoding::Brotli)),
            ("gzip;q=0.5, br;q=0.2", Some(VariantEncoding::Gzip)),
            ("br;q=0.3, gzip;q=0.3", Some(VariantEncoding::Brotli)),
            ("br , gzip;q=0.9", Some(VariantEncoding::Brotli)),
            ("x-experimental, br;q=0", None), // only an unknown coding accepted: identity
            ("", None),
            (",br", Some(VariantEncoding::Brotli)),
            ("br;q=1.5", None),   // malformed field: ignored, never a variant
            ("br;level=4", None), // non-q parameter: malformed, ignored
            ("BR", Some(VariantEncoding::Brotli)), // token match is case-insensitive
        ];
        for (header, expected) in cases {
            assert_eq!(
                negotiate_variant_encoding(Some(header)),
                *expected,
                "Accept-Encoding: {header:?}"
            );
        }
        // Absent header is identity, no matter what is on disk.
        assert_eq!(negotiate_variant_encoding(None), None);
    }

    fn spa_root_with_variant() -> (tempfile::TempDir, WorkerConfig) {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        let original = vec![b'x'; 2048];
        fs::write(root.path().join("index.html"), "<html></html>").unwrap();
        let asset = root.path().join("assets/app-a1b2c3d4.js");
        fs::write(&asset, &original).unwrap();
        // Variants produced by the deploy-time module itself.
        crate::precompress::precompress_worker_assets(
            root.path(),
            &edger_core::ExecutionKind::StaticSpa { inject_base: true },
            &parse_worker_config(&WorkerManifest::default()),
            u64::MAX,
        );
        let final_config = spa_config(root.path());
        (root, final_config)
    }

    fn response_header<'a>(response: &'a SerializedResponse, name: &str) -> Option<&'a str> {
        response
            .headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn static_spa_serves_negotiated_variant_with_original_etag_and_vary() {
        let (root, config) = spa_root_with_variant();
        let original = fs::read(root.path().join("assets/app-a1b2c3d4.js")).unwrap();

        let br =
            serve_static_spa_encoded("/assets/app-a1b2c3d4.js", None, Some("br"), &config).unwrap();
        let expected_br = fs::read(root.path().join("assets/app-a1b2c3d4.js.br")).unwrap();
        assert_eq!(br.status, 200);
        assert_eq!(response_header(&br, "content-encoding"), Some("br"));
        assert_eq!(
            response_header(&br, "content-type"),
            Some("application/javascript; charset=utf-8")
        );
        assert_eq!(
            response_header(&br, "cache-control"),
            Some("public, max-age=31536000, immutable")
        );
        assert!(response_header(&br, "vary")
            .is_some_and(|vary| vary.to_ascii_lowercase().contains("accept-encoding")));
        // The variant shares the WEAK ETag of the original bytes.
        assert_eq!(
            response_header(&br, "etag"),
            Some(weak_etag(&original).as_str())
        );
        assert_eq!(br.body.unwrap().as_ref(), expected_br.as_slice());

        // `br;q=0, gzip` negotiates the gzip variant.
        let gz = serve_static_spa_encoded(
            "/assets/app-a1b2c3d4.js",
            None,
            Some("br;q=0, gzip"),
            &config,
        )
        .unwrap();
        assert_eq!(response_header(&gz, "content-encoding"), Some("gzip"));
        assert_eq!(
            response_header(&gz, "etag"),
            Some(weak_etag(&original).as_str())
        );
        assert_eq!(
            gz.body.unwrap().as_ref(),
            fs::read(root.path().join("assets/app-a1b2c3d4.js.gz"))
                .unwrap()
                .as_slice()
        );
    }

    #[test]
    fn static_spa_identity_of_variant_asset_carries_vary_but_no_encoding() {
        let (root, config) = spa_root_with_variant();
        let original = fs::read(root.path().join("assets/app-a1b2c3d4.js")).unwrap();

        for accept_encoding in [None, Some("identity"), Some("br;q=0, gzip;q=0")] {
            let res =
                serve_static_spa_encoded("/assets/app-a1b2c3d4.js", None, accept_encoding, &config)
                    .unwrap();
            assert!(
                response_header(&res, "content-encoding").is_none(),
                "Accept-Encoding {accept_encoding:?} must serve identity"
            );
            assert!(
                response_header(&res, "vary")
                    .is_some_and(|vary| vary.to_ascii_lowercase().contains("accept-encoding")),
                "identity of a variant asset must vary on accept-encoding"
            );
            assert_eq!(
                response_header(&res, "etag"),
                Some(weak_etag(&original).as_str())
            );
            assert_eq!(res.body.unwrap().as_ref(), original.as_slice());
        }
    }

    #[test]
    fn static_spa_without_variants_keeps_current_behavior() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("index.html"), "<html></html>").unwrap();
        fs::write(root.path().join("assets/app-a1b2c3d4.js"), vec![b'x'; 2048]).unwrap();
        let config = spa_config(root.path());

        let res =
            serve_static_spa_encoded("/assets/app-a1b2c3d4.js", None, Some("br"), &config).unwrap();
        assert!(response_header(&res, "content-encoding").is_none());
        assert!(
            response_header(&res, "vary").is_none(),
            "no variant, no vary"
        );
        assert_eq!(res.body.as_ref().unwrap(), &vec![b'x'; 2048]);

        let legacy = serve_static_spa("/assets/app-a1b2c3d4.js", None, &config).unwrap();
        assert_eq!(legacy.status, res.status);
        assert_eq!(legacy.headers, res.headers);
        assert_eq!(legacy.body.as_ref().unwrap(), &vec![b'x'; 2048]);
    }

    #[test]
    fn static_spa_never_serves_variant_for_non_immutable_asset() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("index.html"), "<html></html>").unwrap();
        fs::write(root.path().join("assets/controller.js"), vec![b'j'; 2048]).unwrap();
        // A stray variant for a NON-immutable file must be ignored: the file
        // is revalidated (max-age=300) and served identity.
        fs::write(root.path().join("assets/controller.js.br"), b"stray-br").unwrap();
        let config = spa_config(root.path());

        let res =
            serve_static_spa_encoded("/assets/controller.js", None, Some("br"), &config).unwrap();
        assert_eq!(response_header(&res, "content-encoding"), None);
        assert_eq!(response_header(&res, "vary"), None);
        assert_eq!(
            response_header(&res, "cache-control"),
            Some("public, max-age=300")
        );
        assert_eq!(res.body.unwrap().as_ref(), vec![b'j'; 2048].as_slice());

        // A direct request for the variant file serves it as a plain file,
        // exactly as before the feature existed.
        let direct =
            serve_static_spa_encoded("/assets/controller.js.br", None, Some("br"), &config)
                .unwrap();
        assert_eq!(response_header(&direct, "content-encoding"), None);
        assert_eq!(
            response_header(&direct, "content-type"),
            Some("application/octet-stream")
        );
        assert_eq!(direct.body.unwrap().as_ref(), b"stray-br");
    }

    #[test]
    fn static_spa_symlinked_variant_is_never_served() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("assets")).unwrap();
        fs::write(root.path().join("index.html"), "<html></html>").unwrap();
        let original = vec![b'x'; 2048];
        fs::write(root.path().join("assets/app-a1b2c3d4.js"), &original).unwrap();
        // The `.br` variant is a symlink to a file OUTSIDE the worker: the
        // variant must be refused, never the external bytes served.
        let external = b"EXTERNAL-VARIANT-BYTES";
        fs::write(outside.path().join("external.br"), external).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("external.br"),
            root.path().join("assets/app-a1b2c3d4.js.br"),
        )
        .unwrap();
        let config = spa_config(root.path());

        let res =
            serve_static_spa_encoded("/assets/app-a1b2c3d4.js", None, Some("br"), &config).unwrap();
        assert_eq!(res.status, 200);
        // Identity original: no content-encoding, and no Vary advertising a
        // variant that is not a regular file.
        assert!(response_header(&res, "content-encoding").is_none());
        assert_eq!(
            response_header(&res, "etag"),
            Some(weak_etag(&original).as_str())
        );
        assert!(
            response_header(&res, "vary").is_none(),
            "a symlinked variant is not a variant: no Vary"
        );
        assert_eq!(res.body.unwrap().as_ref(), original.as_slice());
    }
}
