# Checkpoint: Story 09.03 Provider Turso remoto/sync

**Date:** 2026-06-29
**Origin:** `planning/edger/epics/09-providers-duraveis-externos/03-provider-turso-remoto-sync.md`

## Summary

Criado `edger-ext-turso-remote` como provider substituível sobre
`DurableSqlProvider`, separado do core e do orchestrator. A implementação usa
`libsql` em modo `remote` e `remote_replica`, com configuração por
`EDGER_TURSO_*`, diagnostics sem URL/token e testes always-on para contrato SQL,
isolamento de namespace e redaction.

## Evidence

- `cargo test -p edger-ext-turso-remote` passou com 3 unit tests e 5 integration tests.
- O teste remoto real `opt_in_remote_turso_contract_uses_real_configured_target`
  é opt-in e retorna cedo quando `EDGER_TURSO_TEST_URL` e
  `EDGER_TURSO_TEST_AUTH_TOKEN` não estão definidos.

## Remaining

- Provar erro de credencial/timeout contra Turso real sem vazar segredo.
- Story 09.04: selecionar provider remoto no composition root.
- Story 09.05: provar worker, KV/queue e uma capacidade operacional de gateway
  contra o provider externo configurado.
