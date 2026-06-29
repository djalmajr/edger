//! Routing resolution tests — Buntime path table (story 05.02).

use std::path::PathBuf;

use edger_core::WorkerManifest;
use edger_orchestrator::{
    resolve_route, ManifestIndex, ReservedPath, ResolvedRoute,
};

fn manifest(name: &str, version: &str) -> WorkerManifest {
    WorkerManifest {
        name: name.into(),
        version: Some(version.into()),
        ..Default::default()
    }
}

fn build_index() -> ManifestIndex {
    let mut index = ManifestIndex::new();
    index
        .insert(PathBuf::from("/workers/hello"), manifest("hello", "1.0.0"))
        .unwrap();
    index
        .insert(
            PathBuf::from("/workers/acme-app-1"),
            manifest("@acme/app", "1.0.0"),
        )
        .unwrap();
    index
        .insert(
            PathBuf::from("/workers/acme-app-2"),
            manifest("@acme/app", "2.0.0"),
        )
        .unwrap();
    index
        .insert(
            PathBuf::from("/workers/acme-api"),
            manifest("@acme/api", "1.0.0"),
        )
        .unwrap();
    index
        .insert(
            PathBuf::from("/workers/gateway-worker"),
            manifest("gateway", "1.0.0"),
        )
        .unwrap();
    index
        .insert(
            PathBuf::from("/workers/home"),
            manifest("home", "1.0.0"),
        )
        .unwrap();

    let home = index.resolve_worker("home", None).unwrap();
    index.with_homepage(home)
}

fn plugin_manifest(name: &str, base: &str) -> WorkerManifest {
    WorkerManifest {
        name: name.into(),
        version: Some("1.0.0".into()),
        base: Some(base.into()),
        ..Default::default()
    }
}

#[test]
fn reserved_health() {
    let route = resolve_route("/health", None, &ManifestIndex::new()).unwrap();
    assert_eq!(route, ResolvedRoute::Reserved { kind: ReservedPath::Health });
}

#[test]
fn reserved_ready() {
    let route = resolve_route("/ready", None, &ManifestIndex::new()).unwrap();
    assert_eq!(route, ResolvedRoute::Reserved { kind: ReservedPath::Ready });
}

#[test]
fn reserved_api_prefix() {
    let route = resolve_route("/api/v1/keys", None, &ManifestIndex::new()).unwrap();
    assert_eq!(route, ResolvedRoute::Reserved { kind: ReservedPath::Api });
}

#[test]
fn reserved_well_known() {
    let route = resolve_route("/.well-known/acme-challenge/x", None, &ManifestIndex::new()).unwrap();
    assert_eq!(
        route,
        ResolvedRoute::Reserved {
            kind: ReservedPath::WellKnown
        }
    );
}

#[test]
fn unscoped_worker_root() {
    let index = build_index();
    let route = resolve_route("/hello", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            ..
        } => {
            assert_eq!(worker.name, "hello");
            assert_eq!(rewritten_path, "/");
        }
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn unscoped_worker_with_subpath() {
    let index = build_index();
    let route = resolve_route("/hello/world", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker { rewritten_path, .. } => assert_eq!(rewritten_path, "/world"),
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn namespaced_latest_version() {
    let index = build_index();
    let route = resolve_route("/@acme/app", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker { worker, .. } => assert_eq!(worker.version, "2.0.0"),
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn namespaced_exact_semver() {
    let index = build_index();
    let route = resolve_route("/@acme/app@1.0.0", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker { worker, rewritten_path, .. } => {
            assert_eq!(worker.version, "1.0.0");
            assert_eq!(rewritten_path, "/");
        }
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn namespaced_semver_with_rewrite() {
    let index = build_index();
    let route = resolve_route("/@acme/app@1.0.0/foo", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            ..
        } => {
            assert_eq!(worker.name, "@acme/app");
            assert_eq!(worker.version, "1.0.0");
            assert_eq!(rewritten_path, "/foo");
        }
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn namespaced_subpath_without_explicit_version() {
    let index = build_index();
    let route = resolve_route("/@acme/api/foo", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            ..
        } => {
            assert_eq!(worker.name, "@acme/api");
            assert_eq!(rewritten_path, "/foo");
        }
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn missing_version_returns_not_found() {
    let index = build_index();
    let err = resolve_route("/@acme/app@9.9.9", None, &index).unwrap_err();
    assert_eq!(err.code, "NOT_FOUND");
}

#[test]
fn plugin_base_takes_precedence_over_worker() {
    let mut index = build_index();
    index
        .insert(
            PathBuf::from("/plugins/gateway"),
            plugin_manifest("gateway-plugin", "/gateway"),
        )
        .unwrap();

    let route = resolve_route("/gateway/hook", None, &index).unwrap();
    match route {
        ResolvedRoute::PluginBase { plugin, remainder } => {
            assert_eq!(plugin.name, "gateway-plugin");
            assert_eq!(remainder, "hook");
        }
        other => panic!("expected plugin base, got {other:?}"),
    }
}

#[test]
fn longer_plugin_base_wins() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/plugins/short"),
            plugin_manifest("short", "/gw"),
        )
        .unwrap();
    index
        .insert(
            PathBuf::from("/plugins/long"),
            plugin_manifest("long", "/gw/deep"),
        )
        .unwrap();

    let route = resolve_route("/gw/deep/x", None, &index).unwrap();
    match route {
        ResolvedRoute::PluginBase { plugin, .. } => assert_eq!(plugin.name, "long"),
        other => panic!("expected plugin, got {other:?}"),
    }
}

#[test]
fn homepage_fallback_for_root() {
    let index = build_index();
    let route = resolve_route("/", None, &index).unwrap();
    match route {
        ResolvedRoute::HomepageFallback { worker } => assert_eq!(worker.name, "home"),
        other => panic!("expected homepage, got {other:?}"),
    }
}

#[test]
fn collision_on_duplicate_insert() {
    let mut index = ManifestIndex::new();
    index
        .insert(PathBuf::from("/w/a"), manifest("dup", "1.0.0"))
        .unwrap();
    let err = index
        .insert(PathBuf::from("/w/b"), manifest("dup", "1.0.0"))
        .unwrap_err();
    assert_eq!(err.code, "COLLISION");
}

#[test]
fn unknown_worker_returns_not_found() {
    let index = build_index();
    let err = resolve_route("/nope", None, &index).unwrap_err();
    assert_eq!(err.code, "NOT_FOUND");
}

#[test]
fn unscoped_versioned_path() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/w/h"),
            manifest("svc", "3.1.0"),
        )
        .unwrap();
    let route = resolve_route("/svc@3.1.0/ping", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            ..
        } => {
            assert_eq!(worker.version, "3.1.0");
            assert_eq!(rewritten_path, "/ping");
        }
        other => panic!("expected worker, got {other:?}"),
    }
}