# Story 13.01: Contratos machine-readable

**Origin:** `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## Context

Agentes nao devem depender de leitura livre de docs para saber o que podem fazer. Antes do MCP server, o edger precisa de contratos machine-readable para workers, capabilities, Admin API e validacao local.

**Depende de:** Epic 08.02, Epic 10.01

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/` | edit | Reutilizar tipos puros para schemas quando possivel |
| `edger-orchestrator/src/admin.rs` | edit | Expor ou gerar contratos da Admin API se necessario |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar shape e redaction |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar consumo dos contratos |

## Detail

### AS-IS

- Admin API e capabilities sao testadas, mas contrato formal machine-readable ainda e parcial.
- Docs humanas existem, mas nao bastam para automacao segura.

### TO-BE

- Schemas descrevem workers, manifests, capabilities, Admin API, gateway e validacoes locais.
- Campos sensiveis aparecem como referencia/redaction, nao valor.
- Versionamento de schema permite evolucao sem quebrar agents.

### Scope

- **In:** JSON Schema ou OpenAPI quando adequado, versionamento, redaction.
- **Out:** cliente SDK completo, publicacao remota, deploy.

### Critérios de aceite

- [ ] Contratos descrevem pelo menos workers, capabilities e endpoints admin usados pelo MCP.
- [ ] Schemas tem versao e exemplos seguros.
- [ ] Teste prova que resposta nao contem segredos.
- [ ] Docs explicam como o MCP consome esses contratos.

## Tasks

- [ ] Levantar contratos minimos para discovery e authoring.
- [ ] Definir formato versionado dos schemas.
- [ ] Expor ou gerar contratos no boundary adequado.
- [ ] Adicionar teste de shape e redaction.

## Verification

```bash
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

