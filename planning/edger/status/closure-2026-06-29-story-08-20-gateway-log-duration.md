# Closure: Story 08.20 duração real nos logs do gateway

Date: 2026-06-29

Story 08.20 delivered local duration tracking for gateway recent decisions and
average duration in the read-only gateway log stats endpoint. The slice closes
the Buntime `/logs/stats` average-duration value gap without adding SSE,
history persistence or mutable gateway operations.

## Delivered
- `durationMs` on gateway short-circuit decisions.
- Final status and `durationMs` update for `continue` decisions after
  `on_response`.
- `/api/admin/gateway/logs/stats` duration aggregate:
  - `tracked`
  - `samples`
  - `avgMs`
- Tests proving duration tracking, final response status, aggregation and secret
  hygiene.
- Planning matrix, compatibility matrix and developer docs updated with the new
  duration surface and remaining gaps.

## Explicitly not delivered
- Histograms or p95/p99.
- SSE stream or persisted log history.
- Log delete/clear operations.
- Rate-limit bucket reset APIs.
- Dynamic gateway config mutation APIs.
- External proxy forwarding, cache or distributed rate-limit storage.

## Verification
```bash
cargo test -p edger-ext-gateway --test gateway_middleware diagnostics_records_response_duration_without_sensitive_data -- --exact
cargo test -p edger-orchestrator --test admin_workers_plugins gateway_admin_gateway_log_stats_api_aggregates_recent_decisions -- --exact
cargo test -p edger-ext-gateway --test gateway_middleware
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining follow-up
- Continue remaining value gaps from `planning/edger/docs/value-parity-matrix.md`:
  proxy external forwarding, cache, persistent/distributed gateway state,
  gateway-specific SSE/history/mutation APIs, cron execution, Turso remote/sync,
  retry/DLQ depth and persistent extension reload.
