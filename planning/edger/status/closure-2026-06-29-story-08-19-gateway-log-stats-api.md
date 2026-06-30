# Closure: Story 08.19 stats agregados dos logs do gateway

Date: 2026-06-29

Story 08.19 delivered a dedicated root-only Admin API endpoint for aggregated
gateway log stats. The slice reduces the Buntime `/logs/stats` value gap while
keeping Edger's source of truth in local extension diagnostics.

## Delivered
- `GET /api/admin/gateway/logs/stats` endpoint.
- Root authentication on the endpoint.
- Aggregates over retained `recentDecisions`:
  - `total`
  - `rateLimited`
  - `byStatus`
  - `byDecision`
  - `withoutStatus`
- Explicit duration metadata with `duration.tracked=false` and `avgMs=null`.
- Tests proving auth, aggregation shape and secret hygiene.
- Planning matrix, compatibility matrix and developer docs updated with the new
  read-only stats surface and remaining gaps.

## Explicitly not delivered
- Real average duration measurement.
- SSE stream or persisted log history.
- Log delete/clear operations.
- Rate-limit bucket reset APIs.
- Dynamic gateway config mutation APIs.
- External proxy forwarding, cache or distributed rate-limit storage.

## Verification
```bash
cargo test -p edger-orchestrator --test admin_workers_plugins gateway_admin_gateway_log_stats_api_aggregates_recent_decisions -- --exact
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining follow-up
- Continue remaining value gaps from `planning/edger/docs/value-parity-matrix.md`:
  proxy external forwarding, cache, persistent/distributed gateway state,
  real duration metrics, gateway-specific SSE/history/mutation APIs, cron
  execution, Turso remote/sync, retry/DLQ depth and persistent extension reload.
