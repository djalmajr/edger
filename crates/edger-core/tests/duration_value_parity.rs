//! EDG-22: durable edge-case pins for the serde-saphyr manifest parse.
//!
//! Every behavior here was measured against `serde_yaml` 0.9 (the previous
//! parser) before the swap; the assertions pin what a manifest means in
//! EdgeR after the swap. The options mirror the orchestrator's production
//! parse (`manifest_loader::manifest_yaml_options`).

use edger_core::{
    parse_duration_to_ms, parse_worker_config, WorkerManifest, DEFAULT_MAX_BODY_BYTES,
};
use serde_json::json;

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

fn parse(text: &str) -> Result<WorkerManifest, serde_saphyr::Error> {
    serde_saphyr::from_str_with_options(text, yaml_options())
}

fn parse_ttl(text: &str) -> (WorkerManifest, serde_json::Value) {
    let manifest = parse(text).unwrap_or_else(|e| panic!("manifest must parse: {text:?}: {e}"));
    let json = serde_json::to_value(&manifest).unwrap();
    (manifest, json)
}

fn ttl_ms(manifest: &WorkerManifest) -> Option<u64> {
    manifest.ttl.as_ref().and_then(parse_duration_to_ms)
}

// --- Duration field forms: every form the previous parser accepted is
// accepted at parse time; only non-negative integer seconds and duration
// strings normalize, everything else yields None (the field's default).

#[test]
fn duration_ttl_integer_seconds_is_normalized() {
    let (m, json) = parse_ttl("name: example\nttl: 30\n");
    assert_eq!(json["ttl"], json!(30), "JSON keeps the raw integer");
    assert_eq!(ttl_ms(&m), Some(30_000));
    assert_eq!(parse_worker_config(&m).ttl_ms, 30_000);
}

#[test]
fn duration_ttl_quoted_and_bare_duration_strings_are_normalized() {
    for yaml in ["name: example\nttl: \"30s\"\n", "name: example\nttl: 30s\n"] {
        let (m, json) = parse_ttl(yaml);
        assert_eq!(
            json["ttl"],
            json!("30s"),
            "JSON keeps the raw string ({yaml:?})"
        );
        assert_eq!(ttl_ms(&m), Some(30_000));
    }
}

#[test]
fn duration_ttl_zero_disables_the_limit() {
    let (m, json) = parse_ttl("name: example\nttl: 0\n");
    assert_eq!(json["ttl"], json!(0));
    assert_eq!(ttl_ms(&m), Some(0));
    assert_eq!(parse_worker_config(&m).ttl_ms, 0);
}

#[test]
fn duration_ttl_float_is_accepted_and_ignored() {
    let (m, json) = parse_ttl("name: example\nttl: 1.5\n");
    assert_eq!(json["ttl"], json!(1.5), "JSON keeps the raw float");
    assert_eq!(
        ttl_ms(&m),
        None,
        "a float must not be a parse error, and must not normalize"
    );
}

#[test]
fn duration_ttl_negative_number_is_accepted_and_ignored() {
    let (m, json) = parse_ttl("name: example\nttl: -1\n");
    assert_eq!(json["ttl"], json!(-1));
    assert_eq!(ttl_ms(&m), None);
}

#[test]
fn duration_ttl_bool_is_accepted_and_ignored() {
    let (m, json) = parse_ttl("name: example\nttl: true\n");
    assert_eq!(json["ttl"], json!(true));
    assert_eq!(ttl_ms(&m), None);
}

#[test]
fn duration_ttl_null_is_none() {
    let (m, json) = parse_ttl("name: example\nttl: ~\n");
    assert_eq!(json["ttl"], json!(null));
    assert!(m.ttl.is_none(), "an explicit null is the absent field");
    assert_eq!(ttl_ms(&m), None);
}

#[test]
fn duration_ttl_sequence_is_accepted_and_ignored() {
    let (m, json) = parse_ttl("name: example\nttl: [1]\n");
    assert_eq!(json["ttl"], json!([1]));
    assert_eq!(ttl_ms(&m), None);
}

#[test]
fn duration_ttl_mapping_is_accepted_and_ignored() {
    let (m, json) = parse_ttl("name: example\nttl: {a: 1}\n");
    assert_eq!(json["ttl"], json!({"a": 1}));
    assert_eq!(ttl_ms(&m), None);
}

#[test]
fn duration_ttl_y11_boolean_shorthand_is_a_string() {
    // Measured serde_yaml behavior: `yes` is a plain string (it is not a
    // boolean in the previous parser), so the duration is ignored. The
    // strict-booleans option keeps that here.
    let (m, json) = parse_ttl("name: example\nttl: yes\n");
    assert_eq!(json["ttl"], json!("yes"));
    assert_eq!(ttl_ms(&m), None);
}

#[test]
fn duration_ttl_sexagesimal_is_a_string() {
    // Measured serde_yaml behavior: `1:30` is a plain string (not the YAML
    // 1.1 sexagesimal integer 90), so the duration is ignored.
    let (m, json) = parse_ttl("name: example\nttl: 1:30\n");
    assert_eq!(json["ttl"], json!("1:30"));
    assert_eq!(ttl_ms(&m), None);
}

#[test]
fn duration_ttl_hex_integer_is_normalized() {
    // Both parsers resolve `0x1F` to 31; the swap keeps it a 31-second TTL.
    let (m, json) = parse_ttl("name: example\nttl: 0x1F\n");
    assert_eq!(json["ttl"], json!(31));
    assert_eq!(ttl_ms(&m), Some(31_000));
}

#[test]
fn duration_ttl_leading_zero_number_is_an_ignored_float() {
    // `serde_yaml` read `0052` as the string "0052", which
    // `parse_duration_string_to_ms` accepted as 52 s (52_000 ms). With
    // serde-saphyr the same spelling is the float 52.0: accepted at parse
    // time, ignored during normalization (`parse_duration_to_ms` returns
    // `None`), so the field's default applies.
    let (m, json) = parse_ttl("name: example\nttl: 0052\n");
    assert_eq!(
        json["ttl"],
        json!(52.0),
        "the parsed value is the float 52.0"
    );
    assert_eq!(
        ttl_ms(&m),
        None,
        "the float is ignored by parse_duration_to_ms"
    );
}

// --- YAML 1.1 booleans in a bool field: today's result is REJECTED for the
// shorthand forms (serde_yaml treats them as strings that cannot be a bool),
// and accepted for the true/false spellings.

#[test]
fn y11_boolean_shorthands_in_a_bool_field_are_rejected() {
    for shorthand in ["yes", "no", "on", "off", "y", "n"] {
        let result = parse(&format!("name: example\nlowMemory: {shorthand}\n"));
        assert!(
            result.is_err(),
            "`lowMemory: {shorthand}` must stay a parse error (measured serde_yaml behavior)"
        );
    }
    assert!(parse("name: example\nenabled: yes\n").is_err());
}

#[test]
fn true_false_spellings_in_a_bool_field_are_accepted() {
    for (spelling, expected) in [
        ("true", true),
        ("false", false),
        ("True", true),
        ("FALSE", false),
    ] {
        let m = parse(&format!("name: example\nlowMemory: {spelling}\n"))
            .unwrap_or_else(|e| panic!("`lowMemory: {spelling}` must parse: {e}"));
        assert_eq!(m.low_memory, Some(expected));
    }
}

// --- Duplicate keys: the previous parser kept the LAST value inside mapping
// values (measured: `env: {A: 1, A: 2}` parsed with `A: 2`). The last-wins
// policy restores that; note it also relaxes duplicate STRUCT fields (which
// were a serde error before) to the same last-wins rule.

#[test]
fn duplicate_keys_in_mapping_values_keep_the_last_value() {
    let m = parse("name: example\nenv:\n  A: 1\n  A: 2\n").expect("measured serde_yaml: accepted");
    assert_eq!(
        m.env
            .as_ref()
            .and_then(|env| env.get("A"))
            .map(String::as_str),
        Some("2")
    );
    let m = parse("name: example\nenv: {A: 1, A: 2}").expect("measured serde_yaml: accepted");
    assert_eq!(
        m.env
            .as_ref()
            .and_then(|env| env.get("A"))
            .map(String::as_str),
        Some("2")
    );
}

#[test]
fn duplicate_struct_fields_keep_the_last_value() {
    let m = parse("name: a\nname: b\n").expect("last-wins duplicate key policy");
    assert_eq!(m.name, "b");
}

// --- Anchors and aliases.

#[test]
fn anchors_and_aliases_are_supported() {
    let m = parse("name: &x a\nversion: *x\n").expect("anchor + alias must parse");
    assert_eq!(m.name, "a");
    assert_eq!(m.version.as_deref(), Some("a"));

    let m =
        parse("name: example\nttl: &t 30\nqueueTimeout: *t\n").expect("duration alias must parse");
    assert_eq!(m.ttl.as_ref().and_then(parse_duration_to_ms), Some(30_000));
    assert_eq!(
        m.queue_timeout.as_ref().and_then(parse_duration_to_ms),
        Some(30_000)
    );
}

#[test]
fn forward_alias_is_a_parse_error() {
    assert!(
        parse("name: example\nqueueTimeout: *t\nttl: &t 30\n").is_err(),
        "an alias before its anchor must stay a parse error"
    );
}

// --- Empty and comment-only documents: the previous parser accepted them as
// the default manifest.

#[test]
fn empty_and_comment_only_documents_parse_to_the_default_manifest() {
    for text in ["", "# just a comment\n", "---\n"] {
        let m = parse(text).unwrap_or_else(|e| panic!("document must parse: {text:?}: {e}"));
        assert_eq!(
            m,
            WorkerManifest {
                name: String::new(),
                ..Default::default()
            }
        );
        assert_eq!(m.ttl, None);
    }
    // A trailing document end marker is also fine.
    let m = parse("name: example\n...\n").expect("trailing `...` must parse");
    assert_eq!(m.name, "example");
}

// --- Merge keys: the previous parser did NOT expand `<<`; the key was an
// ordinary (ignored) struct field and nothing merged.

#[test]
fn merge_keys_are_ignored_like_the_previous_parser() {
    let m = parse("defaults: &d\n  queueTimeout: 5s\nname: example\n<<: *d\n")
        .expect("a manifest with a merge key must parse");
    assert_eq!(
        m.queue_timeout, None,
        "the previous parser ignored `<<`; nothing may be merged"
    );
}

// --- JSON path: the same struct deserializes from JSON for the same forms
// (the package.json fallback builds the same shape), and the JSON a manifest
// serializes to is unchanged by the parser swap.

#[test]
fn json_deserialization_keeps_the_same_duration_forms() {
    for (ttl, expected_ms) in [
        (json!(30), Some(30_000)),
        (json!("30s"), Some(30_000)),
        (json!(0), Some(0)),
        (json!(1.5), None),
        (json!(-1), None),
        (json!(true), None),
        (json!([1]), None),
        (json!({"a": 1}), None),
    ] {
        let json_text = serde_json::to_string(&json!({"name": "example", "ttl": ttl})).unwrap();
        let m: WorkerManifest = serde_json::from_str(&json_text)
            .unwrap_or_else(|e| panic!("JSON manifest must deserialize: {json_text}: {e}"));
        assert_eq!(
            ttl_ms(&m),
            expected_ms,
            "JSON form {ttl} must normalize like the YAML one"
        );
        // Round-trip stability: serialize -> deserialize -> serialize.
        let again: WorkerManifest =
            serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(m, again);
    }
    // An explicit JSON null is the absent field.
    let m: WorkerManifest = serde_json::from_str(r#"{"name":"example","ttl":null}"#).unwrap();
    assert!(m.ttl.is_none());
}

#[test]
fn manifest_json_is_stable_across_round_trips() {
    let (m, json) = parse_ttl(
        "name: example\nversion: \"1.0.0\"\nttl: 30\nstreamTimeout: 1m\nqueueTimeout: 250ms\n",
    );
    let back: WorkerManifest = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), json);
    assert_eq!(m, back);
    // Spot-check the raw duration values survive the round trip unchanged.
    assert_eq!(json["ttl"], json!(30));
    assert_eq!(json["streamTimeout"], json!("1m"));
    assert_eq!(json["queueTimeout"], json!("250ms"));
    // And the normalized config is intact.
    let config = parse_worker_config(&m);
    assert_eq!(config.ttl_ms, 30_000);
    assert_eq!(config.stream_max_duration_ms, Some(60_000));
    assert_eq!(config.queue_timeout_ms, 250);
    assert_eq!(config.max_body_size_bytes, None);
    assert_eq!(
        edger_core::effective_max_body_size_bytes(&config),
        DEFAULT_MAX_BODY_BYTES
    );
}
