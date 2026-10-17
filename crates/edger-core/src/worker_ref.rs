//! Worker reference used by orchestrator resolution.

use std::path::PathBuf;

use uuid::Uuid;

use crate::config::WorkerConfig;
use crate::execution::{normalize_fullstack_adapter, ExecutionKind, SUPPORTED_FULLSTACK_ADAPTERS};
use crate::manifest::WorkerManifest;
use crate::secrets::is_valid_secret_name;

/// Maximum number of `cron[]` entries a worker manifest may declare
/// (EDG-36); beyond it the manifest validation rejects the manifest.
pub const MAX_CRON_ENTRIES: usize = 32;
/// Minimum `@every` interval a worker manifest may declare (EDG-36, ms):
/// below it the manifest validation rejects the schedule, and the
/// scheduler's own parser treats it as an invalid schedule (defense in
/// depth for indexes built before the limit).
pub const MIN_CRON_EVERY_MS: u64 = 1_000;
/// Maximum `egress.ports[]` entries a worker manifest may declare (EDG-38).
pub const MAX_EGRESS_PORTS: usize = 8;
/// Maximum `secrets[]` entries a worker manifest may declare (EDG-38).
pub const MAX_SECRETS: usize = 32;
/// Egress duration ceilings (EDG-38, ms): `connectTimeout` up to 60 s,
/// `headersTimeout` and `idleTimeout` up to 5 min, `totalTimeout` up to 1 h.
pub const MAX_EGRESS_CONNECT_TIMEOUT_MS: u64 = 60_000;
pub const MAX_EGRESS_HEADERS_TIMEOUT_MS: u64 = 300_000;
pub const MAX_EGRESS_IDLE_TIMEOUT_MS: u64 = 300_000;
pub const MAX_EGRESS_TOTAL_TIMEOUT_MS: u64 = 3_600_000;
/// Egress response-body ceiling (EDG-38): `maxResponseBytes` up to 1 GiB.
pub const MAX_EGRESS_MAX_RESPONSE_BYTES: u64 = 1024 * 1024 * 1024;
/// Egress `maxConcurrent` range (EDG-38): 1 to 256.
pub const MAX_EGRESS_MAX_CONCURRENT: u32 = 256;

/// Resolved worker identity (namespaced + semver).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerRef {
    pub id: Uuid,
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
    pub namespace: Option<String>,
    pub kind: ExecutionKind,
    pub config: WorkerConfig,
}

/// Parse `@scope/name` into namespace + local name.
pub fn parse_namespaced_name(full_name: &str) -> (Option<String>, String) {
    if let Some((scope, local)) = full_name.split_once('/') {
        if scope.starts_with('@') {
            return (Some(scope.to_string()), local.to_string());
        }
    }
    (None, full_name.to_string())
}

/// Create a worker ref from manifest + directory (pure; no filesystem reads).
pub fn create_worker_ref(
    dir: PathBuf,
    manifest: WorkerManifest,
) -> Result<WorkerRef, crate::error::CoreError> {
    if manifest.name.trim().is_empty() {
        return Err(crate::error::CoreError::validation(
            "manifest.name",
            "name is required",
        ));
    }
    validate_worker_manifest(&manifest)?;
    let version = manifest.version.clone().unwrap_or_else(|| "latest".into());
    let config = crate::config::parse_worker_config(&manifest);
    let kind = config.kind.clone().unwrap_or(ExecutionKind::FetchHandler);
    let (namespace, local_name) = parse_namespaced_name(&manifest.name);

    Ok(WorkerRef {
        id: Uuid::new_v4(),
        name: if namespace.is_some() {
            manifest.name.clone()
        } else {
            local_name
        },
        version,
        dir,
        namespace,
        kind,
        config,
    })
}

pub fn validate_worker_manifest(manifest: &WorkerManifest) -> Result<(), crate::error::CoreError> {
    if let Some(check) = manifest.health_check.as_ref() {
        if !check.path.starts_with('/') || check.path.starts_with("//") || check.path.contains("..")
        {
            return Err(crate::error::CoreError::validation(
                "manifest.healthCheck.path",
                "healthCheck.path must be an absolute worker pathname without traversal",
            ));
        }
        let method = check
            .method
            .as_deref()
            .unwrap_or("GET")
            .to_ascii_uppercase();
        if !matches!(method.as_str(), "GET" | "HEAD") {
            return Err(crate::error::CoreError::validation(
                "manifest.healthCheck.method",
                "healthCheck.method must be GET or HEAD",
            ));
        }
        if let Some(timeout) = check.timeout.as_deref() {
            let Some(timeout_ms) = crate::config::parse_duration_string_to_ms(timeout) else {
                return Err(crate::error::CoreError::validation(
                    "manifest.healthCheck.timeout",
                    "healthCheck.timeout must be a duration such as 500ms or 2s",
                ));
            };
            if !(100..=10_000).contains(&timeout_ms) {
                return Err(crate::error::CoreError::validation(
                    "manifest.healthCheck.timeout",
                    "healthCheck.timeout must be between 100ms and 10s",
                ));
            }
        }
    }

    if let Some(warmup) = manifest.warmup.as_ref() {
        // Same error pattern as `basePath` above: a warmup path is always
        // an absolute worker pathname (the synthetic request is a GET to
        // that path). An unparseable `timeout` is NOT rejected here — the
        // normalization falls back to the 10 s default.
        let path = warmup.path.trim();
        if path.is_empty() || !path.starts_with('/') {
            return Err(crate::error::CoreError::validation(
                "manifest.warmup.path",
                "warmup.path must be an absolute path starting with /",
            ));
        }
    }

    if let Some(cron) = manifest.cron.as_deref() {
        // (EDG-36) `cron[]` is bounded: beyond `MAX_CRON_ENTRIES` entries
        // the manifest is rejected as a whole (field `manifest.cron`), so
        // the scheduler's job set and the admin read-out stay finite.
        if cron.len() > MAX_CRON_ENTRIES {
            return Err(crate::error::CoreError::validation(
                "manifest.cron",
                format!("cron must have at most {MAX_CRON_ENTRIES} entries"),
            ));
        }
        // (EDG-34 emenda 2, CWE-117) control characters in a `cron[]`
        // `path`, `schedule` or `method` could forge physical log lines in
        // the scheduler's log output (the cron lines log these fields):
        // reject them at manifest validation, with the same error type as
        // the other cron validations.
        for (position, entry) in cron.iter().enumerate() {
            if entry.path.chars().any(char::is_control) {
                return Err(crate::error::CoreError::validation(
                    &format!("manifest.cron[{position}].path"),
                    "cron path must not contain control characters",
                ));
            }
            if entry.schedule.chars().any(char::is_control) {
                return Err(crate::error::CoreError::validation(
                    &format!("manifest.cron[{position}].schedule"),
                    "cron schedule must not contain control characters",
                ));
            }
            if entry
                .method
                .as_deref()
                .is_some_and(|method| method.chars().any(char::is_control))
            {
                return Err(crate::error::CoreError::validation(
                    &format!("manifest.cron[{position}].method"),
                    "cron method must not contain control characters",
                ));
            }
            // (EDG-36) an `@every` interval below the 1 s minimum is
            // rejected by field. An `@every` the duration parser does not
            // accept (including one that overflows, EDG-36) is NOT
            // rejected here: the scheduler skips it with a `warn`, as
            // before.
            if let Some(duration) = entry.schedule.trim().strip_prefix("@every ") {
                if let Some(ms) = crate::config::parse_duration_string_to_ms(duration) {
                    if ms < MIN_CRON_EVERY_MS {
                        return Err(crate::error::CoreError::validation(
                            &format!("manifest.cron[{position}].schedule"),
                            "cron @every interval must be at least 1s",
                        ));
                    }
                }
            }
        }
    }

    // (EDG-38) egress guard and app secrets. These run before the `kind`
    // early return: they apply to every worker kind. With `egress` absent
    // or `enabled: false` nothing is checked — the other egress fields are
    // ignored, and the manifest behaves exactly as before.
    if let Some(egress) = manifest.egress.as_ref() {
        if egress.enabled.unwrap_or(false) {
            // The guard is the only outbound path for the hosts the user
            // chose: without a declared `allowNet` the worker's network is
            // open and the guard loses its meaning (it may be empty).
            if manifest.allow_net.is_none() {
                return Err(crate::error::CoreError::validation(
                    "manifest.egress",
                    "egress requires allowNet to be declared (it may be empty)",
                ));
            }
            if let Some(ports) = egress.ports.as_deref() {
                if ports.is_empty() || ports.len() > MAX_EGRESS_PORTS {
                    return Err(crate::error::CoreError::validation(
                        "manifest.egress.ports",
                        format!("egress.ports must have between 1 and {MAX_EGRESS_PORTS} entries"),
                    ));
                }
                for (position, port) in ports.iter().enumerate() {
                    // `u16` already bounds 65535; zero is the only value
                    // outside the 1..=65535 range.
                    if *port == 0 {
                        return Err(crate::error::CoreError::validation(
                            &format!("manifest.egress.ports[{position}]"),
                            "egress.ports entries must be between 1 and 65535",
                        ));
                    }
                    if ports[..position].contains(port) {
                        return Err(crate::error::CoreError::validation(
                            &format!("manifest.egress.ports[{position}]"),
                            "egress.ports entries must not repeat",
                        ));
                    }
                }
            }
            validate_egress_duration(
                "manifest.egress.connectTimeout",
                egress.connect_timeout.as_deref(),
                MAX_EGRESS_CONNECT_TIMEOUT_MS,
                "60s",
            )?;
            validate_egress_duration(
                "manifest.egress.headersTimeout",
                egress.headers_timeout.as_deref(),
                MAX_EGRESS_HEADERS_TIMEOUT_MS,
                "5m",
            )?;
            validate_egress_duration(
                "manifest.egress.idleTimeout",
                egress.idle_timeout.as_deref(),
                MAX_EGRESS_IDLE_TIMEOUT_MS,
                "5m",
            )?;
            validate_egress_duration(
                "manifest.egress.totalTimeout",
                egress.total_timeout.as_deref(),
                MAX_EGRESS_TOTAL_TIMEOUT_MS,
                "1h",
            )?;
            if let Some(size) = egress.max_response_bytes.as_deref() {
                // A size whose byte form does not fit `u64` parses to `None`
                // (checked multiplication, EDG-38) and fails here like any
                // unparseable size; zero is rejected too — a response cap of
                // 0 bytes is not a valid limit.
                let Some(bytes) = crate::config::parse_size_to_bytes(size) else {
                    return Err(crate::error::CoreError::validation(
                        "manifest.egress.maxResponseBytes",
                        "egress.maxResponseBytes must be a size such as 100mb",
                    ));
                };
                if bytes == 0 {
                    return Err(crate::error::CoreError::validation(
                        "manifest.egress.maxResponseBytes",
                        "egress.maxResponseBytes must be greater than 0",
                    ));
                }
                if bytes > MAX_EGRESS_MAX_RESPONSE_BYTES {
                    return Err(crate::error::CoreError::validation(
                        "manifest.egress.maxResponseBytes",
                        "egress.maxResponseBytes must be at most 1gb",
                    ));
                }
            }
            if let Some(max_concurrent) = egress.max_concurrent {
                if !(1..=MAX_EGRESS_MAX_CONCURRENT).contains(&max_concurrent) {
                    return Err(crate::error::CoreError::validation(
                        "manifest.egress.maxConcurrent",
                        format!("egress.maxConcurrent must be between 1 and {MAX_EGRESS_MAX_CONCURRENT}"),
                    ));
                }
            }
        }
    }

    if let Some(secrets) = manifest.secrets.as_deref() {
        if secrets.len() > MAX_SECRETS {
            return Err(crate::error::CoreError::validation(
                "manifest.secrets",
                format!("secrets must have at most {MAX_SECRETS} entries"),
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for (position, name) in secrets.iter().enumerate() {
            if !is_valid_secret_name(name) {
                return Err(crate::error::CoreError::validation(
                    &format!("manifest.secrets[{position}]"),
                    "secret names must match ^[A-Z_][A-Z0-9_]{0,63}$",
                ));
            }
            if name.starts_with("EDGER_") {
                return Err(crate::error::CoreError::validation(
                    &format!("manifest.secrets[{position}]"),
                    "secret names must not start with EDGER_ (runtime-injected keys are reserved)",
                ));
            }
            if seen.iter().any(|seen_name| *seen_name == name) {
                return Err(crate::error::CoreError::validation(
                    &format!("manifest.secrets[{position}]"),
                    "secret names must not repeat",
                ));
            }
            if manifest
                .env
                .as_ref()
                .is_some_and(|env| env.contains_key(name))
            {
                return Err(crate::error::CoreError::validation(
                    &format!("manifest.secrets[{position}]"),
                    "secret names must not collide with env keys of the same manifest",
                ));
            }
            // DENO_DIR is the only key `inject_manifest_env` reserves (it is
            // dropped at spawn, so the secret would be silently absent).
            if name == "DENO_DIR" {
                return Err(crate::error::CoreError::validation(
                    &format!("manifest.secrets[{position}]"),
                    "secret name DENO_DIR is reserved for the runtime cache dir",
                ));
            }
            // A secret named by `publicEnv` would be injected into the
            // worker env and then exposed to the browser through the
            // publicEnv allowlist. The consumer trims each publicEnv key
            // before the lookup (static_spa `public_runtime_env`), so the
            // comparison uses the same normalization.
            if manifest
                .public_env
                .iter()
                .any(|public_name| public_name.trim() == name)
            {
                return Err(crate::error::CoreError::validation(
                    &format!("manifest.secrets[{position}]"),
                    "secret names must not appear in publicEnv of the same manifest (the value would be exposed to the browser)",
                ));
            }
            seen.push(name);
        }
    }

    let Some(kind) = manifest.kind.as_deref() else {
        return Ok(());
    };
    if !matches!(
        kind.trim().to_ascii_lowercase().as_str(),
        "ssr" | "fullstack"
    ) {
        return Ok(());
    }

    let adapters = SUPPORTED_FULLSTACK_ADAPTERS.join(", ");
    let Some(adapter) = manifest
        .adapter
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Err(crate::error::CoreError::validation(
            "manifest.adapter",
            format!("adapter is required for kind fullstack (expected one of: {adapters})"),
        ));
    };
    if normalize_fullstack_adapter(adapter).is_none() {
        return Err(crate::error::CoreError::validation(
            "manifest.adapter",
            format!("unsupported adapter {adapter:?} (expected one of: {adapters})"),
        ));
    }

    let has_ssr_entrypoint = manifest
        .ssr_entrypoint
        .as_deref()
        .or(manifest.entrypoint.as_deref())
        .map(str::trim)
        .is_some_and(|entry| !entry.is_empty());
    if !has_ssr_entrypoint {
        return Err(crate::error::CoreError::validation(
            "manifest.ssrEntrypoint",
            "ssrEntrypoint is required for kind fullstack (entrypoint is accepted as an alias)",
        ));
    }

    if let Some(base_path) = manifest.base_path.as_deref().map(str::trim) {
        if !base_path.is_empty()
            && !base_path.eq_ignore_ascii_case("auto")
            && !base_path.starts_with('/')
        {
            return Err(crate::error::CoreError::validation(
                "manifest.basePath",
                "basePath must be \"auto\" or an absolute path starting with /",
            ));
        }
    }

    Ok(())
}

/// One egress duration field (EDG-38): present values must parse, be
/// greater than zero, and stay under the ceiling. An invalid field is
/// rejected — the validation never falls back to the default silently.
fn validate_egress_duration(
    field: &str,
    value: Option<&str>,
    max_ms: u64,
    max_display: &str,
) -> Result<(), crate::error::CoreError> {
    let Some(value) = value else {
        return Ok(());
    };
    let Some(ms) = crate::config::parse_duration_string_to_ms(value) else {
        return Err(crate::error::CoreError::validation(
            field,
            "must be a duration such as 5s or 5m",
        ));
    };
    if ms == 0 {
        return Err(crate::error::CoreError::validation(
            field,
            "must be greater than 0",
        ));
    }
    if ms > max_ms {
        return Err(crate::error::CoreError::validation(
            field,
            format!("must be at most {max_display}"),
        ));
    }
    Ok(())
}

/// A secret name (EDG-38): `^[A-Z_][A-Z0-9_]{0,63}$` — 1 to 64 characters.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::CronJob;

    fn manifest_with_cron(schedule: &str, path: &str) -> WorkerManifest {
        WorkerManifest {
            name: "cron-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            cron: Some(vec![CronJob {
                schedule: schedule.into(),
                path: path.into(),
                method: None,
            }]),
            ..Default::default()
        }
    }

    fn manifest_with_cron_method(schedule: &str, path: &str, method: &str) -> WorkerManifest {
        WorkerManifest {
            name: "cron-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            cron: Some(vec![CronJob {
                schedule: schedule.into(),
                path: path.into(),
                method: Some(method.into()),
            }]),
            ..Default::default()
        }
    }

    /// (EDG-34 emenda 2, CWE-117) a `cron[]` path with an embedded newline
    /// is rejected at manifest validation — the manifest can never reach
    /// the index (and the scheduler's log lines) with it.
    #[test]
    fn cron_path_with_a_newline_is_rejected() {
        let err =
            validate_worker_manifest(&manifest_with_cron("@every 1s", "/ti\nck")).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        assert!(
            err.message.contains("manifest.cron[0].path"),
            "the error names the offending field: {}",
            err.message
        );
    }

    /// (EDG-34 emenda 2, CWE-117) a `cron[]` schedule with an embedded
    /// carriage return is rejected too (any `char::is_control` is
    /// rejected, not only CR/LF).
    #[test]
    fn cron_schedule_with_a_carriage_return_is_rejected() {
        let err =
            validate_worker_manifest(&manifest_with_cron("@every 100\rms", "/tick")).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        assert!(
            err.message.contains("manifest.cron[0].schedule"),
            "the error names the offending field: {}",
            err.message
        );
    }

    /// Clean cron fields keep passing the manifest validation. (EDG-36:
    /// the `@every` interval is at least 1 s — a sub-second value is
    /// rejected by its own rule, which is covered below.)
    #[test]
    fn cron_clean_fields_pass_validation() {
        assert!(validate_worker_manifest(&manifest_with_cron("@every 1s", "/tick")).is_ok());
        assert!(validate_worker_manifest(&manifest_with_cron("0 9 * * 1", "/reports")).is_ok());
        assert!(
            validate_worker_manifest(&manifest_with_cron_method("@every 1s", "/tick", "GET"))
                .is_ok()
        );
    }

    /// (EDG-34 rodada 3, CWE-117) a `cron[]` method with an embedded
    /// newline is rejected at manifest validation — the manifest can
    /// never reach the index (and the scheduler's invalid-job warning
    /// line) with it. Control characters in `path` and `schedule` were
    /// already rejected (emenda 2); this closes the `method` hole.
    #[test]
    fn cron_method_with_a_newline_is_rejected() {
        let err =
            validate_worker_manifest(&manifest_with_cron_method("@every 1s", "/tick", "GE\nT"))
                .unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        assert!(
            err.message.contains("manifest.cron[0].method"),
            "the error names the offending field: {}",
            err.message
        );
    }

    fn manifest_with_cron_entries(n: usize) -> WorkerManifest {
        WorkerManifest {
            name: "cron-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            cron: Some(
                (0..n)
                    .map(|i| CronJob {
                        schedule: "@every 1h".into(),
                        path: format!("/p{i}"),
                        method: None,
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    /// (EDG-36) `cron[]` is capped at `MAX_CRON_ENTRIES` entries: exactly
    /// 32 passes, 33 is rejected on the whole-array field.
    #[test]
    fn cron_entries_are_capped_at_max() {
        assert!(validate_worker_manifest(&manifest_with_cron_entries(MAX_CRON_ENTRIES)).is_ok());
        let err = validate_worker_manifest(&manifest_with_cron_entries(MAX_CRON_ENTRIES + 1))
            .unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        assert!(
            err.message.contains("manifest.cron"),
            "the error names the offending field: {}",
            err.message
        );
        assert!(
            err.message.contains("cron must have at most 32 entries"),
            "the message states the limit: {}",
            err.message
        );
    }

    /// (EDG-36) an `@every` interval below 1 s is rejected by field; the
    /// 1 s boundary passes in every accepted spelling.
    #[test]
    fn cron_every_below_one_second_is_rejected() {
        let err =
            validate_worker_manifest(&manifest_with_cron("@every 999ms", "/tick")).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        assert!(
            err.message.contains("manifest.cron[0].schedule"),
            "the error names the offending field: {}",
            err.message
        );
        assert!(
            err.message
                .contains("cron @every interval must be at least 1s"),
            "the message states the minimum: {}",
            err.message
        );
        for schedule in ["@every 1s", "@every 1000ms", "@every 1"] {
            assert!(
                validate_worker_manifest(&manifest_with_cron(schedule, "/tick")).is_ok(),
                "the boundary schedule must pass: {schedule}"
            );
        }
    }

    /// (EDG-36) an `@every` whose millisecond value overflows `u64` does
    /// not panic and is NOT rejected by the manifest validation: the
    /// duration parser answers `None`, and the scheduler skips such a job
    /// with a `warn`, exactly as before.
    #[test]
    fn cron_every_overflowing_u64_is_not_rejected_by_validation() {
        assert!(
            validate_worker_manifest(&manifest_with_cron("@every 18446744073709552s", "/tick"))
                .is_ok(),
            "an unparseable @every keeps the pre-EDG-36 behavior (scheduler warn, not a rejection)"
        );
    }

    // --- (EDG-38) egress guard and secrets -----------------------------------------------------------

    fn manifest_with_egress_yaml(body: &str) -> WorkerManifest {
        serde_saphyr::from_str_with_options(
            body,
            serde_saphyr::options! {
                strict_booleans: true,
                merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
                duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins,
                with_snippet: false,
            },
        )
        .expect("the manifest yaml parses")
    }

    fn egress_manifest(
        egress: Option<crate::manifest::EgressManifest>,
        allow_net: Option<Vec<String>>,
    ) -> WorkerManifest {
        WorkerManifest {
            name: "egress-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            allow_net,
            egress,
            ..Default::default()
        }
    }

    /// (EDG-38) `egress.enabled: true` without a declared `allowNet` is
    /// rejected: without the allowlist the worker's network stays open and
    /// the guard loses its meaning. A declared `allowNet` (even empty) is
    /// accepted.
    #[test]
    fn egress_requires_allownet_to_be_declared() {
        let err = validate_worker_manifest(&egress_manifest(
            Some(crate::manifest::EgressManifest {
                enabled: Some(true),
                ..Default::default()
            }),
            None,
        ))
        .unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        assert!(
            err.message.contains("manifest.egress"),
            "the error names the offending field: {}",
            err.message
        );
        assert!(
            err.message
                .contains("egress requires allowNet to be declared (it may be empty)"),
            "the message states the rule: {}",
            err.message
        );

        let ok = egress_manifest(
            Some(crate::manifest::EgressManifest {
                enabled: Some(true),
                ..Default::default()
            }),
            Some(vec!["ai-gateway.vercel.sh:443".into()]),
        );
        assert!(
            validate_worker_manifest(&ok).is_ok(),
            "allowNet declared with entries passes"
        );
        // `allowNet: []` is declared (the guard becomes the only path).
        let empty = egress_manifest(
            Some(crate::manifest::EgressManifest {
                enabled: Some(true),
                ..Default::default()
            }),
            Some(vec![]),
        );
        assert!(
            validate_worker_manifest(&empty).is_ok(),
            "an empty allowNet is declared and passes"
        );
    }

    /// (EDG-38) with `egress` absent or `enabled: false` nothing changes:
    /// invalid egress fields are ignored and the manifest passes.
    #[test]
    fn egress_disabled_or_absent_ignores_the_other_fields() {
        let invalid_fields = crate::manifest::EgressManifest {
            enabled: Some(false),
            ports: Some(vec![0, 0, 7, 7, 8, 9, 10, 11, 12]),
            connect_timeout: Some("soon".into()),
            max_response_bytes: Some("nope".into()),
            max_concurrent: Some(0),
            ..Default::default()
        };
        assert!(
            validate_worker_manifest(&egress_manifest(Some(invalid_fields), Some(vec![]),)).is_ok(),
            "enabled: false ignores the rest of the egress object"
        );
        // `enabled` absent behaves like `false`.
        let no_enabled = crate::manifest::EgressManifest {
            enabled: None,
            ports: Some(vec![0]),
            ..Default::default()
        };
        assert!(
            validate_worker_manifest(&egress_manifest(Some(no_enabled), Some(vec![]),)).is_ok()
        );
        assert!(
            validate_worker_manifest(&egress_manifest(None, None)).is_ok(),
            "no egress at all: nothing new is checked"
        );
    }

    /// (EDG-38) `egress.ports`: 1 to 8 entries, each 1 to 65535, no
    /// repetition — inside and outside each limit.
    #[test]
    fn egress_ports_are_bounded_unique_and_nonzero() {
        let ok_ports = |ports: Vec<u16>| {
            egress_manifest(
                Some(crate::manifest::EgressManifest {
                    enabled: Some(true),
                    ports: Some(ports),
                    ..Default::default()
                }),
                Some(vec![]),
            )
        };
        assert!(validate_worker_manifest(&ok_ports(vec![443])).is_ok());
        assert!(
            validate_worker_manifest(&ok_ports(vec![1, 443, 8080, 65535, 2, 3, 4, 5])).is_ok(),
            "8 entries at the boundary pass"
        );
        assert!(validate_worker_manifest(&ok_ports(vec![443, 443])).is_err());
        assert!(validate_worker_manifest(&ok_ports(vec![0])).is_err());
        assert!(
            validate_worker_manifest(&ok_ports(vec![1, 2, 3, 4, 5, 6, 7, 8, 9])).is_err(),
            "9 entries exceed the limit"
        );
        assert!(validate_worker_manifest(&ok_ports(vec![])).is_err());
        let err = validate_worker_manifest(&ok_ports(vec![443, 0])).unwrap_err();
        assert!(
            err.message.contains("manifest.egress.ports[1]"),
            "the error names the offending entry: {}",
            err.message
        );
        let err = validate_worker_manifest(&ok_ports(vec![443, 443])).unwrap_err();
        assert!(
            err.message.contains("manifest.egress.ports[1]"),
            "the duplicate names the second entry: {}",
            err.message
        );
    }

    /// (EDG-38) the egress durations: each with a value inside the ceiling
    /// (including the boundary) and one outside; zero and unparseable
    /// values are rejected — the validation never falls back silently.
    #[test]
    fn egress_durations_are_validated_with_ceilings() {
        let with_duration = |field: &str, value: &str| {
            let mut egress = crate::manifest::EgressManifest {
                enabled: Some(true),
                ..Default::default()
            };
            match field {
                "connectTimeout" => egress.connect_timeout = Some(value.into()),
                "headersTimeout" => egress.headers_timeout = Some(value.into()),
                "idleTimeout" => egress.idle_timeout = Some(value.into()),
                "totalTimeout" => egress.total_timeout = Some(value.into()),
                _ => unreachable!("{field}"),
            }
            egress_manifest(Some(egress), Some(vec![]))
        };

        // Inside (and at the boundary) each ceiling.
        assert!(validate_worker_manifest(&with_duration("connectTimeout", "5s")).is_ok());
        assert!(validate_worker_manifest(&with_duration("connectTimeout", "60s")).is_ok());
        assert!(validate_worker_manifest(&with_duration("connectTimeout", "1ms")).is_ok());
        assert!(validate_worker_manifest(&with_duration("headersTimeout", "30s")).is_ok());
        assert!(validate_worker_manifest(&with_duration("headersTimeout", "5m")).is_ok());
        assert!(validate_worker_manifest(&with_duration("idleTimeout", "300s")).is_ok());
        assert!(validate_worker_manifest(&with_duration("totalTimeout", "5m")).is_ok());
        assert!(validate_worker_manifest(&with_duration("totalTimeout", "1h")).is_ok());

        // Outside each ceiling, zero, and unparseable.
        assert!(validate_worker_manifest(&with_duration("connectTimeout", "61s")).is_err());
        assert!(validate_worker_manifest(&with_duration("headersTimeout", "5m1s")).is_err());
        assert!(validate_worker_manifest(&with_duration("headersTimeout", "301s")).is_err());
        assert!(validate_worker_manifest(&with_duration("idleTimeout", "0")).is_err());
        assert!(validate_worker_manifest(&with_duration("totalTimeout", "3601s")).is_err());
        assert!(validate_worker_manifest(&with_duration("totalTimeout", "soon")).is_err());

        let err = validate_worker_manifest(&with_duration("connectTimeout", "61s")).unwrap_err();
        assert!(
            err.message.contains("manifest.egress.connectTimeout"),
            "the error names the field: {}",
            err.message
        );
        assert!(
            err.message.contains("must be at most 60s"),
            "the message states the ceiling: {}",
            err.message
        );
        let err = validate_worker_manifest(&with_duration("totalTimeout", "3601s")).unwrap_err();
        assert!(
            err.message.contains("must be at most 1h"),
            "the total ceiling: {}",
            err.message
        );
        let err = validate_worker_manifest(&with_duration("idleTimeout", "0")).unwrap_err();
        assert!(
            err.message.contains("must be greater than 0"),
            "zero is rejected: {}",
            err.message
        );
        let err = validate_worker_manifest(&with_duration("totalTimeout", "soon")).unwrap_err();
        assert!(
            err.message.contains("must be a duration such as 5s or 5m"),
            "the unparseable form: {}",
            err.message
        );
    }

    /// (EDG-38) `maxResponseBytes` (the `maxBodySize` size format) is valid
    /// up to 1 GiB; `maxConcurrent` is 1 to 256 — inside and outside each
    /// limit.
    #[test]
    fn egress_max_response_bytes_and_max_concurrent_are_bounded() {
        let with_size = |value: &str| {
            egress_manifest(
                Some(crate::manifest::EgressManifest {
                    enabled: Some(true),
                    max_response_bytes: Some(value.into()),
                    ..Default::default()
                }),
                Some(vec![]),
            )
        };
        assert!(validate_worker_manifest(&with_size("100mb")).is_ok());
        assert!(validate_worker_manifest(&with_size("104857600b")).is_ok());
        assert!(validate_worker_manifest(&with_size("1gb")).is_ok());
        assert!(validate_worker_manifest(&with_size("2gb")).is_err());
        assert!(validate_worker_manifest(&with_size("1025mb")).is_err());
        assert!(validate_worker_manifest(&with_size("soon")).is_err());
        // (EDG-38) zero is not a valid cap, and a size whose byte form does
        // not fit `u64` is invalid (checked multiplication), not a panic.
        assert!(validate_worker_manifest(&with_size("0")).is_err());
        assert!(validate_worker_manifest(&with_size("0b")).is_err());
        assert!(validate_worker_manifest(&with_size("17179869184gb")).is_err());
        let err = validate_worker_manifest(&with_size("0")).unwrap_err();
        assert!(
            err.message.contains("must be greater than 0"),
            "the zero message: {}",
            err.message
        );
        let err = validate_worker_manifest(&with_size("17179869184gb")).unwrap_err();
        assert!(
            err.message.contains("must be a size such as 100mb"),
            "the overflow fails the parse check: {}",
            err.message
        );
        let err = validate_worker_manifest(&with_size("2gb")).unwrap_err();
        assert!(
            err.message.contains("manifest.egress.maxResponseBytes"),
            "the error names the field: {}",
            err.message
        );
        assert!(
            err.message.contains("must be at most 1gb"),
            "the message states the ceiling: {}",
            err.message
        );

        let with_concurrent = |value: u32| {
            egress_manifest(
                Some(crate::manifest::EgressManifest {
                    enabled: Some(true),
                    max_concurrent: Some(value),
                    ..Default::default()
                }),
                Some(vec![]),
            )
        };
        assert!(validate_worker_manifest(&with_concurrent(1)).is_ok());
        assert!(validate_worker_manifest(&with_concurrent(8)).is_ok());
        assert!(validate_worker_manifest(&with_concurrent(256)).is_ok());
        assert!(validate_worker_manifest(&with_concurrent(0)).is_err());
        assert!(validate_worker_manifest(&with_concurrent(257)).is_err());
        let err = validate_worker_manifest(&with_concurrent(257)).unwrap_err();
        assert!(
            err.message.contains("manifest.egress.maxConcurrent"),
            "the error names the field: {}",
            err.message
        );
        assert!(
            err.message.contains("must be between 1 and 256"),
            "the message states the range: {}",
            err.message
        );
    }

    /// (EDG-38) `secrets`: up to 32 names, each matching
    /// `^[A-Z_][A-Z0-9_]{0,63}$`, no repetition, no `EDGER_` prefix, and no
    /// collision with an `env` key of the same manifest.
    #[test]
    fn secrets_names_are_validated() {
        let with_secrets = |names: Vec<&str>| WorkerManifest {
            name: "secrets-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            secrets: Some(names.into_iter().map(str::to_string).collect()),
            ..Default::default()
        };
        assert!(validate_worker_manifest(&with_secrets(vec!["BYOK_MASTER_KEY"])).is_ok());
        assert!(validate_worker_manifest(&with_secrets(vec!["A"])).is_ok());
        assert!(validate_worker_manifest(&with_secrets(vec!["_LEAD"])).is_ok());
        assert!(validate_worker_manifest(&with_secrets(vec!["A_1"])).is_ok());
        let long64 = "A".repeat(64);
        let long65 = "A".repeat(65);
        assert!(
            validate_worker_manifest(&with_secrets(vec![&long64])).is_ok(),
            "64 characters is the boundary"
        );
        assert!(validate_worker_manifest(&with_secrets(vec!["bad-key"])).is_err());
        assert!(validate_worker_manifest(&with_secrets(vec!["1ABC"])).is_err());
        assert!(validate_worker_manifest(&with_secrets(vec!["abc"])).is_err());
        assert!(
            validate_worker_manifest(&with_secrets(vec![&long65])).is_err(),
            "65 characters exceeds the limit"
        );
        assert!(validate_worker_manifest(&with_secrets(vec![""])).is_err());
        let err = validate_worker_manifest(&with_secrets(vec!["bad-key"])).unwrap_err();
        assert!(
            err.message.contains("manifest.secrets[0]"),
            "the error names the entry: {}",
            err.message
        );
        assert!(
            err.message.contains("^[A-Z_][A-Z0-9_]{0,63}$"),
            "the message states the grammar: {}",
            err.message
        );

        // Repetition, the reserved EDGER_ prefix and the env collision.
        assert!(validate_worker_manifest(&with_secrets(vec!["A", "A"])).is_err());
        let err = validate_worker_manifest(&with_secrets(vec!["A", "A"])).unwrap_err();
        assert!(
            err.message.contains("manifest.secrets[1]"),
            "the repeat names the second entry: {}",
            err.message
        );
        assert!(validate_worker_manifest(&with_secrets(vec!["EDGER_KEY"])).is_err());
        let err = validate_worker_manifest(&with_secrets(vec!["EDGER_KEY"])).unwrap_err();
        assert!(
            err.message.contains("must not start with EDGER_"),
            "the reserved prefix: {}",
            err.message
        );
        let colliding = WorkerManifest {
            name: "secrets-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            env: Some(std::collections::HashMap::from([(
                "BYOK_MASTER_KEY".into(),
                "static".into(),
            )])),
            secrets: Some(vec!["BYOK_MASTER_KEY".into()]),
            ..Default::default()
        };
        let err = validate_worker_manifest(&colliding).unwrap_err();
        assert!(
            err.message.contains("must not collide with env keys"),
            "the env collision: {}",
            err.message
        );

        // 32 entries pass, 33 are rejected on the whole field.
        let names32: Vec<String> = (0..32).map(|i| format!("KEY{i}")).collect();
        assert!(validate_worker_manifest(&WorkerManifest {
            name: "secrets-test".into(),
            secrets: Some(names32),
            ..Default::default()
        })
        .is_ok());
        let names33: Vec<String> = (0..33).map(|i| format!("KEY{i}")).collect();
        let err = validate_worker_manifest(&WorkerManifest {
            name: "secrets-test".into(),
            secrets: Some(names33),
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.message.contains("manifest.secrets"),
            "the limit names the field: {}",
            err.message
        );
        assert!(
            err.message.contains("secrets must have at most 32 entries"),
            "the message states the limit: {}",
            err.message
        );
    }

    /// (EDG-38) a secret name that `inject_manifest_env` reserves
    /// (`DENO_DIR`, dropped at spawn) or that the same manifest lists in
    /// `publicEnv` (the value would reach the browser) is refused.
    #[test]
    fn secrets_reserved_and_public_env_names_are_refused() {
        let with_secrets = |names: Vec<&str>| WorkerManifest {
            name: "secrets-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            secrets: Some(names.into_iter().map(str::to_string).collect()),
            ..Default::default()
        };
        // DENO_DIR is reserved by the runtime cache dir.
        assert!(validate_worker_manifest(&with_secrets(vec!["DENO_DIR"])).is_err());
        let err = validate_worker_manifest(&with_secrets(vec!["DENO_DIR"])).unwrap_err();
        assert!(
            err.message.contains("reserved for the runtime cache dir"),
            "the reserved message: {}",
            err.message
        );
        // A name listed in publicEnv would be exposed to the browser.
        let public = WorkerManifest {
            name: "secrets-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            public_env: vec!["BYOK_MASTER".into()],
            secrets: Some(vec!["BYOK_MASTER".into()]),
            ..Default::default()
        };
        assert!(validate_worker_manifest(&public).is_err());
        let err = validate_worker_manifest(&public).unwrap_err();
        assert!(
            err.message.contains("must not appear in publicEnv"),
            "the publicEnv message: {}",
            err.message
        );
        // The consumer trims each publicEnv key before the lookup, so a
        // spaced entry is the same name and is refused too.
        let public_spaced = WorkerManifest {
            name: "secrets-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            public_env: vec![" BYOK_MASTER ".into()],
            secrets: Some(vec!["BYOK_MASTER".into()]),
            ..Default::default()
        };
        assert!(
            validate_worker_manifest(&public_spaced).is_err(),
            "a spaced publicEnv entry is the same name after the consumer's trim"
        );
        // Controls: the same name without publicEnv passes, and a publicEnv
        // entry that is not a secret name passes.
        assert!(validate_worker_manifest(&with_secrets(vec!["BYOK_MASTER"])).is_ok());
        let control = WorkerManifest {
            name: "secrets-test".into(),
            version: Some("1.0.0".into()),
            entrypoint: Some("index.ts".into()),
            public_env: vec!["PUBLIC_FLAG".into()],
            secrets: Some(vec!["BYOK_MASTER".into()]),
            ..Default::default()
        };
        assert!(validate_worker_manifest(&control).is_ok());
    }

    /// (EDG-38) the full egress object at the YAML level: the camelCase
    /// field names parse and validate end to end.
    #[test]
    fn egress_yaml_fields_validate_end_to_end() {
        let ok = manifest_with_egress_yaml(
            r#"
name: pinar
allowNet: []
egress:
  enabled: true
  ports: [443, 8443]
  connectTimeout: 5s
  headersTimeout: 30s
  idleTimeout: 30s
  totalTimeout: 5m
  maxResponseBytes: 100mb
  maxConcurrent: 8
secrets:
  - BYOK_MASTER_KEY
"#,
        );
        assert!(validate_worker_manifest(&ok).is_ok());
        assert_eq!(ok.egress.as_ref().unwrap().ports, Some(vec![443, 8443]));

        let bad = manifest_with_egress_yaml(
            r#"
name: pinar
allowNet: []
egress:
  enabled: true
  totalTimeout: 2h
"#,
        );
        assert!(
            validate_worker_manifest(&bad).is_err(),
            "a totalTimeout above 1h is rejected"
        );
    }
}
