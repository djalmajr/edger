//! Tracing subscriber setup for the edger binary.

use tracing_subscriber::EnvFilter;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TracingInitConfig {
    pub env_filter: String,
    pub otel_exporter_otlp_endpoint: Option<String>,
    pub otel_traces_sampler: Option<String>,
}

impl TracingInitConfig {
    fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let env_filter = lookup("EDGER_LOG")
            .or_else(|| lookup("RUST_LOG"))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                "edger_orchestrator=info,edger_worker=info,edger_isolation=info".into()
            });
        Self {
            env_filter,
            otel_exporter_otlp_endpoint: lookup("OTEL_EXPORTER_OTLP_ENDPOINT")
                .filter(|value| !value.trim().is_empty()),
            otel_traces_sampler: lookup("OTEL_TRACES_SAMPLER")
                .filter(|value| !value.trim().is_empty()),
        }
    }
}

pub fn init_tracing_from_env() -> anyhow::Result<TracingInitConfig> {
    let config = TracingInitConfig::from_env();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&config.env_filter)?)
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to initialize tracing subscriber: {err}"))?;

    if config.otel_exporter_otlp_endpoint.is_some() {
        tracing::warn!(
            otel_exporter = "otlp",
            "OTEL exporter environment detected; continuing with fmt tracing because the OTLP exporter layer is not linked in this build"
        );
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn tracing_config_prefers_edger_log_over_rust_log_and_reads_otel_env() {
        let values = HashMap::from([
            ("EDGER_LOG", "edger_orchestrator=debug"),
            ("RUST_LOG", "warn"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317"),
            ("OTEL_TRACES_SAMPLER", "always_on"),
        ]);

        let config =
            TracingInitConfig::from_lookup(|name| values.get(name).map(|value| value.to_string()));

        assert_eq!(config.env_filter, "edger_orchestrator=debug");
        assert_eq!(
            config.otel_exporter_otlp_endpoint.as_deref(),
            Some("http://127.0.0.1:4317")
        );
        assert_eq!(config.otel_traces_sampler.as_deref(), Some("always_on"));
    }

    #[test]
    fn tracing_config_has_safe_default_filter() {
        let config = TracingInitConfig::from_lookup(|_| None);

        assert!(config.env_filter.contains("edger_orchestrator=info"));
        assert!(config.otel_exporter_otlp_endpoint.is_none());
    }
}
