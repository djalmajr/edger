# Story 09.05: Provas com consumidores duráveis

**Origin:** `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`

## Context

Provider remoto/sync só entrega valor se consumidores reais do edger usarem o mesmo contrato sem mudanças específicas. Esta história fecha a evidência com worker, KV/queue e gateway history usando provider durável externo.

**Status:** completed (2026-06-29) - workers, KV/queue e gateway history foram provados contra `edger-ext-turso-remote` usando libSQL configurado em teste always-on. A prova contra um serviço Turso remoto real permanece opt-in na Story 09.03 por depender de credenciais/alvo externo.

## Traceability

- `crates/edger-orchestrator/tests/state_services.rs`
- `edger-ext-keyval/tests/keyval_queue.rs`
- `edger-ext-gateway/src/lib.rs`
- `planning/edger/docs/value-parity-matrix.md`

## Files

| Path | Action | Reason |
|---|---|---|
| Tests de state services | edit/create | Provar worker usando provider selecionado |
| Tests KV/queue | edit/create | Provar backend SQL remoto/sync sem mudar API KV/queue |
| Gateway history/rate-limit persistence | create/edit | Provar um consumidor operacional do provider durável |
| Evidências em `planning/edger/status/evidence/` | create | Registrar comandos e resultados |

## Detail

### AS-IS

- Workers recebiam descritores de binding com provider local.
- KV/queue tinham cobertura de contrato usando SQL local como backend.
- Gateway mantinha logs/rate-limit em memória e não tinha consumidor durável.

### TO-BE

- Pelo menos um worker usa provider durável externo em fluxo observável.
- KV/queue preservam API e usam provider durável externo quando configurado.
- Gateway persiste histórico operacional por provider durável, sem acoplar gateway a Turso.

### Scope

- **In:** provas integradas com consumidores reais.
- **Out:** UI administrativa, marketplace, garantias exactly-once universais.

### Acceptance criteria

- [x] Worker com `durableSql` mantém comportamento ao trocar provider local por externo.
- [x] KV/queue usam o provider configurado sem alterar contrato público.
- [x] Gateway persiste uma capacidade selecionada via contrato durável.
- [x] Evidência versionada mostra setup, comandos e respostas.
- [x] Matriz do Epic 8 remove a lacuna de consumidores usando provider externo; prova contra Turso remoto real continua opt-in na 09.03.

### Dependencies

- 09.04.

## Tasks

- [x] Escolher fluxo mínimo de worker.
  - Done when: `crates/edger-orchestrator/tests/state_services.rs` prova binding descriptors com registry usando `RemoteTursoProvider`.
- [x] Escolher fluxo mínimo de KV/queue.
  - Done when: `edger-ext-keyval/tests/external_provider_contract.rs` cobre `set/get/delete` e `enqueue/dequeue/ack` sobre provider externo.
- [x] Escolher uma capacidade persistente do gateway.
  - Done when: `edger-ext-gateway/tests/gateway_middleware.rs` cobre `GatewayExtension::with_history_store` persistindo `gateway_decisions`.
- [x] Executar evidência local/opt-in para provider externo.
  - Done when: testes always-on usam `RemoteTursoProvider::new_local_for_tests`; teste real Turso permanece opt-in em 09.03.
- [x] Atualizar matriz e checkpoint.
  - Done when: matriz e closure da Story 09.05 referenciam os testes de consumidores.

## Verification

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

Testes que exigirem serviço Turso remoto real devem ser opt-in e registrar quando foram pulados por falta de credenciais.
