# Closure: Story 08.08 Provas de migração e fechamento da matriz de valor

Date: 2026-06-29
Status: completed

## Delivered

- Added `edger-orchestrator/tests/value_parity.rs` with six representative
  migration/value proofs:
  - SPA `/todos` document, asset and fallback routing;
  - protected worker auth boundary;
  - SQL/KV/queue binding descriptors for a stateful app;
  - shell document routing and iframe worker bypass;
  - gateway CORS preflight without auth bypass;
  - Buntime-derived `base: ""` route-hijack guard.
- Added `workers/value-parity/todos/` as a Browser-friendly visual fixture.
- Updated the value and compatibility matrices with tested/partial/gap status.
- Updated README and roadmap to reflect Epic 08 value proof completion.
- Added `planning/edger/status/evidence/story-08-08-runtime.txt`.
- Added `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`.

## Explicit gaps

- Native cron is not marked tested; scheduler foundation remains pending.
- Gateway proxy/cache/rate-limit persistence remains partial.
- Turso remote/sync, retry/backoff/DLQ depth, persistent extension reload,
  UI administration and marketplace remain future work.
- Embedded `deno_core` remains the production target; Deno CLI bridge is still
  the current JS/TS execution path.

## Verification

```bash
cargo test -p edger-orchestrator --test value_parity
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

Browser validation:

```text
http://127.0.0.1:19084/todos
baseHref=/todos/
rootExists=true
ready=true
```
