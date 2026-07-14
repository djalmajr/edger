# Story 09.01: Contrato de provider SQL remoto

**Origin:** `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`

## Context

O edger já possui `DurableSqlProvider`, mas ainda não há uma especificação explícita para providers remotos/sync. Antes de implementar Turso remoto, é preciso definir o que um provider remoto deve provar sem alterar o core para detalhes de transporte.

## Status

completed (2026-06-29) - contrato operacional inicial documentado em `planning/edger/docs/durable-provider-contract.md`; implementação Turso remoto/sync, wiring configurável e provas com consumidores seguem nas stories 09.03-09.05.

## Traceability

- `crates/edger-core/src/bindings.rs`
- `edger-ext-turso/src/lib.rs`
- `planning/edger/epics/08-valor-buntime/04-servicos-de-estado-turso-kv-queue.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/docs/durable-provider-contract.md`

## Files

| Path | Action | Reason |
|---|---|---|
| `planning/edger/docs/durable-provider-contract.md` | create | Registrar contrato operacional de providers SQL remotos |
| `planning/edger/docs/extensions.md` | edit | Referenciar requisitos de provider remoto |
| `planning/edger/docs/value-parity-matrix.md` | edit | Vincular provider externo planejado ao valor de SQL durável |
| `planning/edger/epics/09-providers-duraveis-externos/00-overview.md` | edit | Atualizar status da história |

## Detail

### AS-IS

- `DurableSqlProvider` cobre `execute`, `query` e `execute_batch`.
- O provider atual usa SQLite local e prova namespace/durabilidade local.
- A matriz de valor agora aponta Epic 09 como owner de Turso remoto/sync, mas a implementação ainda não existe.

### TO-BE

- Documento de contrato define requisitos para providers remotos:
  - configuração e credenciais fora de manifests de worker;
  - health/readiness do provider;
  - isolamento por namespace;
  - sem vazamento de URL/token em logs, diagnostics ou binding headers;
  - sem alteração no contrato de worker consumidor;
  - comportamento esperado para indisponibilidade, timeout e retry.

### Scope

- **In:** contrato, critérios de aceitação e docs de extensão.
- **Out:** implementação Turso remoto/sync, rename de crates, wiring runtime.

### Acceptance criteria

- [x] Contrato remoto documentado sem exigir SDK específico em `edger-core`.
- [x] Matriz aponta Epic 09 como owner do provider remoto/sync planejado.
- [x] Docs deixam claro que o provider local atual continua válido para single-node.
- [x] Critérios de health, secrets e namespace são verificáveis em histórias futuras.

### Dependencies

- Epic 08.04.

## Tasks

- [x] Documentar contrato operacional de provider remoto.
- [x] Atualizar docs de extensões com a fronteira `DurableSqlProvider`.
- [x] Atualizar matriz de valor para referenciar o Epic 09.
- [x] Validar planejamento com `run-gates.sh`.

## Verification

```bash
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
