//! Buntime manifest field mapping tests (story 02.02).

use edger_core::{
    create_worker_ref, infer_execution_kind, parse_duration_string_to_ms, parse_size_to_bytes,
    parse_worker_config, ExecutionKind, WorkerManifest,
};

const SAMPLE_YAML: &str = include_str!("fixtures/sample_manifest.yaml");

#[test]
fn manifest_deserializes_from_yaml_fixture() {
    let manifest: WorkerManifest = serde_yaml::from_str(SAMPLE_YAML).expect("yaml parse");
    assert_eq!(manifest.name, "@acme/checkout");
    assert_eq!(manifest.version.as_deref(), Some("1.2.3"));
    assert_eq!(manifest.max_requests, Some(1000));
    assert_eq!(manifest.shell_excludes, vec!["todos", "platform"]);
    assert!(manifest
        .public_routes
        .as_ref()
        .unwrap()
        .routes
        .contains(&"/health".into()));
}

#[test]
fn parse_worker_config_normalizes_buntime_fields() {
    let manifest: WorkerManifest = serde_yaml::from_str(SAMPLE_YAML).unwrap();
    let config = parse_worker_config(&manifest);

    assert!(config.enabled);
    assert_eq!(config.ttl_ms, 300_000);
    assert_eq!(config.timeout_ms, 30_000);
    assert_eq!(config.idle_timeout_ms, 120_000);
    assert_eq!(config.max_requests, 1000);
    assert_eq!(config.max_body_size_bytes, Some(10 * 1024 * 1024));
    assert!(config.low_memory);
    assert!(!config.auto_install);
    assert!(config.inject_base);
    assert_eq!(config.visibility, "protected");
    assert_eq!(config.shell_excludes, vec!["todos", "platform"]);
    assert_eq!(config.cron.len(), 1);
    assert_eq!(config.kind, Some(ExecutionKind::FetchHandler));
}

#[test]
fn worker_ref_includes_namespace_and_version() {
    let manifest: WorkerManifest = serde_yaml::from_str(SAMPLE_YAML).unwrap();
    let worker =
        create_worker_ref(std::path::PathBuf::from("/workers/checkout"), manifest).unwrap();
    assert_eq!(worker.namespace.as_deref(), Some("@acme"));
    assert_eq!(worker.name, "@acme/checkout");
    assert_eq!(worker.version, "1.2.3");
}

#[test]
fn infer_execution_kind_rules() {
    let spa = WorkerManifest {
        name: "ui".into(),
        entrypoint: Some("index.html".into()),
        inject_base: Some(true),
        ..Default::default()
    };
    assert_eq!(
        infer_execution_kind(&spa),
        ExecutionKind::StaticSpa { inject_base: true }
    );

    let wasm = WorkerManifest {
        name: "mod".into(),
        entrypoint: Some("handler.wasm".into()),
        ..Default::default()
    };
    assert_eq!(
        infer_execution_kind(&wasm),
        ExecutionKind::WasmModule {
            entry: Some("handler.wasm".into())
        }
    );

    let explicit = WorkerManifest {
        name: "api".into(),
        kind: Some("routes".into()),
        ..Default::default()
    };
    assert_eq!(infer_execution_kind(&explicit), ExecutionKind::RoutesTable);

    let explicit_wasm = WorkerManifest {
        name: "explicit-wasm".into(),
        entrypoint: Some("index.wasm".into()),
        kind: Some("wasm".into()),
        ..Default::default()
    };
    assert_eq!(
        infer_execution_kind(&explicit_wasm),
        ExecutionKind::WasmModule {
            entry: Some("index.wasm".into())
        }
    );

    let wat = WorkerManifest {
        name: "wat".into(),
        entrypoint: Some("index.wat".into()),
        ..Default::default()
    };
    assert_eq!(
        infer_execution_kind(&wat),
        ExecutionKind::WasmModule {
            entry: Some("index.wat".into())
        }
    );
}

#[test]
fn duration_and_size_parsers() {
    assert_eq!(parse_duration_string_to_ms("30s"), Some(30_000));
    assert_eq!(parse_duration_string_to_ms("100ms"), Some(100));
    assert_eq!(parse_duration_string_to_ms("5m"), Some(300_000));
    assert_eq!(parse_size_to_bytes("10mb"), Some(10 * 1024 * 1024));
    assert_eq!(parse_size_to_bytes("1024"), Some(1024));
}

#[test]
fn ttl_zero_means_ephemeral() {
    let manifest = WorkerManifest {
        name: "ephemeral".into(),
        ttl: Some(serde_yaml::Value::Number(0.into())),
        ..Default::default()
    };
    assert_eq!(parse_worker_config(&manifest).ttl_ms, 0);
}
