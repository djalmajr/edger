# Closure: Story 09.04 Wiring de provider configurável

**Date:** 2026-06-29
**Origin:** `planning/edger/epics/09-providers-duraveis-externos/04-wiring-provider-configuravel.md`

## Files changed

- `edger-orchestrator/src/bin/edger.rs`
- `edger-orchestrator/Cargo.toml`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`
- `planning/edger/epics/09-providers-duraveis-externos/04-wiring-provider-configuravel.md`

## Outcome

- `EDGER_DURABLE_SQL_PROVIDER` selects `local`, `turso-remote` or `turso-sync`.
- Local SQLite remains the default provider for dev/test.
- Remote/sync selection instantiates `RemoteTursoProvider::from_env()` and still
  registers only `Arc<dyn DurableSqlProvider>` in the registry.
- Unknown provider names fail startup instead of silently falling back to local.

## Verification

- `cargo test -p edger-orchestrator --bin edger`
- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- `cargo fmt -- --check`
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`
