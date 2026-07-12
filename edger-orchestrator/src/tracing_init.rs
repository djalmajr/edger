//! Tracing subscriber setup for the edger binary.

use tracing_subscriber::EnvFilter;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TracingInitConfig {
    pub env_filter: String,
    pub otel_enabled: bool,
    pub otel_required: bool,
    pub otel_exporter_otlp_endpoint: Option<String>,
    pub otel_exporter_otlp_protocol: String,
    pub otel_traces_sampler: Option<String>,
    pub otel_traces_sampler_arg: Option<String>,
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
        let endpoint =
            lookup("OTEL_EXPORTER_OTLP_ENDPOINT").filter(|value| !value.trim().is_empty());
        let enabled = env_flag(lookup("EDGER_OTEL_ENABLED").as_deref(), endpoint.is_some());
        Self {
            env_filter,
            otel_enabled: enabled,
            otel_required: env_flag(lookup("EDGER_OTEL_REQUIRED").as_deref(), false),
            otel_exporter_otlp_endpoint: endpoint,
            otel_exporter_otlp_protocol: lookup("OTEL_EXPORTER_OTLP_PROTOCOL")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "grpc".into()),
            otel_traces_sampler: lookup("OTEL_TRACES_SAMPLER")
                .filter(|value| !value.trim().is_empty()),
            otel_traces_sampler_arg: lookup("OTEL_TRACES_SAMPLER_ARG")
                .filter(|value| !value.trim().is_empty()),
        }
    }
}

fn env_flag(value: Option<&str>, default: bool) -> bool {
    value
        .map(str::trim)
        .map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default)
}

pub struct TracingGuard {
    pub config: TracingInitConfig,
    #[cfg(feature = "otel")]
    provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    #[cfg(feature = "otel")]
    logger_provider: Option<opentelemetry_sdk::logs::SdkLoggerProvider>,
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        #[cfg(feature = "otel")]
        if let Some(provider) = self.provider.take() {
            if let Err(error) = provider.shutdown() {
                eprintln!("OTEL tracer shutdown did not complete cleanly: {error}");
            }
        }
        #[cfg(feature = "otel")]
        if let Some(provider) = self.logger_provider.take() {
            if let Err(error) = provider.shutdown() {
                eprintln!("OTEL logger shutdown did not complete cleanly: {error}");
            }
        }
    }
}

pub fn init_tracing_from_env() -> anyhow::Result<TracingGuard> {
    let config = TracingInitConfig::from_env();

    #[cfg(feature = "otel")]
    if config.otel_enabled {
        match init_otel_subscriber(&config) {
            Ok((provider, logger_provider)) => {
                return Ok(TracingGuard {
                    config,
                    provider: Some(provider),
                    logger_provider: Some(logger_provider),
                });
            }
            Err(error) if config.otel_required => return Err(error),
            Err(error) => {
                eprintln!("OTEL initialization failed; continuing with local tracing: {error}");
            }
        }
    }

    #[cfg(not(feature = "otel"))]
    if config.otel_enabled {
        if config.otel_required {
            anyhow::bail!(
                "OTEL is required but this EdgeR build does not include the `otel` feature"
            );
        }
        eprintln!("OTEL configuration detected; continuing locally because this build excludes the `otel` feature");
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&config.env_filter)?)
        .try_init()
        .map_err(|err| anyhow::anyhow!("failed to initialize tracing subscriber: {err}"))?;

    Ok(TracingGuard {
        config,
        #[cfg(feature = "otel")]
        provider: None,
        #[cfg(feature = "otel")]
        logger_provider: None,
    })
}

#[cfg(feature = "otel")]
fn init_otel_subscriber(
    config: &TracingInitConfig,
) -> anyhow::Result<(
    opentelemetry_sdk::trace::SdkTracerProvider,
    opentelemetry_sdk::logs::SdkLoggerProvider,
)> {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::{Protocol, WithExportConfig};
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    let endpoint = config
        .otel_exporter_otlp_endpoint
        .as_deref()
        .ok_or_else(|| {
            anyhow::anyhow!("OTEL_EXPORTER_OTLP_ENDPOINT is required when OTEL is enabled")
        })?;
    let trace_exporter = match config.otel_exporter_otlp_protocol.as_str() {
        "grpc" => opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()?,
        "http/protobuf" => {
            let traces_endpoint = if endpoint.trim_end_matches('/').ends_with("/v1/traces") {
                endpoint.to_string()
            } else {
                format!("{}/v1/traces", endpoint.trim_end_matches('/'))
            };
            opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .with_endpoint(traces_endpoint)
                .build()?
        }
        protocol => anyhow::bail!("unsupported OTLP protocol: {protocol}"),
    };
    let batch =
        opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor::builder(
            trace_exporter,
            opentelemetry_sdk::runtime::Tokio,
        )
        .with_batch_config(
            opentelemetry_sdk::trace::BatchConfigBuilder::default()
                .with_max_queue_size(2_048)
                .with_max_export_batch_size(512)
                .with_max_export_timeout(std::time::Duration::from_secs(5))
                .build(),
        )
        .build();
    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name("edger")
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_span_processor(batch)
        .with_sampler(build_sampler(config)?)
        .with_resource(resource.clone())
        .build();
    let log_exporter = match config.otel_exporter_otlp_protocol.as_str() {
        "grpc" => opentelemetry_otlp::LogExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()?,
        "http/protobuf" => {
            let logs_endpoint = if endpoint.trim_end_matches('/').ends_with("/v1/logs") {
                endpoint.to_string()
            } else {
                format!("{}/v1/logs", endpoint.trim_end_matches('/'))
            };
            opentelemetry_otlp::LogExporter::builder()
                .with_http()
                .with_protocol(Protocol::HttpBinary)
                .with_endpoint(logs_endpoint)
                .build()?
        }
        protocol => anyhow::bail!("unsupported OTLP protocol: {protocol}"),
    };
    let log_batch =
        opentelemetry_sdk::logs::log_processor_with_async_runtime::BatchLogProcessor::builder(
            log_exporter,
            opentelemetry_sdk::runtime::Tokio,
        )
        .build();
    let logger_provider = opentelemetry_sdk::logs::SdkLoggerProvider::builder()
        .with_log_processor(log_batch)
        .with_resource(resource)
        .build();
    let tracer = provider.tracer("edger-orchestrator");
    tracing_subscriber::registry()
        .with(EnvFilter::try_new(&config.env_filter)?)
        .with(tracing_subscriber::fmt::layer())
        .with(
            opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(
                &logger_provider,
            ),
        )
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .try_init()
        .map_err(|error| {
            anyhow::anyhow!("failed to initialize OTEL tracing subscriber: {error}")
        })?;
    Ok((provider, logger_provider))
}

#[cfg(feature = "otel")]
fn build_sampler(config: &TracingInitConfig) -> anyhow::Result<opentelemetry_sdk::trace::Sampler> {
    use opentelemetry_sdk::trace::Sampler;

    let sampler = config
        .otel_traces_sampler
        .as_deref()
        .unwrap_or("parentbased_always_on")
        .trim()
        .to_ascii_lowercase();
    let ratio = || -> anyhow::Result<f64> {
        let value = config
            .otel_traces_sampler_arg
            .as_deref()
            .unwrap_or("1.0")
            .parse::<f64>()?;
        if !(0.0..=1.0).contains(&value) {
            anyhow::bail!("OTEL_TRACES_SAMPLER_ARG must be between 0 and 1")
        }
        Ok(value)
    };

    match sampler.as_str() {
        "always_on" => Ok(Sampler::AlwaysOn),
        "always_off" => Ok(Sampler::AlwaysOff),
        "traceidratio" => Ok(Sampler::TraceIdRatioBased(ratio()?)),
        "parentbased_always_on" => Ok(Sampler::ParentBased(Box::new(Sampler::AlwaysOn))),
        "parentbased_always_off" => Ok(Sampler::ParentBased(Box::new(Sampler::AlwaysOff))),
        "parentbased_traceidratio" => Ok(Sampler::ParentBased(Box::new(
            Sampler::TraceIdRatioBased(ratio()?),
        ))),
        value => anyhow::bail!("unsupported OTEL traces sampler: {value}"),
    }
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
            ("EDGER_OTEL_ENABLED", "true"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317"),
            ("OTEL_TRACES_SAMPLER", "always_on"),
        ]);
        let config =
            TracingInitConfig::from_lookup(|name| values.get(name).map(|value| value.to_string()));
        assert_eq!(config.env_filter, "edger_orchestrator=debug");
        assert!(config.otel_enabled);
        assert_eq!(
            config.otel_exporter_otlp_endpoint.as_deref(),
            Some("http://127.0.0.1:4317")
        );
        assert_eq!(config.otel_traces_sampler.as_deref(), Some("always_on"));
    }

    #[test]
    fn tracing_config_has_safe_default_filter_and_otel_off() {
        let config = TracingInitConfig::from_lookup(|_| None);
        assert!(config.env_filter.contains("edger_orchestrator=info"));
        assert!(!config.otel_enabled);
        assert!(config.otel_exporter_otlp_endpoint.is_none());
    }

    #[test]
    fn explicit_false_keeps_otel_off_even_when_endpoint_exists() {
        let config = TracingInitConfig::from_lookup(|name| match name {
            "EDGER_OTEL_ENABLED" => Some("false".into()),
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some("http://collector:4317".into()),
            _ => None,
        });
        assert!(!config.otel_enabled);
    }

    #[cfg(feature = "otel")]
    #[test]
    fn otel_sampler_rejects_invalid_names_and_ratios() {
        let invalid_name = TracingInitConfig::from_lookup(|name| match name {
            "OTEL_TRACES_SAMPLER" => Some("not-a-sampler".into()),
            _ => None,
        });
        assert!(build_sampler(&invalid_name).is_err());

        let invalid_ratio = TracingInitConfig::from_lookup(|name| match name {
            "OTEL_TRACES_SAMPLER" => Some("parentbased_traceidratio".into()),
            "OTEL_TRACES_SAMPLER_ARG" => Some("1.5".into()),
            _ => None,
        });
        assert!(build_sampler(&invalid_ratio).is_err());
    }

    #[cfg(feature = "otel")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn otel_feature_exports_a_batched_trace_to_http_protobuf() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind receiver");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("receiver address")
        );
        let (sender, receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept OTLP request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read timeout");
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 8192];
                loop {
                    let read = stream.read(&mut chunk).expect("read OTLP request");
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..read]);
                    let Some(header_end) =
                        bytes.windows(4).position(|window| window == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&bytes[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(str::trim)
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
                sender.send(bytes).expect("send captured request");
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .expect("write OTLP response");
            }
        });

        let config = TracingInitConfig::from_lookup(|name| match name {
            "EDGER_OTEL_ENABLED" => Some("true".into()),
            "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(endpoint.clone()),
            "OTEL_EXPORTER_OTLP_PROTOCOL" => Some("http/protobuf".into()),
            _ => None,
        });
        let (provider, logger_provider) =
            init_otel_subscriber(&config).expect("initialize OTLP exporter");
        tracing::info_span!(
            "worker.dispatch",
            worker.name = "otel-fixture",
            worker.version = "1.0.0",
            request_id = "otel-request"
        )
        .in_scope(|| tracing::info!(outcome = "ok", "dispatch completed"));
        crate::observability::OperationalStore::default().record(
            crate::observability::OperationalEventInput {
                source: crate::observability::OperationalEventSource::Runtime,
                kind: "request.completed".into(),
                level: crate::observability::OperationalEventLevel::Error,
                namespace: Some("default".into()),
                worker: Some("otel-fixture".into()),
                version: Some("1.0.0".into()),
                process_id: Some("process-1".into()),
                request_id: Some("otel-request".into()),
                trace_id: Some("4bf92f3577b34da6a3ce929d0e0e4736".into()),
                outcome: Some("error".into()),
                status: Some(500),
                duration_ms: Some(12),
                code: Some("WORKER_ERROR".into()),
                message: Some("authorization=secret-value /Users/private/source.ts".into()),
                truncated: Some(false),
                dropped_count: None,
            },
        );
        provider.force_flush().expect("flush OTLP trace");
        logger_provider.force_flush().expect("flush OTLP logs");

        let requests = (0..2)
            .map(|_| {
                receiver
                    .recv_timeout(Duration::from_secs(5))
                    .expect("receiver observed export")
            })
            .collect::<Vec<_>>();
        assert!(requests
            .iter()
            .any(|request| request.starts_with(b"POST /v1/traces HTTP/1.1")));
        assert!(requests
            .iter()
            .any(|request| request.starts_with(b"POST /v1/logs HTTP/1.1")));
        let payload = requests.concat();
        assert!(payload
            .windows(b"otel-fixture".len())
            .any(|part| part == b"otel-fixture"));
        assert!(payload
            .windows(b"request.completed".len())
            .any(|part| part == b"request.completed"));
        assert!(payload
            .windows(b"[redacted]".len())
            .any(|part| part == b"[redacted]"));
        assert!(!payload
            .windows(b"secret-value".len())
            .any(|part| part == b"secret-value"));
        assert!(!payload
            .windows(b"/Users/private".len())
            .any(|part| part == b"/Users/private"));
        provider.shutdown().expect("shutdown provider");
        logger_provider
            .shutdown()
            .expect("shutdown logger provider");
        server.join().expect("receiver thread");
    }

    #[cfg(feature = "otel")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unavailable_collector_does_not_replace_the_local_event_store() {
        use opentelemetry::trace::{Span as _, Tracer as _, TracerProvider as _};
        use opentelemetry_otlp::{Protocol, WithExportConfig};

        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint("http://127.0.0.1:1/v1/traces")
            .with_timeout(std::time::Duration::from_millis(100))
            .build()
            .expect("build unreachable exporter");
        let processor = opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor::builder(
            exporter,
            opentelemetry_sdk::runtime::Tokio,
        )
        .with_batch_config(
            opentelemetry_sdk::trace::BatchConfigBuilder::default()
                .with_max_export_timeout(std::time::Duration::from_millis(100))
                .build(),
        )
        .build();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_span_processor(processor)
            .build();
        provider
            .tracer("collector-failure-test")
            .start("collector unavailable")
            .end();

        let store = crate::observability::OperationalStore::default();
        store.record(crate::observability::OperationalEventInput {
            source: crate::observability::OperationalEventSource::Runtime,
            kind: "dispatch".into(),
            level: crate::observability::OperationalEventLevel::Error,
            namespace: None,
            worker: Some("local-first".into()),
            version: Some("1.0.0".into()),
            process_id: None,
            request_id: None,
            trace_id: None,
            outcome: Some("error".into()),
            status: Some(500),
            duration_ms: Some(1),
            code: Some("WORKER_ERROR".into()),
            message: Some("collector unavailable".into()),
            truncated: Some(false),
            dropped_count: None,
        });

        assert_eq!(store.query(Default::default()).events.len(), 1);
        let flush = provider.force_flush();
        assert!(
            flush.is_err(),
            "the unreachable collector must be observable at flush"
        );
        let _ = provider.shutdown();
    }
}
