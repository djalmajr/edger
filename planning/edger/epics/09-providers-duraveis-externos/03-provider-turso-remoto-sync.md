# Story 09.03: Provider Turso remoto/sync

> OBSOLETO desde o Epic 17.C: providers de estado, `DurableSqlProvider` e
> service bindings foram removidos do runtime. Esta story não deve ser retomada
> no backlog atual.

**Origin:** `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`

## Context

Turso remoto/sync é útil para estado compartilhado entre processos/pods, mas deve ser uma implementação substituível do contrato de SQL durável. Esta história entrega o provider remoto/sync sem acoplar workers ou orchestrator ao transporte.

**Status:** obsolete/cancelled (2026-07-03) - Epic 17.C removeu `DurableSqlProvider`, providers de estado e bindings do runtime. Esta história permanece como registro histórico da fase pré-Epic 17 e não deve ser retomada no backlog atual do edger.

## Traceability

- `crates/edger-core/src/bindings.rs`
- `planning/edger/docs/durable-provider-contract.md`
- `planning/edger/docs/value-parity-matrix.md`
- `edger-ext-turso-remote/src/lib.rs`

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-turso-remote/` | create | Implementar `DurableSqlProvider` com Turso/libSQL remoto/sync em crate separado |
| `edger-ext-turso-remote/tests/remote_provider.rs` | create | Provar execute/query, namespace, capability, redaction e smoke remoto opt-in |
| `Cargo.toml` / `Cargo.lock` | edit | Registrar crate e dependência `libsql` no workspace |
| Docs de configuração | create/edit | Documentar env vars, secrets e modos remote/sync |
| `planning/edger/epics/09-providers-duraveis-externos/00-overview.md` | edit | Atualizar status |

## Detail

### AS-IS

- Existe `edger-ext-turso-remote`, mas ele ainda não é selecionado pelo binário `edger`.
- Testes always-on usam libSQL local configurado; testes contra Turso remoto real são opt-in por env.
- O edger usa provider local para demos/testes.

### TO-BE

- Provider remoto/sync implementa `DurableSqlProvider`.
- Configuração aceita URL/token por ambiente seguro.
- Diagnostics e erros não expõem credenciais.
- Testes cobrem fluxo feliz e falhas operacionais representativas.
- Story 09.04 seleciona o provider no composition root.
- Story 09.05 prova consumidores reais contra o provider externo.

### Scope

- **In:** provider remoto/sync, contrato SQL, testes, docs.
- **Out:** reescrever KV/queue/gateway; eles devem continuar falando com o contrato.

### Acceptance criteria

- [x] Provider remoto/sync compila separado do core/orchestrator.
- [x] `execute`, `query` e `execute_batch` funcionam contra libSQL configurado em teste always-on.
- [x] Namespace permanece isolado por configuração explícita.
- [ ] Falhas de credencial/timeout retornam erro operacional tipado contra um alvo Turso real.
- [x] Logs e diagnostics não expõem token ou URL sensível nos testes locais.

### Dependencies

- 09.01.

## Tasks

- [x] Escolher localização do provider externo.
  - Done when: provider vive em `edger-ext-turso-remote`, sem dependência de `edger-orchestrator`.
- [x] Implementar `DurableSqlProvider`.
  - Done when: `RemoteTursoProvider` implementa `execute`, `query` e `execute_batch` usando `libsql::Builder`.
- [x] Adicionar configuração segura.
  - Done when: `from_env()` lê `EDGER_TURSO_*`, exige token para remoto e não serializa segredos em diagnostics.
- [x] Cobrir testes de contrato locais.
  - Done when: `cargo test -p edger-ext-turso-remote` cobre SQL, namespace, capability e redaction.
- [ ] Cobrir falhas remotas reais.
  - Done when: teste opt-in com Turso real registrar erro de credencial/timeout sem vazar segredo.
- [ ] Documentar operação.

## Verification

```bash
cargo test -p edger-ext-turso-remote
cargo clippy -p edger-ext-turso-remote -- -D warnings
cargo fmt -- --check
```

Quando depender de Turso remoto real, separar testes locais de testes opt-in por env.

### Opt-in remoto

```bash
EDGER_TURSO_TEST_URL=libsql://... \
EDGER_TURSO_TEST_AUTH_TOKEN=... \
cargo test -p edger-ext-turso-remote opt_in_remote_turso_contract_uses_real_configured_target -- --exact
```

Para sync/remote replica, adicionar `EDGER_TURSO_TEST_LOCAL_PATH=/tmp/edger-turso-replica.db`.
