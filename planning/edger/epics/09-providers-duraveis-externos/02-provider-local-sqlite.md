# Story 09.02: Provider local SQLite

**Origin:** `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`

## Context

O provider atual vive em `edger-ext-turso`, mas implementa SQLite local por namespace. Essa história separa naming/compatibilidade da decisão de provider remoto para evitar confundir Turso Sync com o fallback local.

## Status

completed (2026-06-29) - `LocalSqliteProvider` é o nome canônico no código; `LocalTursoProvider` permanece como alias legado. O crate `edger-ext-turso` e o nome operacional `turso` foram preservados por compatibilidade.

## Traceability

- `edger-ext-turso/src/lib.rs`
- `edger-ext-turso/tests/local_provider.rs`
- `edger-orchestrator/src/bin/edger.rs`
- `edger-orchestrator/tests/registry_providers.rs`
- `edger-orchestrator/tests/state_services.rs`
- `edger-orchestrator/tests/value_parity.rs`
- `edger-ext-keyval/tests/keyval_queue.rs`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/docs/extensions.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/docs/durable-provider-contract.md`

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-turso/src/lib.rs` | alter | Criar `LocalSqliteProvider` canônico e manter `LocalTursoProvider` como alias |
| `edger-ext-turso/tests/local_provider.rs` | alter | Usar o nome canônico e provar compatibilidade do alias |
| `edger-orchestrator/src/bin/edger.rs` | alter | Usar `LocalSqliteProvider` no composition root |
| `edger-orchestrator/tests/registry_providers.rs` | alter | Usar `LocalSqliteProvider` nos testes de registry |
| `edger-orchestrator/tests/state_services.rs` | alter | Usar `LocalSqliteProvider` nos testes de state services |
| `edger-orchestrator/tests/value_parity.rs` | alter | Usar `LocalSqliteProvider` nas provas de paridade |
| `edger-ext-keyval/tests/keyval_queue.rs` | alter | Usar `LocalSqliteProvider` como backend de teste |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Clarificar provider local SQLite |
| `planning/edger/docs/extensions.md` | edit | Clarificar naming e modo de provider |
| `planning/edger/docs/value-parity-matrix.md` | edit | Registrar nome canônico local e alias legado |
| `planning/edger/docs/durable-provider-contract.md` | edit | Registrar tier local SQLite com nome canônico |
| `planning/edger/epics/09-providers-duraveis-externos/00-overview.md` | edit | Atualizar status |

## Detail

### AS-IS

- `LocalTursoProvider` era o nome público principal de um provider SQLite local.
- O nome histórico "turso" ajuda a preservar o objetivo de compatibilidade, mas pode sugerir Turso remoto/sync onde ele não existe.

### TO-BE

- Naming e documentação deixam explícito que o provider atual é local/single-node.
- `LocalSqliteProvider` vira o tipo canônico para uso novo.
- `LocalTursoProvider` preserva compatibilidade como alias.
- O crate `edger-ext-turso` e o nome operacional `turso` permanecem estáveis nesta fatia.

### Scope

- **In:** clarificação, eventual alias/rename controlado, testes de compatibilidade.
- **Out:** Turso remoto/sync real.

### Approach

| Decisão | Resultado | Motivo |
|---|---|---|
| Rename físico de crate | Não fazer agora | Quebraria paths, Cargo e inventário sem entregar valor adicional nesta história |
| Nome canônico de tipo Rust | `LocalSqliteProvider` | Comunica o backend real e evita confusão com Turso Sync |
| Compatibilidade | `LocalTursoProvider` como alias | Mantém código e documentação históricos funcionando |
| Nome operacional da extensão | Manter `turso` | Evita breaking change na Admin API e no inventário de extensões |

### Test-first plan

- **Comportamento a provar:** código novo usa `LocalSqliteProvider`; código antigo ainda compila e executa via `LocalTursoProvider`.
- **Primeiro teste que falharia:** `legacy_local_turso_alias_keeps_sql_provider_compatibility` falharia se o alias fosse removido ou perdesse métodos do provider.
- **Nível:** testes de provider local + testes de registry/state/value parity existentes.
- **Evitar:** teste que só verifica que o tipo existe sem executar SQL.

### Acceptance criteria

- [x] Usuário lendo docs entende que o provider atual é SQLite local.
- [x] Testes existentes de state services e keyval continuam verdes.
- [x] Qualquer mudança de nome preserva registro `provider:durableSql`.
- [x] Matriz não descreve o provider local como Turso remoto/sync.

### Dependencies

- 09.01.

## Tasks

- [x] Decidir entre alias documentado e rename físico.
  - Done when: história registra `LocalSqliteProvider` canônico, `LocalTursoProvider` alias e crate/nome operacional preservados.
- [x] Atualizar docs de provider local.
  - Done when: operação, extensões, matriz e contrato durável deixam claro o backend SQLite local.
- [x] Cobrir compatibilidade se houver rename.
  - Done when: teste executa SQL usando `LocalTursoProvider` alias e consumidores internos usam `LocalSqliteProvider`.
- [x] Rodar gates Rust e planejamento.
  - Done when: comandos da seção Verification passam.

## Verification

```bash
cargo test -p edger-ext-turso
cargo test -p edger-orchestrator --test state_services
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
