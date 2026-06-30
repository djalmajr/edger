# Story 13.01: Contratos machine-readable

**Origin:** `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## Context

Agentes nao devem depender de leitura livre de docs para saber o que podem fazer. Antes do MCP server, o edger precisa de contratos machine-readable para workers, capabilities, Admin API e validacao local.

**Depende de:** Epic 08.02, Epic 10.01

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-mcp/src/contracts.rs` | create | Declarar contratos versionados consumidos pelas tools MCP |
| `edger-mcp/src/discovery.rs` | create | Transformar inventario real em payloads seguros para agente |
| `edger-mcp/tests/protocol.rs` | create | Provar shape, versionamento e redaction em contrato MCP |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar consumo dos contratos |

## Detail

### AS-IS

- Admin API e capabilities sao testadas, mas contrato formal machine-readable ainda e parcial.
- Docs humanas existem, mas nao bastam para automacao segura.

### TO-BE

- Schemas descrevem workers, manifests, capabilities, Admin API, gateway e validacoes locais.
- Campos sensiveis aparecem como referencia/redaction, nao valor.
- Versionamento de schema permite evolucao sem quebrar agents.
- A primeira entrega usa um contrato JSON versionado embutido no crate `edger-mcp`, cobrindo discovery de workers/capabilities e shape das tools MCP. OpenAPI completo da Admin API fica para uma fatia posterior.

### Scope

- **In:** contrato JSON versionado para tools MCP iniciais, versionamento, redaction.
- **Out:** cliente SDK completo, publicacao remota, deploy.

### Approach

- Criar `edger-mcp` como crate separado, dependente de `edger-core` e `edger-orchestrator`.
- Manter `edger-core` sem I/O; manifest discovery continua no boundary de orquestracao.
- Representar schemas como JSON serializavel e testado, evitando nova dependencia de gerador de schema nesta primeira fatia.
- Reusar `AdminWorkerInfo` como shape de worker quando possivel, com redaction defensiva nos payloads MCP.

### Test-first plan

- **Behavior:** `edger.list_capabilities` deve retornar contrato versionado, tools conhecidas e schemas de entrada sem segredos.
- **First failing test:** chamar handler MCP `tools/call` para `edger.list_capabilities` e assertar `schemaVersion`, `tools`, `resourceTypes` e ausencia de termos sensiveis.
- **Level:** integration test no crate `edger-mcp`, via JSON-RPC in-memory.
- **Low-value tests to avoid:** assertar que objetos sao apenas "defined" ou snapshots grandes do JSON completo.

### Critérios de aceite

- [x] Contratos descrevem pelo menos workers, capabilities e endpoints admin usados pelo MCP.
- [x] Schemas tem versao e exemplos seguros.
- [x] Teste prova que resposta nao contem segredos.
- [x] Docs explicam como o MCP consome esses contratos.

## Tasks

- [x] Levantar contratos minimos para discovery e authoring.
  - Done when: `edger-mcp/src/contracts.rs` lista tools iniciais, resource types e versionamento.
- [x] Definir formato versionado dos schemas.
  - Done when: cada resposta MCP inclui `schemaVersion` e schemas de input das tools.
- [x] Expor ou gerar contratos no boundary adequado.
  - Done when: `edger-mcp` publica API Rust testavel sem adicionar I/O ao `edger-core`.
- [x] Adicionar teste de shape e redaction.
  - Done when: teste falha se capabilities retornarem segredo, token ou env bruto.

## Verification

```bash
cargo test -p edger-mcp
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Status

completed (2026-06-29) - contratos machine-readable v1 vivem em `edger-mcp/src/contracts.rs` com `schemaVersion`, tools MCP iniciais e limites de seguranca; `edger-mcp/tests/protocol.rs` cobre shape e redaction.
