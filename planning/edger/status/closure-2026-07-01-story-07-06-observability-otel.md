# Story 07.06 Closure: Observabilidade OTEL

## Summary

Story 07.06 is complete for the observability v1 boundary. The runtime now
propagates generated request IDs through the worker pipeline, records HTTP
Prometheus metrics, emits structured worker dispatch logs, instruments pool and
isolate dispatch, and initializes tracing from environment configuration.

## Delivered

- Added `edger-orchestrator/src/tracing_init.rs` with `EDGER_LOG` precedence,
  `RUST_LOG` fallback, safe default filters and non-fatal `OTEL_*` env parsing.
- Wired the `edger` binary to use the centralized tracing initializer.
- Added HTTP request counters and last-observed duration to `/metrics`.
- Fixed generated request ID propagation by inserting the generated header into
  the request before dispatch and echoing the same value in the response.
- Added structured worker dispatch logs with request ID, worker name, version
  and namespace while avoiding headers/body/secrets.
- Added `pool.fetch` and `isolate.execute` spans in the worker pool dispatch
  path.
- Updated operator docs, Epic 07 status, pendency tracking, compat matrix and
  value parity matrix.

## Evidence

- `cargo test -p edger-orchestrator --test metrics_endpoint` passed.
- `cargo test -p edger-orchestrator tracing_init` passed.
- `cargo test -p edger-worker --test metrics_ephemeral` passed.
- `cargo fmt -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`
  passed.
- `planning/edger/status/evidence/story-07-06-runtime.txt` records the covered
  behavior and workspace test caveat.

## Follow-up

- Link `tracing-opentelemetry` and an OTLP exporter layer when dependency
  updates are allowed.
- Add detailed spawn/retire spans and performance-oriented duration histograms
  in the hardening/perf slice.
- Continue the active roadmap sequence with Story 07.05 Wasm execution.
