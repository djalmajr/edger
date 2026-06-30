# Closure: Story 08.17 diagnóstico operacional do gateway

Date: 2026-06-29

Story 08.17 delivered local gateway diagnostics through the existing root-only
extension inventory. The slice reduces the Buntime gateway observability gap by
making gateway decisions inspectable without copying Buntime's dedicated UI,
SSE, persistence or plugin-specific API surface.

## Delivered
- Optional `Extension::diagnostics()` contract in `edger-core`.
- Optional `diagnostics` payload in `AdminExtensionInfo`.
- Registry aggregation of extension diagnostics without downcasting to concrete
  extension types.
- `GatewayExtension` counters for `continue`, `preflight`, `redirect` and
  `rate_limited` decisions.
- Ring buffer with the latest 100 gateway decisions.
- Diagnostic entries with request id, method, path, client, decision, status and
  `rateLimited`.
- Admin API inventory exposes gateway diagnostics to root operators.
- Tests verify counters, recent decision retention and secret hygiene.

## Explicitly not delivered
- Gateway-specific `/gateway/api/*` endpoints.
- SSE stream or persisted metrics history.
- Log filters, average duration or response logging for worker responses.
- External proxy forwarding, cache or vhost routing.
- Persisted/distributed rate-limit state.

## Verification
```bash
cargo test -p edger-ext-gateway --test gateway_middleware
cargo test -p edger-orchestrator --test admin_workers_plugins root_lists_registered_extensions_and_auth_provider -- --exact
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining follow-up
- Continue remaining value gaps from `planning/edger/docs/value-parity-matrix.md`:
  proxy external forwarding, cache, persistent/distributed gateway state,
  gateway-specific SSE/history/config APIs, cron execution, Turso remote/sync,
  retry/DLQ depth and persistent extension reload.
