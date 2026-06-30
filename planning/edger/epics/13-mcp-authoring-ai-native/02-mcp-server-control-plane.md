# Story 13.02: MCP server control plane

**Origin:** `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## Context

O MCP server e a primeira interface AI-native funcional do edger. Ele deve expor ferramentas locais pequenas e seguras, com descoberta e dry-run, em vez de entregar um wrapper generico que execute comandos arbitrarios.

**Depende de:** Story 13.01

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/` | edit | Adicionar crate ou binario MCP se essa for a fronteira escolhida |
| `edger-orchestrator/src/bin/` | edit | Integrar binario local somente se fizer sentido arquitetural |
| `edger-orchestrator/tests/` | edit | Provar discovery e auth local |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar execucao local do MCP |

## Detail

### AS-IS

- Nao existe MCP server do edger.
- Admin API e dados operacionais existem, mas agentes precisam invocar o runtime diretamente.

### TO-BE

- MCP server local inicia sem rede externa e lista tools.
- Tools iniciais: list_workers, list_capabilities, inspect_worker, validate_local.
- Auth e workspace root sao configuraveis localmente.
- Respostas sao pequenas, estruturadas e sem segredos.

### Scope

- **In:** servidor MCP local, tools de discovery, redaction, testes.
- **Out:** deploy remoto, execucao arbitraria de shell, editor completo.

### Critérios de aceite

- [ ] MCP server inicia localmente e responde initialize/list tools.
- [ ] Tools de discovery retornam JSON estruturado.
- [ ] Respostas nao incluem tokens, headers sensiveis ou env bruto.
- [ ] Teste local cobre pelo menos uma chamada de discovery.

## Tasks

- [ ] Escolher fronteira de crate/binario MCP sem contaminar core.
- [ ] Implementar inicializacao e listagem de tools.
- [ ] Implementar discovery de workers/capabilities.
- [ ] Adicionar testes e docs de execucao local.

## Verification

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

