# Story 11.02 Closure: Cache e rate limit persistente

## Summary

Story 11.02 is complete. `edger-ext-gateway` now supports optional durable
cache and optional persistent rate limiting through `DurableSqlProvider`,
without adding provider-specific types to `edger-core`.

## Delivered

- Added `GatewayCacheConfig` and `GatewayExtension::with_cache_store(...)`.
- Added `GatewayExtension::with_persistent_rate_limit_store(...)`.
- Cache stores only hashed method/host/URI keys, TTL, status, response headers
  and body for public `GET`/`HEAD` status `200` responses.
- Cache skips requests carrying `Authorization`, `Cookie`,
  `Proxy-Authorization` or `x-api-key`.
- Cache exposes `x-edger-cache: hit|miss` and `diagnostics.cache`.
- Persistent rate limit uses fixed-window durable counters and stores only hash
  of the bucket key.
- Local memory rate limit remains the fallback when no persistent store is
  configured.
- The `edger` binary wires gateway cache, history and persistent rate limiting
  from env using the selected durable SQL provider.

## Evidence

- `cargo test -p edger-ext-gateway durable_cache_records_hit_miss_and_redacts_cache_key` passed.
- `cargo test -p edger-ext-gateway durable_cache_ttl_expiry_is_observable` passed.
- `cargo test -p edger-ext-gateway durable_cache_skips_sensitive_requests` passed.
- `cargo test -p edger-ext-gateway persistent_rate_limit_survives_gateway_reconstruction` passed.
- `cargo test -p edger-ext-gateway memory_rate_limit_remains_local_without_provider` passed.
- `cargo test -p edger-orchestrator --test state_services` passed.
- `cargo test -p edger-orchestrator --test admin_workers_plugins gateway_admin_rate_limit_metrics_api_exposes_local_bucket_summary` passed.
- `cargo fmt -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` passed.

`cargo test -p edger-ext-gateway` still fails in this sandbox only at the
pre-existing TCP loopback test
`proxy_rule_forwards_to_local_upstream_without_sensitive_headers`, where
`TcpListener::bind("127.0.0.1:0")` returns `PermissionDenied`.
`cargo test --workspace` stops at the same sandbox-denied test after earlier
workspace crates pass.

## Follow-up

- Continue Story 11.03 for gateway history/SSE.
