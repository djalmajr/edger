//! Manifest loader tests (story 07.01).

use std::fs;

use edger_core::{ExecutionKind, WorkerOrigin};
use edger_orchestrator::{
    load_manifests_from_dirs, load_manifests_from_roots, parse_runtime_worker_dirs, ManifestIndex,
};

fn static_worker(root: &std::path::Path, directory: &str, name: &str, version: &str) {
    let worker = root.join(directory);
    fs::create_dir_all(&worker).unwrap();
    fs::write(
        worker.join("manifest.yaml"),
        format!("name: {name}\nversion: \"{version}\"\nentrypoint: index.html\nkind: static\n"),
    )
    .unwrap();
    fs::write(worker.join("index.html"), name).unwrap();
}

#[test]
fn runtime_worker_dirs_parser_ignores_empty_segments() {
    let dirs = parse_runtime_worker_dirs("workers/a:: workers/b :");
    assert_eq!(dirs.len(), 2);
    assert_eq!(dirs[0].to_string_lossy(), "workers/a");
    assert_eq!(dirs[1].to_string_lossy(), "workers/b");
}

#[test]
fn index_resolves_single_latest_version() {
    let mut index = ManifestIndex::new();
    index
        .insert(
            "/workers/hello".into(),
            edger_core::WorkerManifest {
                name: "hello".into(),
                ..Default::default()
            },
        )
        .unwrap();

    let worker = index.resolve_worker("hello", None).unwrap();

    assert_eq!(worker.version, "latest");
}

#[test]
fn load_manifests_from_root_discovers_worker_dirs() {
    let root = tempfile::tempdir().unwrap();
    let fetch_dir = root.path().join("hello-world");
    let package_dir = root.path().join("serve");
    let wasm_dir = root.path().join("wasm-hello");
    let disabled_dir = root.path().join("disabled");

    fs::create_dir_all(&fetch_dir).unwrap();
    fs::write(
        fetch_dir.join("index.ts"),
        "Deno.serve(() => new Response('ok'));",
    )
    .unwrap();

    fs::create_dir_all(&package_dir).unwrap();
    fs::write(
        package_dir.join("package.json"),
        r#"{ "name": "serve", "version": "1.2.3", "module": "index.ts" }"#,
    )
    .unwrap();
    fs::write(
        package_dir.join("index.ts"),
        "export default { fetch() {} };",
    )
    .unwrap();

    fs::create_dir_all(&wasm_dir).unwrap();
    fs::write(
        wasm_dir.join("manifest.yaml"),
        r#"name: wasm-hello
version: "1.0.0"
entrypoint: index.wasm
kind: wasm
"#,
    )
    .unwrap();
    fs::write(wasm_dir.join("index.wasm"), b"\0asm\x01\0\0\0").unwrap();

    fs::create_dir_all(&disabled_dir).unwrap();
    fs::write(
        disabled_dir.join("manifest.yaml"),
        r#"name: disabled
enabled: false
entrypoint: index.ts
"#,
    )
    .unwrap();

    let index = load_manifests_from_dirs(&[root.path().to_path_buf()]).unwrap();

    let fetch_worker = index.resolve_worker("hello-world", None).unwrap();
    assert_eq!(fetch_worker.config.entrypoint.as_deref(), Some("index.ts"));
    assert_eq!(fetch_worker.kind, ExecutionKind::FetchHandler);

    let package_worker = index.resolve_worker("serve", Some("1.2.3")).unwrap();
    assert_eq!(
        package_worker.config.entrypoint.as_deref(),
        Some("index.ts")
    );

    let wasm_worker = index.resolve_worker("wasm-hello", None).unwrap();
    assert_eq!(
        wasm_worker.kind,
        ExecutionKind::WasmModule {
            entry: Some("index.wasm".into())
        }
    );

    let disabled = index.resolve_worker("disabled", None).unwrap_err();
    assert_eq!(disabled.code, "NOT_FOUND");
}

#[test]
fn manifestless_index_html_is_discovered_as_static_spa_before_js() {
    let root = tempfile::tempdir().unwrap();
    let spa_dir = root.path().join("landing");

    fs::create_dir_all(&spa_dir).unwrap();
    fs::write(
        spa_dir.join("index.html"),
        "<html><body>landing</body></html>",
    )
    .unwrap();
    fs::write(
        spa_dir.join("index.ts"),
        "Deno.serve(() => new Response('service'));",
    )
    .unwrap();

    let index = load_manifests_from_dirs(&[root.path().to_path_buf()]).unwrap();
    let worker = index.resolve_worker("landing", None).unwrap();

    assert_eq!(worker.config.entrypoint.as_deref(), Some("index.html"));
    assert_eq!(worker.kind, ExecutionKind::StaticSpa { inject_base: true });
}

#[test]
fn direct_worker_dir_with_only_index_html_is_discovered() {
    let root = tempfile::tempdir().unwrap();
    let spa_dir = root.path().join("direct-spa");

    fs::create_dir_all(&spa_dir).unwrap();
    fs::write(
        spa_dir.join("index.html"),
        "<html><body>direct</body></html>",
    )
    .unwrap();

    let index = load_manifests_from_dirs(&[spa_dir]).unwrap();
    let worker = index.resolve_worker("direct-spa", None).unwrap();

    assert_eq!(worker.config.entrypoint.as_deref(), Some("index.html"));
    assert_eq!(worker.kind, ExecutionKind::StaticSpa { inject_base: true });
}

#[test]
fn manifest_yaml_uses_package_json_name_when_omitted() {
    let root = tempfile::tempdir().unwrap();
    let todo_dir = root.path().join("todos");

    fs::create_dir_all(&todo_dir).unwrap();
    fs::write(
        todo_dir.join("manifest.yaml"),
        r#"entrypoint: index.html
injectBase: true
"#,
    )
    .unwrap();
    fs::write(
        todo_dir.join("package.json"),
        r#"{ "name": "todos", "version": "3.0.0" }"#,
    )
    .unwrap();
    fs::write(todo_dir.join("index.html"), "<html></html>").unwrap();

    let index = load_manifests_from_dirs(&[root.path().to_path_buf()]).unwrap();
    let worker = index.resolve_worker("todos", Some("3.0.0")).unwrap();

    assert_eq!(worker.name, "todos");
    assert_eq!(worker.version, "3.0.0");
    assert_eq!(worker.config.entrypoint.as_deref(), Some("index.html"));
    assert_eq!(worker.kind, ExecutionKind::StaticSpa { inject_base: true });
}

#[test]
fn load_manifests_merges_multiple_worker_roots() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let alpha = root_a.path().join("alpha");
    let beta = root_b.path().join("beta");

    fs::create_dir_all(&alpha).unwrap();
    fs::write(
        alpha.join("index.ts"),
        "Deno.serve(() => new Response('a'));",
    )
    .unwrap();

    fs::create_dir_all(&beta).unwrap();
    fs::write(
        beta.join("index.ts"),
        "Deno.serve(() => new Response('b'));",
    )
    .unwrap();

    let index =
        load_manifests_from_dirs(&[root_a.path().to_path_buf(), root_b.path().to_path_buf()])
            .unwrap();

    assert_eq!(index.resolve_worker("alpha", None).unwrap().name, "alpha");
    assert_eq!(index.resolve_worker("beta", None).unwrap().name, "beta");
}

#[test]
fn core_overlay_coexists_with_bundled_and_becomes_default_by_version() {
    let bundled = tempfile::tempdir().unwrap();
    let overlay = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    static_worker(bundled.path(), "cpanel", "cpanel", "1.0.0");
    static_worker(overlay.path(), "cpanel@2.0.0", "cpanel", "2.0.0");
    static_worker(bundled.path(), "webide", "webide", "1.0.0");
    static_worker(user.path(), "hello", "hello", "1.0.0");

    let index = load_manifests_from_roots(
        &[bundled.path().to_path_buf()],
        Some(&overlay.path().to_path_buf()),
        &[user.path().to_path_buf()],
    )
    .unwrap();

    assert_eq!(
        index.resolve_worker("cpanel", None).unwrap().version,
        "2.0.0"
    );
    let inventory = index.admin_workers();
    assert!(inventory.iter().any(|worker| {
        worker.name == "cpanel"
            && worker.version == "1.0.0"
            && worker.origin == WorkerOrigin::CoreBundled
    }));
    assert!(inventory.iter().any(|worker| {
        worker.name == "cpanel"
            && worker.version == "2.0.0"
            && worker.origin == WorkerOrigin::CoreOverlay
    }));
    assert!(inventory
        .iter()
        .any(|worker| { worker.name == "hello" && worker.origin == WorkerOrigin::User }));
}

#[test]
fn user_root_cannot_shadow_a_reserved_core_identity() {
    let bundled = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    static_worker(bundled.path(), "cpanel", "cpanel", "1.0.0");
    static_worker(user.path(), "shadow", "cpanel", "9.0.0");

    let error = load_manifests_from_roots(
        &[bundled.path().to_path_buf()],
        None,
        &[user.path().to_path_buf()],
    )
    .unwrap_err();

    assert_eq!(error.code, "CORE_NAME_RESERVED");
}
