# Closure: Story 09.02 Provider local SQLite

Date: 2026-06-29
Story: `planning/edger/epics/09-providers-duraveis-externos/02-provider-local-sqlite.md`
Status: completed

## Delivered

- `LocalSqliteProvider` is now the canonical local durable SQL provider type.
- `LocalTursoProvider` remains as a compatibility alias.
- `edger-ext-turso` crate name and extension inventory name `turso` remain
  stable for existing composition/admin surfaces.
- Internal code and tests now prefer `LocalSqliteProvider`.
- Operation, extension, value-matrix and durable-provider docs clarify that the
  current provider is SQLite local/single-node, not Turso remote/sync.

## Files changed

- `edger-ext-turso/src/lib.rs`
- `edger-ext-turso/tests/local_provider.rs`
- `edger-ext-keyval/tests/keyval_queue.rs`
- `edger-orchestrator/src/bin/edger.rs`
- `edger-orchestrator/tests/registry_providers.rs`
- `edger-orchestrator/tests/state_services.rs`
- `edger-orchestrator/tests/value_parity.rs`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/docs/extensions.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/docs/durable-provider-contract.md`
- `planning/edger/epics/08-valor-buntime/04-servicos-de-estado-turso-kv-queue.md`
- `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`
- `planning/edger/epics/09-providers-duraveis-externos/02-provider-local-sqlite.md`

## Verification

All checks passed:

```bash
cargo test -p edger-ext-turso
cargo test -p edger-orchestrator --test state_services
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining work

- Story 09.03: implement Turso remote/sync as a substitutable provider.
- Story 09.04: select durable providers by configuration in the composition
  root.
- Story 09.05: prove workers, KV/queue and a durable gateway consumer against
  the external provider.
