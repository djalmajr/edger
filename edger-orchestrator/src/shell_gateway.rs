//! Shell routing helpers for the gateway/app-shell contract.

use axum::http::HeaderMap;
use edger_core::WorkerRef;

use crate::manifest_index_stub::ManifestIndex;

const API_PREFIXES: [&str; 3] = ["/api", "/_/api", "/gateway/api"];
const FRAME_DESTINATIONS: [&str; 3] = ["iframe", "embed", "object"];

pub fn resolve_shell_worker(
    method: &str,
    path: &str,
    headers: &HeaderMap,
    index: &ManifestIndex,
) -> Option<WorkerRef> {
    if !matches!(method.to_ascii_uppercase().as_str(), "GET" | "HEAD") {
        return None;
    }
    let normalized = normalize_path(path);
    if is_reserved_or_api_path(&normalized) || is_frame_embedding(headers) {
        return None;
    }
    let shell = index.shell()?;
    if is_shell_excluded(&normalized, &shell.config.shell_excludes) {
        return None;
    }
    if should_route_to_shell(&normalized, headers) {
        Some(shell)
    } else {
        None
    }
}

fn should_route_to_shell(path: &str, headers: &HeaderMap) -> bool {
    fetch_dest(headers).is_some_and(|dest| dest == "document")
        || path == "/"
        || is_single_segment_path(path)
}

fn is_shell_excluded(path: &str, excludes: &[String]) -> bool {
    basename(path).is_some_and(|basename| {
        excludes
            .iter()
            .filter_map(|exclude| normalize_exclude(exclude))
            .any(|exclude| exclude == basename)
    })
}

fn basename(path: &str) -> Option<&str> {
    path.trim_start_matches('/')
        .split('/')
        .find(|part| !part.is_empty())
}

fn normalize_exclude(exclude: &str) -> Option<&str> {
    let exclude = exclude.trim();
    if exclude.is_empty()
        || !exclude
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return None;
    }
    Some(exclude)
}

fn is_single_segment_path(path: &str) -> bool {
    let segments = path
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count();
    segments == 1
}

fn is_frame_embedding(headers: &HeaderMap) -> bool {
    fetch_dest(headers).is_some_and(|dest| FRAME_DESTINATIONS.contains(&dest))
}

fn fetch_dest(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("sec-fetch-dest")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
}

fn is_reserved_or_api_path(path: &str) -> bool {
    path == "/health"
        || path == "/ready"
        || path.starts_with("/.well-known")
        || API_PREFIXES
            .iter()
            .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

fn normalize_path(path: &str) -> String {
    if path.is_empty() || !path.starts_with('/') {
        format!("/{}", path.trim_start_matches('/'))
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use axum::http::HeaderValue;
    use edger_core::WorkerManifest;

    use super::*;

    fn index_with_shell(excludes: Vec<&str>) -> ManifestIndex {
        let mut index = ManifestIndex::new();
        index
            .insert(
                PathBuf::from("/workers/shell-demo"),
                WorkerManifest {
                    name: "shell-demo".into(),
                    version: Some("1.0.0".into()),
                    base: Some("/".into()),
                    shell_excludes: excludes.into_iter().map(str::to_string).collect(),
                    ..Default::default()
                },
            )
            .unwrap();
        index
    }

    fn headers(dest: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(dest) = dest {
            headers.insert("sec-fetch-dest", HeaderValue::from_str(dest).unwrap());
        }
        headers
    }

    #[test]
    fn document_navigation_routes_to_shell() {
        let shell = resolve_shell_worker(
            "GET",
            "/deployments/list",
            &headers(Some("document")),
            &index_with_shell(vec![]),
        )
        .unwrap();

        assert_eq!(shell.name, "shell-demo");
    }

    #[test]
    fn excluded_basename_bypasses_shell() {
        assert!(resolve_shell_worker(
            "GET",
            "/todos",
            &headers(Some("document")),
            &index_with_shell(vec!["todos"]),
        )
        .is_none());
    }

    #[test]
    fn iframe_embedding_bypasses_shell() {
        assert!(resolve_shell_worker(
            "GET",
            "/todos",
            &headers(Some("iframe")),
            &index_with_shell(vec![]),
        )
        .is_none());
    }

    #[test]
    fn reserved_paths_never_route_to_shell() {
        for path in [
            "/api/admin/session",
            "/health",
            "/ready",
            "/.well-known/openid",
        ] {
            assert!(
                resolve_shell_worker(
                    "GET",
                    path,
                    &headers(Some("document")),
                    &index_with_shell(vec![]),
                )
                .is_none(),
                "{path} should bypass shell"
            );
        }
    }

    #[test]
    fn mutating_methods_bypass_shell() {
        assert!(resolve_shell_worker(
            "POST",
            "/todos",
            &headers(Some("document")),
            &index_with_shell(vec![]),
        )
        .is_none());
    }
}
