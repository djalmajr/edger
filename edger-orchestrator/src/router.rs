//! Buntime-compatible path resolution (story 05.02).
//!
//! Mapping table (Buntime -> Rust):
//! - `/health`, `/ready` -> `Reserved`
//! - `/api`, `/.well-known/*` -> `Reserved`
//! - plugin `base` prefix -> `PluginBase` (longest match wins)
//! - `/name`, `/name@ver`, `/@scope/name`, `/@scope/name@ver` -> `Worker`
//! - `/` or unknown path -> `HomepageFallback` when configured

use edger_core::{CoreError, ExecutionKind, WorkerRef};

use crate::manifest_index_stub::ManifestIndex;

/// Reserved platform paths (no worker dispatch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReservedPath {
    Health,
    Ready,
    Api,
    WellKnown,
}

/// Plugin identity for base-path precedence routing.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginRef {
    pub name: String,
    pub base: String,
    pub dir: std::path::PathBuf,
    pub manifest: edger_core::WorkerManifest,
}

/// Result of path resolution before auth/pipeline dispatch.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedRoute {
    Worker {
        worker: WorkerRef,
        rewritten_path: String,
        kind_hint: ExecutionKind,
    },
    Reserved {
        kind: ReservedPath,
    },
    HomepageFallback {
        worker: WorkerRef,
    },
    PluginBase {
        plugin: PluginRef,
        remainder: String,
    },
}

/// Parsed worker address segment from the URL path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathParser {
    pub name: String,
    pub version: Option<String>,
    pub remainder: String,
}

impl PathParser {
    pub fn parse(path: &str) -> Result<Self, CoreError> {
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Err(CoreError::new("HOMEPAGE", "root path"));
        }

        let parts: Vec<&str> = normalized
            .trim_start_matches('/')
            .split('/')
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            return Err(CoreError::new("HOMEPAGE", "empty path"));
        }

        if parts[0].starts_with('@') {
            return parse_namespaced(&parts);
        }
        parse_unscoped(&parts)
    }
}

fn normalize_path(path: &str) -> String {
    if path.is_empty() || !path.starts_with('/') {
        format!("/{}", path.trim_start_matches('/'))
    } else {
        path.to_string()
    }
}

fn parse_namespaced(parts: &[&str]) -> Result<PathParser, CoreError> {
    if parts.len() < 2 {
        return Err(CoreError::parse("namespaced path requires @scope/name"));
    }
    let scope = parts[0];
    let (local, version) = split_name_version(parts[1]);
    let name = format!("{scope}/{local}");
    let remainder = join_remainder(&parts[2..]);
    Ok(PathParser {
        name,
        version,
        remainder,
    })
}

fn parse_unscoped(parts: &[&str]) -> Result<PathParser, CoreError> {
    let (local, version) = split_name_version(parts[0]);
    let remainder = join_remainder(&parts[1..]);
    Ok(PathParser {
        name: local,
        version,
        remainder,
    })
}

fn join_remainder(tail: &[&str]) -> String {
    if tail.is_empty() {
        "/".into()
    } else {
        format!("/{}", tail.join("/"))
    }
}

fn split_name_version(segment: &str) -> (String, Option<String>) {
    if let Some((name, ver)) = segment.rsplit_once('@') {
        if !ver.is_empty() && !name.is_empty() {
            return (name.to_string(), Some(ver.to_string()));
        }
    }
    (segment.to_string(), None)
}

fn reserved_kind(path: &str) -> Option<ReservedPath> {
    match path {
        "/health" => Some(ReservedPath::Health),
        "/ready" => Some(ReservedPath::Ready),
        p if p == "/api" || p.starts_with("/api/") => Some(ReservedPath::Api),
        p if p.starts_with("/.well-known") => Some(ReservedPath::WellKnown),
        _ => None,
    }
}

/// Resolve an HTTP path against manifests and Buntime routing rules.
pub fn resolve_route(
    path: &str,
    _base_href: Option<&str>,
    index: &ManifestIndex,
) -> Result<ResolvedRoute, CoreError> {
    let normalized = normalize_path(path);

    if let Some(kind) = reserved_kind(&normalized) {
        return Ok(ResolvedRoute::Reserved { kind });
    }

    if let Some((plugin, remainder)) = index.plugin_for_path(&normalized) {
        return Ok(ResolvedRoute::PluginBase { plugin, remainder });
    }

    match PathParser::parse(&normalized) {
        Ok(parsed) => {
            let worker = index.resolve_worker(&parsed.name, parsed.version.as_deref())?;
            Ok(ResolvedRoute::Worker {
                kind_hint: worker.kind.clone(),
                rewritten_path: parsed.remainder,
                worker,
            })
        }
        Err(err) if err.code == "HOMEPAGE" => match index.homepage() {
            Some(worker) => Ok(ResolvedRoute::HomepageFallback { worker }),
            None => Err(CoreError::new("NOT_FOUND", "no homepage configured")),
        },
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parser_namespaced_with_version() {
        let parsed = PathParser::parse("/@acme/app@1.0.0/foo").unwrap();
        assert_eq!(parsed.name, "@acme/app");
        assert_eq!(parsed.version.as_deref(), Some("1.0.0"));
        assert_eq!(parsed.remainder, "/foo");
    }
}