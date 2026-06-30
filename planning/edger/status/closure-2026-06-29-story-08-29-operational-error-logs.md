# Closure: Story 08.29 operational error logs

Date: 2026-06-29
Scope: per-story closure inside Epic 08

## Plan status

- [x] Structured `edger.operational` warning logs for operational failures.
- [x] Logs include `surface`, `request_id`, `status` and `code`.
- [x] Logs avoid headers, bodies, raw tokens and potentially sensitive error messages.
- [x] Tests capture real tracing output from request flows.
- [x] Value-parity matrix marks `Logging e warnings acionáveis` as `tested`.

## Result

Story 08.29 delivered a small `operational_log` helper in the orchestrator and wired it into Admin API error responses and pipeline-level failures. The captured logs are actionable for local operation because they expose the failing surface, request correlation ID, HTTP status and typed error code without serializing request data.

## Scope drift review

- Intentional expansion: the initial target emphasized Admin API errors; implementation also logs pipeline failures. Rationale: the matrix row is global operational logging, and pipeline body/header-limit failures are one of the highest-value local runtime failures to correlate by request ID.
- No external observability stack was added.
- No SSE, retention store, UI, OTel exporter or gateway mutation API was introduced.

## Files changed

- `edger-orchestrator/src/operational_log.rs`
- `edger-orchestrator/src/lib.rs`
- `edger-orchestrator/src/admin_api.rs`
- `edger-orchestrator/src/pipeline.rs`
- `edger-orchestrator/tests/security_operational.rs`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/epics/08-valor-buntime/29-operational-error-logs.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`
- `planning/edger/status/evidence/story-08-29-runtime.txt`

## Verification

- `cargo test -p edger-orchestrator --test security_operational` PASS
- `cargo test --workspace` PASS
- `cargo clippy --workspace -- -D warnings` PASS
- `cargo fmt -- --check` PASS
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` PASS

## Remaining risks

- Historical retention, SSE/UI and external observability remain out of scope for this story.
- Gateway-specific long-lived history and distributed logging remain part of later gateway/provider work.

## Handoff

Continue Epic 08 through the remaining `must partial` matrix rows: extension/plugin APIs, durable SQL provider boundaries and gateway/proxy rules.
