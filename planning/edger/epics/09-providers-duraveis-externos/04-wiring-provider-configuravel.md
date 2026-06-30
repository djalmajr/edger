# Story 09.04: Wiring de provider configurável

**Origin:** `planning/edger/epics/09-providers-duraveis-externos/00-overview.md`

## Context

Depois que provider local e remoto tiverem fronteiras claras, o binário `edger` precisa selecionar qual implementação registrar sem espalhar lógica de transporte pelo pipeline.

**Status:** completed (2026-06-29) - `edger` seleciona o provider SQL durável por `EDGER_DURABLE_SQL_PROVIDER`, mantendo `local` como default seguro e usando `RemoteTursoProvider::from_env()` para `turso-remote`/`turso-sync`.

## Traceability

- `edger-orchestrator/src/bin/edger.rs`
- `edger-orchestrator/Cargo.toml`
- `planning/edger/docs/extensions.md`
- `docs/developers/06-operacao-e-testes.adoc`

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/bin/edger.rs` | alter | Selecionar provider por configuração no composition root |
| `edger-orchestrator/Cargo.toml` | alter | Adicionar dependência do provider remoto apenas no composition root/bin package |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar seleção de provider e fallback local |
| Unit tests do bin `edger` | edit | Provar parsing e seleção remota sem conexão de rede |
| `planning/edger/epics/09-providers-duraveis-externos/00-overview.md` | edit | Atualizar status |

## Detail

### AS-IS

- `edger` seleciona `local` por default.
- `EDGER_DURABLE_SQL_PROVIDER=turso-remote` ou `turso-sync` seleciona `RemoteTursoProvider`.
- O pipeline continua recebendo apenas `Arc<dyn DurableSqlProvider>`.

### TO-BE

- Configuração explícita seleciona provider local ou remoto.
- O pipeline continua resolvendo apenas `provider:durableSql`.
- Falha de configuração retorna erro operacional claro no startup.
- Provas com consumidores reais ficam na Story 09.05; Turso remoto real segue
  como validação opt-in da Story 09.03.

### Scope

- **In:** composition root, docs, teste de seleção.
- **Out:** lógica de query, SDK Turso dentro do orchestrator, alteração de contratos de worker.

### Acceptance criteria

- [x] Provider local continua default seguro para dev/test.
- [x] Provider remoto pode ser selecionado por configuração explícita.
- [x] Config inválida falha no startup com erro claro e sem segredo.
- [x] `ExtensionRegistry` continua recebendo um único `DurableSqlProvider`.

### Dependencies

- 09.02.
- 09.03.

## Tasks

- [x] Definir env/config de seleção.
  - Done when: `EDGER_DURABLE_SQL_PROVIDER` aceita `local`, `turso-remote` e `turso-sync`.
- [x] Implementar wiring no composition root.
  - Done when: `durable_sql_provider_from_env()` retorna `Arc<dyn DurableSqlProvider>` para local ou remoto.
- [x] Testar provider local default.
  - Done when: unit test valida default local e workspace tests continuam verdes.
- [x] Testar erro de configuração inválida.
  - Done when: unit test rejeita valor desconhecido sem cair para fallback silencioso.
- [x] Testar seleção remota.
  - Done when: unit test instancia provider `turso-remote` por env sem conectar ao serviço externo.
- [x] Documentar operação.

## Verification

```bash
cargo test -p edger-orchestrator --bin edger
cargo test -p edger-orchestrator --test registry_providers
cargo test -p edger-orchestrator --test state_services
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
