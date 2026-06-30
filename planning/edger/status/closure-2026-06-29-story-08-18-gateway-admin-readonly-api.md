# Closure: Story 08.18 API admin read-only do gateway

Date: 2026-06-29

Story 08.18 delivered a dedicated root-only Admin API surface for gateway
operations. The slice intentionally uses Edger's local extension diagnostics as
the source of truth instead of copying Buntime's plugin-specific gateway API,
SSE stream or persisted history.

## Delivered
- Safe gateway `config` block in `GatewayExtension` diagnostics.
- `GET /api/admin/gateway/stats` returning the full local gateway diagnostic
  snapshot.
- `GET /api/admin/gateway/config` returning read-only CORS, redirect rule count
  and rate-limit configuration.
- `GET /api/admin/gateway/logs` returning recent gateway decisions.
- Log filters for `limit`, `rateLimited`, `status` and `decision`.
- Root authentication on every gateway Admin API endpoint.
- Tests proving stats/config/logs behavior, filters and secret hygiene.
- Planning matrix, compatibility matrix and developer docs updated with the new
  read-only surface and remaining gaps.

## Explicitly not delivered
- Public or plugin-specific `/gateway/api/*` endpoints.
- SSE stream, persisted log history or average-duration analytics.
- Log delete/clear operations.
- Rate-limit bucket reset APIs.
- Dynamic gateway config mutation APIs.
- External proxy forwarding, cache or distributed rate-limit storage.

## Verification
```bash
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
