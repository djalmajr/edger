//! EDG-15: the opt-in `warmup` manifest field — parse and validation.
//!
//! `warmup` is an OBJECT with `path` (required) and `timeout` (optional
//! duration, same format as `healthCheck.timeout`): a bare string is a
//! manifest parse error, and a path without a leading `/` is rejected by
//! the manifest validation (the same error pattern the code already uses
//! for an invalid `basePath`).

use edger_core::{
    create_worker_ref, parse_worker_config, validate_worker_manifest, WorkerManifest,
    WorkerWarmupConfig,
};

fn manifest_yaml(body: &str) -> WorkerManifest {
    serde_yaml::from_str(body).expect("the manifest yaml parses")
}

// The valid object: `path` + `timeout: 5s` normalize to the manifest path
// and 5_000 ms.
#[test]
fn warmup_object_normalizes_path_and_timeout() {
    let manifest = manifest_yaml(
        r#"
name: warm-app
warmup:
  path: /
  timeout: 5s
"#,
    );
    validate_worker_manifest(&manifest).unwrap();
    let config = parse_worker_config(&manifest);
    let warmup = config.warmup.expect("the warmup config is present");
    assert_eq!(warmup.path, "/");
    assert_eq!(warmup.timeout_ms, 5_000, "5s normalizes to 5_000 ms");
}

// The default: without `timeout` the warmup budget is 10_000 ms (10 s).
#[test]
fn warmup_without_timeout_defaults_to_10_seconds() {
    let manifest = manifest_yaml(
        r#"
name: warm-app
warmup:
  path: /dashboard
"#,
    );
    validate_worker_manifest(&manifest).unwrap();
    let config = parse_worker_config(&manifest);
    let warmup = config.warmup.expect("the warmup config is present");
    assert_eq!(warmup.path, "/dashboard");
    assert_eq!(
        warmup.timeout_ms, 10_000,
        "absent timeout normalizes to 10_000 ms"
    );
}

// An unparseable `timeout` is not a validation error: the normalization
// falls back to the 10 s default (same rule as the default itself).
#[test]
fn warmup_unparseable_timeout_falls_back_to_the_default() {
    let manifest = manifest_yaml(
        r#"
name: warm-app
warmup:
  path: /
  timeout: soon
"#,
    );
    validate_worker_manifest(&manifest).unwrap();
    let config = parse_worker_config(&manifest);
    let warmup = config.warmup.expect("the warmup config is present");
    assert_eq!(
        warmup.timeout_ms, 10_000,
        "unparseable timeout normalizes to 10_000 ms"
    );
}

// `warmup: /` (a string instead of an object) is a manifest PARSE error.
#[test]
fn warmup_as_a_string_is_a_parse_error() {
    let result: Result<WorkerManifest, _> = serde_yaml::from_str("name: warm-app\nwarmup: /\n");
    assert!(
        result.is_err(),
        "a bare string where the warmup object is expected must not parse"
    );
}

// `warmup.path: dashboard` (no leading `/`) is rejected by the validation
// with the same error pattern used for an invalid `basePath`
// (`VALIDATION_ERROR`, `manifest.warmup.path: ...`).
#[test]
fn warmup_path_without_a_leading_slash_is_rejected() {
    let manifest = manifest_yaml(
        r#"
name: warm-app
warmup:
  path: dashboard
"#,
    );
    let err = validate_worker_manifest(&manifest).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
    assert!(
        err.message.contains("manifest.warmup.path"),
        "the error names the warmup path field: {err:?}"
    );
    // The full create_worker_ref path is rejected too.
    let ref_err =
        create_worker_ref(std::path::PathBuf::from("/workers/warm-app"), manifest).unwrap_err();
    assert_eq!(ref_err.code, "VALIDATION_ERROR");
}

// No `warmup` field: the config has no warmup (the behavior is untouched).
#[test]
fn manifest_without_warmup_has_none_in_the_config() {
    let manifest = manifest_yaml("name: plain-app\n");
    validate_worker_manifest(&manifest).unwrap();
    let config = parse_worker_config(&manifest);
    assert_eq!(config.warmup, None);
    // The re-exported normalized type round-trips through the manifest one.
    let manifest = WorkerManifest {
        name: "warm-app".into(),
        warmup: Some(edger_core::WorkerWarmup {
            path: "/".into(),
            timeout: None,
        }),
        ..Default::default()
    };
    validate_worker_manifest(&manifest).unwrap();
    let config = parse_worker_config(&manifest);
    assert_eq!(
        config.warmup,
        Some(WorkerWarmupConfig {
            path: "/".into(),
            timeout_ms: 10_000
        })
    );
}
