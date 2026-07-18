//! Buntime manifest field mapping tests (story 02.02).

use edger_core::{
    create_worker_ref, effective_max_body_size_bytes, infer_execution_kind,
    parse_duration_string_to_ms, parse_size_to_bytes, parse_worker_config,
    validate_worker_manifest, DenoCacheMode, ExecutionKind, FullstackBasePath, WorkerIsolation,
    WorkerManifest, DEFAULT_MAX_BODY_BYTES,
};

#[test]
fn health_check_is_validated_and_normalized_without_periodic_mode() {
    let manifest = WorkerManifest {
        name: "health-worker".into(),
        health_check: Some(edger_core::WorkerHealthCheck {
            path: "/health".into(),
            method: Some("head".into()),
            mode: edger_core::WorkerHealthCheckMode::OnDeploy,
            timeout: Some("750ms".into()),
        }),
        ..Default::default()
    };
    validate_worker_manifest(&manifest).unwrap();
    let config = parse_worker_config(&manifest);
    let check = config.health_check.expect("normalized health check");
    assert_eq!(check.path, "/health");
    assert_eq!(check.method, "HEAD");
    assert_eq!(check.timeout_ms, 750);
    assert_eq!(check.mode, edger_core::WorkerHealthCheckMode::OnDeploy);

    for invalid in [
        edger_core::WorkerHealthCheck {
            path: "health".into(),
            method: None,
            mode: Default::default(),
            timeout: None,
        },
        edger_core::WorkerHealthCheck {
            path: "/health".into(),
            method: Some("POST".into()),
            mode: Default::default(),
            timeout: None,
        },
        edger_core::WorkerHealthCheck {
            path: "/health".into(),
            method: None,
            mode: Default::default(),
            timeout: Some("30s".into()),
        },
    ] {
        let invalid_manifest = WorkerManifest {
            name: "invalid-health".into(),
            health_check: Some(invalid),
            ..Default::default()
        };
        assert!(validate_worker_manifest(&invalid_manifest).is_err());
    }
}

const SAMPLE_YAML: &str = include_str!("fixtures/sample_manifest.yaml");

#[test]
fn manifest_deserializes_from_yaml_fixture() {
    let manifest: WorkerManifest = serde_yaml::from_str(SAMPLE_YAML).expect("yaml parse");
    assert_eq!(manifest.name, "@acme/checkout");
    assert_eq!(manifest.version.as_deref(), Some("1.2.3"));
    assert_eq!(manifest.max_requests, Some(1000));
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
    assert_eq!(config.circuit_breaker_failures, 3);
    assert_eq!(config.cooldown_ms, 30_000);
    assert_eq!(config.isolation, WorkerIsolation::Persistent);
    assert_eq!(config.queue_limit, 8);
    assert_eq!(config.queue_timeout_ms, 1_000);
    assert_eq!(config.max_body_size_bytes, Some(10 * 1024 * 1024));
    assert!(config.low_memory);
    assert!(!config.auto_install);
    assert!(config.inject_base);
    assert_eq!(config.cron.len(), 1);
    assert_eq!(config.kind, Some(ExecutionKind::FetchHandler));
}

#[test]
fn parse_worker_config_normalizes_circuit_breaker_and_oneshot_isolation() {
    let manifest: WorkerManifest = serde_yaml::from_str(
        r#"name: crashy
entrypoint: index.ts
maxRequests: 99
circuitBreakerFailures: 2
cooldown: 250ms
isolation: oneshot
"#,
    )
    .unwrap();

    let config = parse_worker_config(&manifest);

    assert_eq!(config.circuit_breaker_failures, 2);
    assert_eq!(config.cooldown_ms, 250);
    assert_eq!(config.isolation, WorkerIsolation::Oneshot);
    assert_eq!(
        config.max_requests, 1,
        "oneshot isolation must force recycle after exactly one request"
    );
}

#[test]
fn manifest_deserializes_public_env_allowlist() {
    // Guards against leaving publicEnv out of the manifest/config mapping.
    let manifest: WorkerManifest = serde_yaml::from_str(
        r#"name: public-spa
entrypoint: index.html
env:
  PUBLIC_API_URL: https://api.example.test
  INTERNAL_FLAG: hidden
publicEnv:
  - PUBLIC_API_URL
  - INTERNAL_FLAG
"#,
    )
    .unwrap();

    assert_eq!(
        manifest.public_env,
        vec!["PUBLIC_API_URL".to_string(), "INTERNAL_FLAG".to_string()]
    );

    let config = parse_worker_config(&manifest);
    assert_eq!(
        config.public_env,
        vec!["PUBLIC_API_URL".to_string(), "INTERNAL_FLAG".to_string()]
    );
}

#[test]
fn effective_body_limit_uses_global_default_without_manifest_override() {
    let config = parse_worker_config(&WorkerManifest {
        name: "default-body-limit".into(),
        ..Default::default()
    });

    assert_eq!(config.max_body_size_bytes, None);
    assert_eq!(
        effective_max_body_size_bytes(&config),
        DEFAULT_MAX_BODY_BYTES
    );
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
fn fullstack_manifest_requires_supported_adapter() {
    let missing = WorkerManifest {
        name: "ssr-app".into(),
        kind: Some("fullstack".into()),
        ssr_entrypoint: Some("server.js".into()),
        ..Default::default()
    };
    let err = validate_worker_manifest(&missing).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
    assert!(err.message.contains("manifest.adapter"));
    assert!(err.message.contains(
        "astro, fresh, hono, lume, nextjs, nuxt, remix, solidstart, sveltekit, tanstack"
    ));

    let invalid = WorkerManifest {
        adapter: Some("unknown".into()),
        ..missing
    };
    let err = validate_worker_manifest(&invalid).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
    assert!(err.message.contains("unsupported adapter"));
}

#[test]
fn fullstack_manifest_normalizes_deno_framework_aliases() {
    for (input, normalized) in [
        ("astro", "astro"),
        ("fresh", "fresh"),
        ("lume", "lume"),
        ("next", "nextjs"),
        ("nextjs", "nextjs"),
        ("nuxtjs", "nuxt"),
        ("react-router", "remix"),
        ("remix", "remix"),
        ("solid", "solidstart"),
        ("svelte", "sveltekit"),
        ("tanstack-start", "tanstack"),
    ] {
        let manifest = WorkerManifest {
            adapter: Some(input.into()),
            entrypoint: Some("server.js".into()),
            kind: Some("fullstack".into()),
            name: format!("{normalized}-app"),
            ..Default::default()
        };
        validate_worker_manifest(&manifest).unwrap();
        assert_eq!(
            infer_execution_kind(&manifest),
            ExecutionKind::Fullstack {
                adapter: normalized.into()
            }
        );
    }
}

#[test]
fn fullstack_manifest_accepts_entrypoint_as_ssr_alias() {
    let manifest = WorkerManifest {
        name: "ssr-app".into(),
        adapter: Some("hono".into()),
        entrypoint: Some("index.ts".into()),
        kind: Some("fullstack".into()),
        ..Default::default()
    };

    validate_worker_manifest(&manifest).unwrap();
    assert_eq!(
        infer_execution_kind(&manifest),
        ExecutionKind::Fullstack {
            adapter: "hono".into()
        }
    );
}

#[test]
fn lume_fullstack_manifest_accepts_static_site_entrypoint() {
    let manifest = WorkerManifest {
        name: "lume-app".into(),
        adapter: Some("lume".into()),
        client_dir: Some("_site".into()),
        entrypoint: Some("_site/index.html".into()),
        kind: Some("fullstack".into()),
        ..Default::default()
    };

    validate_worker_manifest(&manifest).unwrap();
    let config = parse_worker_config(&manifest);
    let fullstack = config.fullstack.unwrap();
    assert_eq!(fullstack.adapter, "lume");
    assert_eq!(fullstack.client_dir.as_deref(), Some("_site"));
    assert_eq!(fullstack.asset_prefixes, vec!["/".to_string()]);
}

#[test]
fn parse_worker_config_normalizes_fullstack_tanstack_fields() {
    let manifest: WorkerManifest = serde_yaml::from_str(
        r#"name: tanstack-demo
version: "1.0.0"
kind: fullstack
adapter: tanstack
ssrEntrypoint: server/server.js
clientDir: client
basePath: auto
"#,
    )
    .unwrap();

    let config = parse_worker_config(&manifest);

    assert_eq!(
        config.kind,
        Some(ExecutionKind::Fullstack {
            adapter: "tanstack".into()
        })
    );
    assert_eq!(config.ttl_ms, 300_000);
    // Regression (fullstack SSR spawn): the raw config `entrypoint` must mirror
    // `ssrEntrypoint`. The process backend spawns eagerly (Supervisor::spawn ->
    // Isolate::prepare) with this raw config, BEFORE the per-request fullstack
    // transform wires the entrypoint — a `None` here made the Deno SSR process
    // fail resolution with UDS_ENTRYPOINT_MISSING before ever serving a request.
    assert_eq!(config.entrypoint.as_deref(), Some("server/server.js"));
    let fullstack = config.fullstack.unwrap();
    assert_eq!(fullstack.adapter, "tanstack");
    assert_eq!(
        fullstack.ssr_entrypoint.as_deref(),
        Some("server/server.js")
    );
    assert_eq!(fullstack.client_dir.as_deref(), Some("client"));
    assert_eq!(fullstack.base_path, FullstackBasePath::Auto);
    assert!(fullstack.asset_prefixes.contains(&"/assets/".into()));
    assert!(fullstack.asset_prefixes.contains(&"/favicon.ico".into()));
}

#[test]
fn deno_frameworks_receive_static_asset_prefixes_when_client_dir_is_declared() {
    for (adapter, prefix) in [
        ("astro", "/_astro/"),
        ("lume", "/"),
        ("nuxt", "/_nuxt/"),
        ("remix", "/assets/"),
        ("solidstart", "/_build/"),
    ] {
        let config = parse_worker_config(&WorkerManifest {
            adapter: Some(adapter.into()),
            client_dir: Some("public".into()),
            kind: Some("fullstack".into()),
            name: format!("{adapter}-demo"),
            ssr_entrypoint: Some("server.ts".into()),
            ..Default::default()
        });
        let fullstack = config.fullstack.unwrap();
        assert!(fullstack.asset_prefixes.contains(&prefix.into()));
    }
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

#[test]
fn parse_worker_config_normalizes_worker_queue_controls() {
    let manifest = WorkerManifest {
        name: "queued".into(),
        queue_limit: Some(0),
        queue_timeout: Some(serde_yaml::Value::String("25ms".into())),
        ..Default::default()
    };

    let config = parse_worker_config(&manifest);

    assert_eq!(config.queue_limit, 0);
    assert_eq!(config.queue_timeout_ms, 25);
}

#[test]
fn parse_worker_config_normalizes_deno_sandbox_controls() {
    let manifest: WorkerManifest = serde_yaml::from_str(
        r#"name: sandboxed
entrypoint: index.ts
allow_net:
  - api.example.com
  - "cdn.example.com:443, jsr.io"
deno_cache_mode: shared
"#,
    )
    .unwrap();

    let config = parse_worker_config(&manifest);

    assert_eq!(
        config.allow_net,
        Some(vec![
            "api.example.com".into(),
            "cdn.example.com:443".into(),
            "jsr.io".into()
        ])
    );
    assert_eq!(config.deno_cache_mode, DenoCacheMode::Shared);
}
