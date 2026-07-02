//! Routing resolution tests — Buntime path table (story 05.02).

use std::path::PathBuf;

use edger_core::WorkerManifest;
use edger_orchestrator::{
    resolve_host_route, resolve_route, ManifestIndex, ReservedPath, ResolvedRoute,
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
        .insert(PathBuf::from("/workers/home"), manifest("home", "1.0.0"))
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

fn host_manifest(name: &str, version: &str, hosts: Vec<&str>) -> WorkerManifest {
    WorkerManifest {
        name: name.into(),
        version: Some(version.into()),
        hosts: hosts.into_iter().map(String::from).collect(),
        ..Default::default()
    }
}

#[test]
fn reserved_health() {
    let route = resolve_route("/health", None, &ManifestIndex::new()).unwrap();
    assert_eq!(
        route,
        ResolvedRoute::Reserved {
            kind: ReservedPath::Health
        }
    );
}

#[test]
fn reserved_ready() {
    let route = resolve_route("/ready", None, &ManifestIndex::new()).unwrap();
    assert_eq!(
        route,
        ResolvedRoute::Reserved {
            kind: ReservedPath::Ready
        }
    );
}

#[test]
fn reserved_api_prefix() {
    let route = resolve_route("/api/v1/keys", None, &ManifestIndex::new()).unwrap();
    assert_eq!(
        route,
        ResolvedRoute::Reserved {
            kind: ReservedPath::Api
        }
    );
}

#[test]
fn reserved_well_known() {
    let route =
        resolve_route("/.well-known/acme-challenge/x", None, &ManifestIndex::new()).unwrap();
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
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            ..
        } => {
            assert_eq!(worker.version, "1.0.0");
            assert_eq!(rewritten_path, "/");
        }
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn namespaced_semver_range_picks_highest_matching_version() {
    let index = build_index();
    let route = resolve_route("/@acme/app@^1.0.0/foo", None, &index).unwrap();
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
fn known_host_resolves_to_configured_worker_before_path_routing() {
    let mut index = build_index();
    index
        .insert(
            PathBuf::from("/workers/hosted"),
            host_manifest("@acme/hosted", "2.0.0", vec!["App.Example.test:19080"]),
        )
        .unwrap();

    let route = resolve_host_route("/dashboard", Some("app.example.test:19080"), &index)
        .unwrap()
        .expect("host route");
    match route {
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            ..
        } => {
            assert_eq!(worker.name, "@acme/hosted");
            assert_eq!(worker.version, "2.0.0");
            assert_eq!(rewritten_path, "/dashboard");
        }
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn unknown_host_keeps_existing_path_fallback() {
    let index = build_index();
    let host_route = resolve_host_route("/hello", Some("unknown.example.test"), &index).unwrap();
    assert_eq!(host_route, None);

    let route = resolve_route("/hello", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker { worker, .. } => assert_eq!(worker.name, "hello"),
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn reserved_path_wins_over_known_host() {
    let mut index = build_index();
    index
        .insert(
            PathBuf::from("/workers/hosted"),
            host_manifest("hosted", "1.0.0", vec!["app.example.test"]),
        )
        .unwrap();

    let route = resolve_host_route("/api/admin/session", Some("app.example.test"), &index)
        .unwrap()
        .expect("reserved route");
    assert_eq!(
        route,
        ResolvedRoute::Reserved {
            kind: ReservedPath::Api
        }
    );
}

#[test]
fn disabled_host_worker_is_not_resolved_by_host_alias() {
    let mut index = build_index();
    index
        .insert(
            PathBuf::from("/workers/hosted"),
            host_manifest("hosted", "1.0.0", vec!["app.example.test"]),
        )
        .unwrap();
    index.set_worker_enabled("hosted", None, false).unwrap();

    let route = resolve_host_route("/", Some("app.example.test"), &index).unwrap();
    assert_eq!(route, None);
}

#[test]
fn unscoped_semver_range_picks_highest_matching_version() {
    let mut index = ManifestIndex::new();
    index
        .insert(PathBuf::from("/w/svc-1"), manifest("svc", "1.2.0"))
        .unwrap();
    index
        .insert(PathBuf::from("/w/svc-2"), manifest("svc", "1.4.0"))
        .unwrap();
    index
        .insert(PathBuf::from("/w/svc-3"), manifest("svc", "2.0.0"))
        .unwrap();

    let route = resolve_route("/svc@~1.2.0/ping", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker {
            worker,
            rewritten_path,
            ..
        } => {
            assert_eq!(worker.name, "svc");
            assert_eq!(worker.version, "1.2.0");
            assert_eq!(rewritten_path, "/ping");
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
fn duplicate_host_alias_is_a_collision() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            PathBuf::from("/w/a"),
            host_manifest("a", "1.0.0", vec!["app.example.test"]),
        )
        .unwrap();
    let err = index
        .insert(
            PathBuf::from("/w/b"),
            host_manifest("b", "1.0.0", vec!["APP.example.test."]),
        )
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
fn disabled_worker_is_removed_from_route_resolution_and_inventory() {
    let index = build_index();
    let disabled = index.set_worker_enabled("hello", None, false).unwrap();
    assert_eq!(disabled.name, "hello");
    assert_eq!(disabled.status, "disabled");

    let err = resolve_route("/hello", None, &index).unwrap_err();
    assert_eq!(err.code, "NOT_FOUND");
    let workers = index.admin_workers();
    let hello = workers
        .iter()
        .find(|worker| worker.name == "hello")
        .unwrap();
    assert_eq!(hello.status, "disabled");

    let enabled = index.set_worker_enabled("hello", None, true).unwrap();
    assert_eq!(enabled.status, "loaded");
    let route = resolve_route("/hello", None, &index).unwrap();
    match route {
        ResolvedRoute::Worker { worker, .. } => assert_eq!(worker.name, "hello"),
        other => panic!("expected worker, got {other:?}"),
    }
}

#[test]
fn unscoped_versioned_path() {
    let mut index = ManifestIndex::new();
    index
        .insert(PathBuf::from("/w/h"), manifest("svc", "3.1.0"))
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
