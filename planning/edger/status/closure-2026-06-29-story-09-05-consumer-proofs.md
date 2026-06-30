# Closure: Story 09.05 Provas com consumidores duráveis

**Date:** 2026-06-29
**Origin:** `planning/edger/epics/09-providers-duraveis-externos/05-provas-consumidores-duraveis.md`

## Files changed

- `edger-ext-gateway/src/lib.rs`
- `edger-ext-gateway/tests/gateway_middleware.rs`
- `edger-ext-gateway/Cargo.toml`
- `edger-ext-keyval/Cargo.toml`
- `edger-ext-keyval/tests/external_provider_contract.rs`
- `edger-orchestrator/tests/state_services.rs`
- `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`
- `planning/edger/epics/09-providers-duraveis-externos/05-provas-consumidores-duraveis.md`
- `planning/edger/docs/durable-provider-contract.md`
- `planning/edger/docs/extensions.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/roadmap.md`
- `docs/developers/06-operacao-e-testes.adoc`

## Outcome

- Worker binding resolution was proven with `RemoteTursoProvider` registered as
  the durable SQL provider.
- KV/queue public contracts were proven against the external provider in a
  dedicated integration test binary, avoiding local SQLite/libSQL runtime
  collision in the same test executable.
- Gateway now supports optional persistent decision history through
  `GatewayExtension::with_history_store(Arc<dyn DurableSqlProvider>, namespace)`.
- Gateway owns the `gateway_decisions` schema and persists only operational
  decision fields, not request bodies, auth headers or query strings.
- Diagnostics expose `history.persistent.enabled` and the persisted decision
  count when a history store is configured.
- Real Turso remote/sync service validation remains opt-in under Story 09.03
  because this session had no configured remote target or credentials.

## Verification

Focused commands:

```bash
cargo test -p edger-ext-gateway
cargo test -p edger-ext-keyval
cargo test -p edger-orchestrator --test state_services
cargo fmt
```

Full gate commands:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
git diff --check
```

Result: all commands passed on 2026-06-29. `bun test` was skipped by
`run-gates.sh` because no root JS/TS test suite exists after the Bun adapter
removal.
