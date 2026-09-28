//! Routing policy model, index, and persistence (story 25.01).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use edger_orchestrator::{
    clear_persisted_routing_policy, load_manifests_from_roots, parse_routing_policy,
    persist_routing_policy, rescan_workers,
};

fn static_worker(root: &Path, directory: &str, name: &str, version: &str) {
    let worker = root.join(directory);
    fs::create_dir_all(&worker).unwrap();
    fs::write(
        worker.join("manifest.yaml"),
        format!("name: \"{name}\"\nversion: \"{version}\"\nentrypoint: index.html\nkind: static\n"),
    )
    .unwrap();
    fs::write(worker.join("index.html"), name).unwrap();
}

fn load_user(root: &Path) -> edger_orchestrator::ManifestIndex {
    load_manifests_from_roots(&[], None, &[root.to_path_buf()]).unwrap()
}

fn policy_file_name(name: &str) -> String {
    let mut encoded = String::new();
    for byte in name.bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded.push_str(".json");
    encoded
}

fn example_policy(name: &str) -> String {
    format!(
        r#"{{"name":"{name}","tenantAccess":{{"mode":"allowlist","tenants":["acme"]}},"traffic":{{"versions":[{{"version":"1.0.0","weight":80}},{{"version":"2.0.0","weight":20}}]}}}}"#
    )
}

#[test]
fn parses_the_routing_policy_example() {
    let policy = parse_routing_policy(example_policy("app").as_bytes()).unwrap();
    assert_eq!(policy.name, "app");
    let edger_orchestrator::TenantAccess::Allowlist { tenants } = &policy.tenant_access else {
        panic!("allowlist mode");
    };
    assert_eq!(tenants, &vec!["acme".to_string()]);
    let traffic = policy.traffic.as_ref().unwrap();
    assert_eq!(traffic.versions.len(), 2);
    assert_eq!(traffic.versions[0].version, "1.0.0");
    assert_eq!(traffic.versions[0].weight, 80);
    assert_eq!(traffic.versions[1].version, "2.0.0");
    assert_eq!(traffic.versions[1].weight, 20);
}

#[test]
fn public_is_not_an_empty_allowlist() {
    let empty =
        parse_routing_policy(br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":[]}}"#)
            .unwrap_err();
    assert_eq!(empty.code, "VALIDATION_ERROR");
    assert!(empty.message.contains("allowlist"), "{}", empty.message);
    assert!(empty.message.contains("public"), "{}", empty.message);

    let public_with_tenants = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"public","tenants":[]},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap_err();
    assert_eq!(public_with_tenants.code, "VALIDATION_ERROR");

    let public_without_traffic =
        parse_routing_policy(br#"{"name":"app","tenantAccess":{"mode":"public"}}"#).unwrap_err();
    assert_eq!(public_without_traffic.code, "VALIDATION_ERROR");
    assert!(
        public_without_traffic.message.contains("public"),
        "{}",
        public_without_traffic.message
    );

    let open_split = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"public"},"traffic":{"versions":[{"version":"1.0.0","weight":100}]}}"#,
    )
    .unwrap();
    assert!(matches!(
        open_split.tenant_access,
        edger_orchestrator::TenantAccess::Public
    ));

    let allowlist_only = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap();
    assert!(allowlist_only.traffic.is_none());
    assert_ne!(open_split.tenant_access, allowlist_only.tenant_access);
}

#[test]
fn rejects_unknown_fields_duplicate_keys_and_bad_json() {
    let unknown = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"extra":true}"#,
    )
    .unwrap_err();
    assert_eq!(unknown.code, "VALIDATION_ERROR");

    let duplicate = parse_routing_policy(
        br#"{"name":"app","name":"other","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap_err();
    assert_eq!(duplicate.code, "VALIDATION_ERROR");
    assert!(
        duplicate.message.contains("duplicate"),
        "{}",
        duplicate.message
    );

    let escaped_duplicate = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"\u006eame":"other"}"#,
    )
    .unwrap_err();
    assert_eq!(escaped_duplicate.code, "VALIDATION_ERROR");
    assert!(
        escaped_duplicate.message.contains("duplicate"),
        "{}",
        escaped_duplicate.message
    );

    let nested = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","version":"2.0.0","weight":100}]}}"#,
    )
    .unwrap_err();
    assert_eq!(nested.code, "VALIDATION_ERROR");
    assert!(nested.message.contains("duplicate"), "{}", nested.message);

    let syntax = parse_routing_policy(b"{").unwrap_err();
    assert_eq!(syntax.code, "PARSE_ERROR");
}

#[test]
fn rejects_bad_slugs_weights_and_duplicates() {
    let cases = [
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["Acme"]}}"#,
            "slug",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme-"]}}"#,
            "slug",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["-acme"]}}"#,
            "slug",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["a--b"]}}"#,
            "slug",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme","acme"]}}"#,
            "duplicate",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","weight":0},{"version":"2.0.0","weight":100}]}}"#,
            "weight",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","weight":101}]}}"#,
            "weight",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","weight":-1}]}}"#,
            "weight",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","weight":80.5}]}}"#,
            "weight",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","weight":40},{"version":"2.0.0","weight":50}]}}"#,
            "100",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[{"version":"1.0.0","weight":50},{"version":"1.0.0","weight":50}]}}"#,
            "duplicate",
        ),
        (
            r#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]},"traffic":{"versions":[]}}"#,
            "8",
        ),
    ];
    for (json, needle) in cases {
        let err = parse_routing_policy(json.as_bytes()).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR", "{json} -> {err}");
        assert!(
            err.message.to_ascii_lowercase().contains(needle),
            "{json} -> {err}"
        );
    }

    let slug_63 = "a".repeat(63);
    parse_routing_policy(
        format!(
            r#"{{"name":"app","tenantAccess":{{"mode":"allowlist","tenants":["{slug_63}"]}}}}"#
        )
        .as_bytes(),
    )
    .unwrap();
    let slug_64 = "b".repeat(64);
    let too_long = parse_routing_policy(
        format!(
            r#"{{"name":"app","tenantAccess":{{"mode":"allowlist","tenants":["{slug_64}"]}}}}"#
        )
        .as_bytes(),
    )
    .unwrap_err();
    assert_eq!(too_long.code, "VALIDATION_ERROR");

    let mut versions = Vec::new();
    for index in 0..9 {
        let weight = if index == 0 { 92 } else { 1 };
        versions.push(format!(r#"{{"version":"0.0.{index}","weight":{weight}}}"#));
    }
    let nine = parse_routing_policy(
        format!(
            r#"{{"name":"app","tenantAccess":{{"mode":"allowlist","tenants":["acme"]}},"traffic":{{"versions":[{}]}}}}"#,
            versions.join(",")
        )
        .as_bytes(),
    )
    .unwrap_err();
    assert_eq!(nine.code, "VALIDATION_ERROR");
    assert!(nine.message.contains('8'), "{}", nine.message);
}

#[test]
fn round_trip_survives_reload_without_changing_default_version() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    static_worker(root.path(), "app@2.0.0", "app", "2.0.0");
    let pointer_dir = root.path().join(".edger-defaults");
    fs::create_dir_all(&pointer_dir).unwrap();
    fs::write(
        pointer_dir.join(policy_file_name("app")),
        "{\"name\":\"app\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();

    let index = load_user(root.path());
    assert_eq!(index.default_version("app").as_deref(), Some("1.0.0"));
    assert_eq!(index.resolve_worker("app", None).unwrap().version, "1.0.0");
    assert!(index.routing_policy("app").unwrap().is_none());

    let policy = parse_routing_policy(example_policy("app").as_bytes()).unwrap();
    persist_routing_policy(&index, &policy).unwrap();
    assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));
    assert_eq!(index.default_version("app").as_deref(), Some("1.0.0"));
    assert_eq!(index.resolve_worker("app", None).unwrap().version, "1.0.0");
    index.validate_routing_policy(&policy).unwrap();

    let reloaded = load_user(root.path());
    assert_eq!(
        reloaded.routing_policy("app").unwrap().as_ref(),
        Some(&policy)
    );
    assert_eq!(reloaded.default_version("app").as_deref(), Some("1.0.0"));
    assert_eq!(
        reloaded.resolve_worker("app", None).unwrap().version,
        "1.0.0"
    );
    let stored = fs::read(
        root.path()
            .join(".edger-routing")
            .join(policy_file_name("app")),
    )
    .unwrap();
    assert_eq!(parse_routing_policy(&stored).unwrap(), policy);
}

#[test]
fn failed_persist_keeps_the_previous_policy() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    static_worker(root.path(), "app@2.0.0", "app", "2.0.0");
    let index = load_user(root.path());
    let original = parse_routing_policy(example_policy("app").as_bytes()).unwrap();
    persist_routing_policy(&index, &original).unwrap();
    let path = root
        .path()
        .join(".edger-routing")
        .join(policy_file_name("app"));
    let before = fs::read(&path).unwrap();

    let replacement = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["other"]},"traffic":{"versions":[{"version":"1.0.0","weight":80},{"version":"2.0.0","weight":20}]}}"#,
    )
    .unwrap();
    let directory = path.parent().unwrap();
    let mut permissions = fs::metadata(directory).unwrap().permissions();
    permissions.set_mode(0o555);
    fs::set_permissions(directory, permissions).unwrap();

    let err = persist_routing_policy(&index, &replacement).unwrap_err();
    assert_eq!(err.code, "DEPLOY_IO", "{err}");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        index.routing_policy("app").unwrap().as_ref(),
        Some(&original)
    );

    let mut permissions = fs::metadata(directory).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(directory, permissions).unwrap();
}

#[test]
fn namespaced_name_is_stored_as_hex_inside_the_routing_directory() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "scope-app-1", "@scope/name", "1.0.0");
    static_worker(root.path(), "scope-app-2", "@scope/name", "2.0.0");
    let index = load_user(root.path());
    let policy = parse_routing_policy(example_policy("@scope/name").as_bytes()).unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    let routing = root.path().join(".edger-routing");
    let mut names = fs::read_dir(&routing)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec![policy_file_name("@scope/name")]);
    let file_name = &names[0];
    assert!(!file_name.contains('/'));
    assert!(!file_name.contains(".."));
    let stem = file_name.strip_suffix(".json").unwrap();
    assert!(stem
        .chars()
        .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));
    let bytes = (0..stem.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&stem[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(String::from_utf8(bytes).unwrap(), "@scope/name");
    assert!(routing.join(file_name).starts_with(&routing));

    clear_persisted_routing_policy(&index, "@scope/name").unwrap();
    assert!(index.routing_policy("@scope/name").unwrap().is_none());
    assert!(!routing.join(file_name).exists());
    let reloaded = load_user(root.path());
    assert!(reloaded.routing_policy("@scope/name").unwrap().is_none());
    assert_eq!(
        reloaded
            .resolve_worker("@scope/name", None)
            .unwrap()
            .version,
        "2.0.0"
    );
}

#[test]
fn rejects_disabled_staged_and_internal_versions_without_writing() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    static_worker(root.path(), "app@2.0.0", "app", "2.0.0");
    fs::write(
        root.path().join("app@2.0.0").join(".edger-revision"),
        "rev-1\nstaged=true\n",
    )
    .unwrap();
    let index = load_user(root.path());
    let policy = parse_routing_policy(example_policy("app").as_bytes()).unwrap();
    let staged = persist_routing_policy(&index, &policy).unwrap_err();
    assert_eq!(staged.code, "PROMOTE_STAGED_MARKER_PRESENT", "{staged}");
    assert!(index.routing_policy("app").unwrap().is_none());
    assert!(!root.path().join(".edger-routing").exists());

    let internal_root = tempfile::tempdir().unwrap();
    static_worker(internal_root.path(), "app@1.0.0", "app", "1.0.0");
    let internal_dir = internal_root.path().join("app@2.0.0");
    fs::create_dir_all(&internal_dir).unwrap();
    fs::write(
        internal_dir.join("manifest.yaml"),
        "name: app\nversion: \"2.0.0\"\nentrypoint: index.html\nkind: static\nvisibility: internal\n",
    )
    .unwrap();
    fs::write(internal_dir.join("index.html"), "app").unwrap();
    let internal_index = load_user(internal_root.path());
    let internal = persist_routing_policy(&internal_index, &policy).unwrap_err();
    assert_eq!(internal.code, "PROMOTE_INTERNAL_VERSION", "{internal}");
    assert!(internal_index.routing_policy("app").unwrap().is_none());

    let disabled_root = tempfile::tempdir().unwrap();
    static_worker(disabled_root.path(), "app@1.0.0", "app", "1.0.0");
    static_worker(disabled_root.path(), "app@2.0.0", "app", "2.0.0");
    let disabled_index = load_user(disabled_root.path());
    disabled_index
        .set_worker_enabled("app", Some("2.0.0"), false)
        .unwrap();
    let disabled = persist_routing_policy(&disabled_index, &policy).unwrap_err();
    assert_eq!(disabled.code, "VALIDATION_ERROR", "{disabled}");
    assert!(
        disabled.message.contains("disabled"),
        "{}",
        disabled.message
    );
    assert!(disabled_index.routing_policy("app").unwrap().is_none());
    assert!(!disabled_root.path().join(".edger-routing").exists());
}

#[test]
fn rejects_core_and_reserved_names() {
    let bundled = tempfile::tempdir().unwrap();
    let overlay = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    static_worker(bundled.path(), "cpanel", "cpanel", "1.0.0");
    static_worker(bundled.path(), "webide", "webide", "1.0.0");
    let index = load_manifests_from_roots(
        &[bundled.path().to_path_buf()],
        Some(&overlay.path().to_path_buf()),
        &[user.path().to_path_buf()],
    )
    .unwrap();
    for name in ["cpanel", "webide"] {
        let policy = parse_routing_policy(
            format!(
                r#"{{"name":"{name}","tenantAccess":{{"mode":"allowlist","tenants":["acme"]}}}}"#
            )
            .as_bytes(),
        )
        .unwrap();
        let err = persist_routing_policy(&index, &policy).unwrap_err();
        assert_eq!(err.code, "CORE_NAME_RESERVED", "{name}: {err}");
    }
    assert!(!user.path().join(".edger-routing").exists());
    assert!(!overlay.path().join(".edger-routing").exists());
    assert!(!bundled.path().join(".edger-routing").exists());
}

#[test]
fn delete_failure_on_a_later_root_keeps_the_allowlist() {
    let base = tempfile::tempdir().unwrap();
    let user = base.path().join("a-user");
    let overlay = base.path().join("z-overlay");
    fs::create_dir_all(&user).unwrap();
    fs::create_dir_all(&overlay).unwrap();
    static_worker(&user, "app@1.0.0", "app", "1.0.0");
    let index =
        load_manifests_from_roots(&[], Some(&overlay), std::slice::from_ref(&user)).unwrap();
    let policy = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    let file_name = policy_file_name("app");
    let user_file = user.join(".edger-routing").join(&file_name);
    let before = fs::read(&user_file).unwrap();
    let overlay_dir = overlay.join(".edger-routing");
    fs::create_dir_all(&overlay_dir).unwrap();
    fs::write(overlay_dir.join(&file_name), &before).unwrap();
    let mut permissions = fs::metadata(&overlay_dir).unwrap().permissions();
    permissions.set_mode(0o555);
    fs::set_permissions(&overlay_dir, permissions).unwrap();

    let err = clear_persisted_routing_policy(&index, "app").unwrap_err();

    let mut permissions = fs::metadata(&overlay_dir).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&overlay_dir, permissions).unwrap();

    assert_eq!(err.code, "DEPLOY_IO", "{err}");
    assert_eq!(fs::read(&user_file).unwrap(), before);
    assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));
}

#[test]
fn rescan_keeps_the_allowlist_when_the_policy_directory_is_invalid() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    let index = load_user(root.path());
    let policy = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();
    fs::write(
        root.path().join(".edger-routing").join("notes.json"),
        b"not a policy",
    )
    .unwrap();

    let err = rescan_workers(&index, false).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR", "{err}");
    assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));
}

#[test]
fn removed_version_stays_named_and_does_not_fall_back_to_another() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    static_worker(root.path(), "app@2.0.0", "app", "2.0.0");
    let index = load_user(root.path());
    let policy = parse_routing_policy(example_policy("app").as_bytes()).unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    fs::remove_dir_all(root.path().join("app@2.0.0")).unwrap();
    rescan_workers(&index, false).unwrap();
    assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));
    let missing = index.validate_routing_policy(&policy).unwrap_err();
    assert_eq!(missing.code, "NOT_FOUND", "{missing}");
    assert!(index.default_version("app").is_none());
    // The pre-policy resolver still serves the remaining version. The policy
    // document is not rewritten to move the removed weight onto it.
    assert_eq!(index.resolve_worker("app", None).unwrap().version, "1.0.0");
    let reloaded = load_user(root.path());
    assert_eq!(
        reloaded.routing_policy("app").unwrap().as_ref(),
        Some(&policy)
    );
    assert_eq!(
        reloaded.validate_routing_policy(&policy).unwrap_err().code,
        "NOT_FOUND"
    );

    static_worker(root.path(), "app@3.0.0", "app", "3.0.0");
    rescan_workers(&index, false).unwrap();
    let stored = index.routing_policy("app").unwrap().unwrap();
    assert_eq!(stored, policy);
    assert!(stored
        .traffic
        .unwrap()
        .versions
        .iter()
        .all(|version| version.version != "3.0.0"));
    assert_eq!(
        index.validate_routing_policy(&policy).unwrap_err().code,
        "NOT_FOUND"
    );

    static_worker(root.path(), "app@2.0.0", "app", "2.0.0");
    rescan_workers(&index, false).unwrap();
    index.validate_routing_policy(&policy).unwrap();
    assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));
}

#[test]
fn invalid_routing_document_fails_boot() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    let routing = root.path().join(".edger-routing");
    fs::create_dir_all(&routing).unwrap();
    fs::write(routing.join(policy_file_name("app")), b"{").unwrap();
    let err = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap_err();
    assert_eq!(err.code, "PARSE_ERROR", "{err}");

    fs::write(
        routing.join(policy_file_name("app")),
        br#"{"name":"other","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap();
    let mismatch = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap_err();
    assert_eq!(mismatch.code, "VALIDATION_ERROR", "{mismatch}");

    fs::remove_file(routing.join(policy_file_name("app"))).unwrap();
    fs::write(routing.join("notes.json"), b"not a policy").unwrap();
    let unexpected =
        load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap_err();
    assert_eq!(unexpected.code, "VALIDATION_ERROR", "{unexpected}");

    fs::remove_file(routing.join("notes.json")).unwrap();
    fs::write(routing.join(".publish.tmp"), b"{").unwrap();
    let index = load_user(root.path());
    assert!(index.routing_policy("app").unwrap().is_none());
    assert!(index.resolve_worker("app", None).is_ok());
}

#[test]
fn boot_rejects_a_routing_symlink_without_reading_the_target() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    let routing = root.path().join(".edger-routing");
    fs::create_dir_all(&routing).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", routing.join(policy_file_name("app"))).unwrap();
    let err = load_manifests_from_roots(&[], None, &[root.path().to_path_buf()]).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR", "{err}");
    assert!(err.message.contains("symlink"), "{}", err.message);
    assert!(!err.message.contains("root:"), "{}", err.message);
}

#[test]
fn same_policy_name_in_two_roots_fails_boot() {
    let overlay = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    static_worker(user.path(), "app@1.0.0", "app", "1.0.0");
    let body = br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#;
    for root in [overlay.path(), user.path()] {
        let routing = root.join(".edger-routing");
        fs::create_dir_all(&routing).unwrap();
        fs::write(routing.join(policy_file_name("app")), body).unwrap();
    }
    let err = load_manifests_from_roots(
        &[],
        Some(&overlay.path().to_path_buf()),
        &[user.path().to_path_buf()],
    )
    .unwrap_err();
    assert_eq!(err.code, "COLLISION", "{err}");
}

#[test]
fn persist_rejects_when_routing_directory_becomes_symlink_after_boot() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    let index = load_user(root.path());
    let original = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &original).unwrap();

    let routing = root.path().join(".edger-routing");
    let file_name = policy_file_name("app");
    let before = fs::read(routing.join(&file_name)).unwrap();

    // Swap the directory for a symlink after boot, pointing outside.
    let real = root.path().join(".edger-routing-real");
    fs::rename(&routing, &real).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), &routing).unwrap();

    let updated = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["beta"]}}"#,
    )
    .unwrap();
    let err = persist_routing_policy(&index, &updated).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR", "{err}");
    assert!(err.message.contains("symlink"), "{}", err.message);
    // Nothing escaped through the symlink.
    assert!(!outside.path().join(&file_name).exists());
    // Memory and the real directory are untouched.
    assert_eq!(
        index.routing_policy("app").unwrap().as_ref(),
        Some(&original)
    );
    assert_eq!(fs::read(real.join(&file_name)).unwrap(), before);

    // Restore so teardown sees a normal tree.
    fs::remove_file(&routing).unwrap();
    fs::rename(&real, &routing).unwrap();
}

#[test]
fn clear_rejects_when_routing_directory_becomes_symlink_after_boot() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    let index = load_user(root.path());
    let policy = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    let routing = root.path().join(".edger-routing");
    let file_name = policy_file_name("app");
    let before = fs::read(routing.join(&file_name)).unwrap();

    // Swap the directory for a symlink after boot, staging a copy outside
    // so a delete through the link would visibly escape.
    let real = root.path().join(".edger-routing-real");
    fs::rename(&routing, &real).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join(&file_name), &before).unwrap();
    std::os::unix::fs::symlink(outside.path(), &routing).unwrap();

    let err = clear_persisted_routing_policy(&index, "app").unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR", "{err}");
    assert!(err.message.contains("symlink"), "{}", err.message);
    // The file outside was not touched and the allowlist stayed.
    assert_eq!(fs::read(outside.path().join(&file_name)).unwrap(), before);
    assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));

    fs::remove_file(&routing).unwrap();
    fs::rename(&real, &routing).unwrap();
    // Normal delete still works once the real directory is back.
    clear_persisted_routing_policy(&index, "app").unwrap();
    assert!(index.routing_policy("app").unwrap().is_none());
}

#[test]
fn rescan_rejects_when_routing_directory_becomes_symlink_after_boot() {
    let root = tempfile::tempdir().unwrap();
    static_worker(root.path(), "app@1.0.0", "app", "1.0.0");
    let index = load_user(root.path());
    let policy = parse_routing_policy(
        br#"{"name":"app","tenantAccess":{"mode":"allowlist","tenants":["acme"]}}"#,
    )
    .unwrap();
    persist_routing_policy(&index, &policy).unwrap();

    let routing = root.path().join(".edger-routing");
    let real = root.path().join(".edger-routing-real");
    fs::rename(&routing, &real).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), &routing).unwrap();

    let err = rescan_workers(&index, false).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR", "{err}");
    assert!(err.message.contains("symlink"), "{}", err.message);
    assert_eq!(index.routing_policy("app").unwrap().as_ref(), Some(&policy));

    fs::remove_file(&routing).unwrap();
    fs::rename(&real, &routing).unwrap();
}
