# Checkpoint: Epic 09 provider reclassification

Date: 2026-06-29
Status: Epic 09 opened; Stories 09.01 and 09.02 completed

## Decision

Turso remote/sync is no longer tracked as an internal implementation gap inside
Epic 08. It is a planned external durable provider over `DurableSqlProvider`.

## Updated artifacts

- `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`
- `planning/edger/epics/09-providers-duraveis-externos/01-contrato-provider-sql-remoto.md`
- `planning/edger/docs/durable-provider-contract.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/roadmap.md`
- `README.md`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/docs/extensions.md`

## Implications

- Epic 08 keeps local SQL/KV/queue value and evidence.
- Epic 09 owns provider naming clarification, Turso remote/sync implementation,
  configurable wiring and durable-provider consumer proofs.
- Story 09.02 made `LocalSqliteProvider` the canonical local provider type and
  kept `LocalTursoProvider` as a legacy alias.
- `edger-core` remains the provider contract boundary.
- `edger-orchestrator` must not depend directly on Turso/libSQL SDKs.

## Verification to keep green

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
