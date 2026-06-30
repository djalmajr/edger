# Closure: Story 08.15 gateway redirect rules

Date: 2026-06-29

Story 08.15 delivered a small gateway rules slice: deterministic prefix redirects in `edger-ext-gateway`, with suffix/query preservation and CORS preflight still taking precedence. The slice reduces the `Gateway/proxy rules` gap without claiming full reverse proxy, cache, rate-limit or dynamic persisted rule management.

## Delivered
- `GatewayRedirectRule` with normalized prefix matching and default `308`.
- Ordered `GatewayExtension::with_redirect_rules` short-circuit behavior.
- Segment-aware matching so `/api` does not match `/apiary`.
- Redirect `Location` preserves path suffix and query string.
- `axum_to_serialized` preserves `path?query`.
- Worker dispatch keeps query string after base-path rewrite.
- Tests cover redirect, CORS preflight precedence, wire query preservation and pipeline query rewrite.

## Explicitly not delivered
- Upstream HTTP proxy/forwarding.
- Regex rewrite or dynamic REST API for redirect rules.
- Durable storage for dynamic gateway/proxy rules.
- Cache or rate-limit persistence.
- Vhost/host routing.

## Verification
```bash
cargo test -p edger-ext-gateway --test gateway_middleware
cargo test -p edger-orchestrator wire::tests::roundtrip_preserves_method_path_query_headers_body
cargo test -p edger-orchestrator pipeline::tests::worker_request_preserves_query_after_path_rewrite
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining follow-up
- Continue remaining value gaps from `planning/edger/docs/value-parity-matrix.md`: proxy external forwarding, cache/rate-limit persistence, cron execution, Turso remote/sync, retry/DLQ depth, persistent extension reload and deeper logging/performance harnesses.
