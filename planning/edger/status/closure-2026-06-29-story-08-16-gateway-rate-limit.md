# Closure: Story 08.16 gateway rate limit em memória

Date: 2026-06-29

Story 08.16 delivered local in-memory gateway rate limiting using per-client token buckets. The slice reduces the `Gateway/proxy rules` gap with a practical protection before worker/redirect execution, while keeping persistence, distribution, admin APIs and UI outside this delivery.

## Delivered
- `GatewayRateLimitConfig` with capacity and window configuration.
- Opt-in `GatewayExtension::with_rate_limit`.
- Per-client bucket keyed by `X-Forwarded-For`, `X-Real-IP`, or `unknown`.
- `429` short-circuit with `x-ratelimit-limit`, `x-ratelimit-remaining: 0`, and `retry-after`.
- CORS preflight remains a 204 response and does not consume rate-limit tokens.
- Rate limit runs before redirect rules.
- Tests cover limit exhaustion, independent clients, preflight behavior and redirect ordering.

## Explicitly not delivered
- Persisted or distributed buckets.
- Runtime/admin API for metrics, bucket listing or reset.
- Rate-limit keying by authenticated user identity.
- Regex `excludePaths`.
- Proxy forwarding, cache or vhost routing.

## Verification
```bash
cargo test -p edger-ext-gateway --test gateway_middleware
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining follow-up
- Continue remaining value gaps from `planning/edger/docs/value-parity-matrix.md`: proxy external forwarding, persistent/distributed gateway state, cache, cron execution, Turso remote/sync, retry/DLQ depth, persistent extension reload and deeper logging/performance harnesses.
