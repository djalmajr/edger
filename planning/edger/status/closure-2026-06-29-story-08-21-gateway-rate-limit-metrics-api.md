# Closure: Story 08.21 — Gateway rate-limit metrics API

Date: 2026-06-29
Story: `planning/edger/epics/08-valor-buntime/21-gateway-rate-limit-metrics-api.md`
Status: completed

## Files changed

- `edger-orchestrator/src/admin_api.rs`
- `edger-orchestrator/tests/admin_workers_plugins.rs`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/docs/compat-matrix.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/epics/08-valor-buntime/21-gateway-rate-limit-metrics-api.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`
- `planning/edger/status/evidence/story-08-21-runtime.txt`
- `README.md`

## What changed

- Added root-only `GET /api/admin/gateway/rate-limit/metrics`.
- Reused `Extension::diagnostics()` as the source of truth for gateway rate-limit state.
- Added `scope: "local-memory"` to make the current non-distributed behavior explicit.
- Kept bucket listing/reset and mutation APIs out of scope to avoid exposing client bucket keys in this slice.

## Verification

- `cargo test -p edger-orchestrator --test admin_workers_plugins gateway_admin_rate_limit_metrics_api_exposes_local_bucket_summary`
- `cargo test -p edger-orchestrator --test admin_workers_plugins`
- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- `cargo fmt -- --check`
- Runtime launch: `ROOT_API_KEY=test-root PORT=19085 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger`
- Curl root: `GET /api/admin/gateway/rate-limit/metrics` returned `200 {"activeBuckets":0,"enabled":false,"scope":"local-memory"}`
- Curl without key: `GET /api/admin/gateway/rate-limit/metrics` returned `401 UNAUTHORIZED`
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`

All verification commands passed. Planning gate reported 8 epics, 52 stories,
128 refs and 0 missing refs.

## Remaining gaps

- Gateway bucket list/reset APIs remain unimplemented.
- Rate-limit persistence/distribution remains unimplemented.
- Proxy external forwarding, cache, gateway SSE/history and dynamic mutation APIs remain future slices.
