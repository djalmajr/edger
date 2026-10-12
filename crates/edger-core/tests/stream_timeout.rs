use edger_core::{parse_worker_config, WorkerManifest};

/// Canonical serde-saphyr options for parsing worker manifests (EDG-22),
/// mirroring the orchestrator's production parse.
fn yaml_options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins,
        with_snippet: false,
    }
}

fn config(manifest: &str) -> edger_core::WorkerConfig {
    let manifest =
        serde_saphyr::from_str_with_options::<WorkerManifest>(manifest, yaml_options()).unwrap();
    parse_worker_config(&manifest)
}

#[test]
fn stream_timeout_duration_is_normalized_to_milliseconds() {
    let config = config("name: example\nstreamTimeout: 30s\n");

    assert_eq!(config.stream_max_duration_ms, Some(30_000));
}

#[test]
fn stream_timeout_zero_disables_the_limit() {
    let config = config("name: example\nstreamTimeout: 0\n");

    assert_eq!(config.stream_max_duration_ms, Some(0));
}

#[test]
fn missing_stream_timeout_keeps_the_global_default_unset() {
    let config = config("name: example\n");

    assert_eq!(config.stream_max_duration_ms, None);
}

#[test]
fn invalid_stream_timeout_uses_the_same_fallback_as_invalid_ttl() {
    let config = config("name: example\nstreamTimeout: invalid\n");

    assert_eq!(config.stream_max_duration_ms, None);
}
