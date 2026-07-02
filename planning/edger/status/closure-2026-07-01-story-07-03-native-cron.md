# Story 07.03 Closure: Cron nativo

## Summary

Story 07.03 is complete. The orchestrator now starts a native Tokio cron
scheduler from enabled `manifest.cron[]` jobs, dispatches internal HTTP through
the local Axum pipeline, and exposes cron execution counters in `/metrics`.

## Delivered

- Added `edger-orchestrator/src/cron.rs` with schedule validation, job
  registration, Tokio task lifecycle, internal dispatch and shutdown.
- Added manifest-index collection for enabled workers with cron jobs.
- Wired the `edger` binary to start cron after manifest loading and shut it down
  before extension/pool shutdown.
- Cron dispatch uses `x-edger-internal: true`, root bearer credentials from
  `ROOT_API_KEY`, and `x-request-id: cron-...` at the pipeline boundary; root
  credentials are stripped before worker serialization.
- Added Prometheus counters `edger_cron_executions_total` and
  `edger_cron_failures_total`.
- Added `workers/cron-worker` as a concrete cron fixture.
- Documented supported schedule v1, reload behavior for re-enabled workers, and
  operational env/metrics behavior.

## Evidence

- `cargo test -p edger-orchestrator --test cron_scheduler_test` passed.
- `cargo test -p edger-orchestrator cron` passed.
- `cargo test -p edger-core` passed.
- `cargo fmt -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`
  passed.

`cargo test --workspace` still stops at the pre-existing gateway TCP loopback
test because the sandbox denies `TcpListener::bind("127.0.0.1:0")`.

## Follow-up

- Continue the active roadmap sequence with Story 07.06 Observabilidade OTEL.
- Full cron grammar, timezones and distributed leader election remain outside
  this v1 slice.
