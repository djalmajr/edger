# Story 13.04: Validacao local de worker

**Origin:** `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## Context

Authoring sem validacao gera risco. O MCP deve conseguir rodar validacao local do worker, capturar evidencia e reportar falhas sem deploy remoto.

**Depende de:** Story 13.03

## Files

| Path | Action | Reason |
|---|---|---|
| `planning/edger/status/evidence/` | edit | Registrar evidencia local de validacao |
| `edger-mcp/src/discovery.rs` | create | Implementar validacao local in-process |
| `edger-mcp/tests/protocol.rs` | create | Provar sucesso e falha de validacao |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar comando local e limites |

## Detail

### AS-IS

- Gated workflow existe para repo inteiro.
- Nao ha validacao local especifica invocavel por MCP para worker modificado.

### TO-BE

- Tool de validacao roda checks locais relevantes: manifest/autodiscovery, dispatch, security preflight e gates definidos.
- Evidencia contem comando, status, resumo e paths de logs.
- Falhas retornam diagnostico suficiente para agente corrigir.
- A primeira versao de `edger.validate_local` valida descoberta/parsing de manifests sem executar shell arbitrario. Gates completos continuam fora da tool e documentados no runbook.

### Scope

- **In:** validacao local, evidencia, diagnostico estruturado.
- **Out:** deploy remoto, testes de carga longos, execucao de comandos arbitrarios.

### Critérios de aceite

- [x] Validacao falha para worker invalido com mensagem estruturada.
- [x] Validacao passa para worker criado pela Story 13.03.
- [x] Evidencia local e retornada sem segredos pela resposta MCP.
- [x] Tool nao executa comando fora da allowlist definida.

## Tasks

- [x] Definir allowlist de checks locais.
- [x] Implementar runner de validacao ou adaptador para gates existentes.
- [x] Retornar evidencia objetiva na resposta MCP.
- [x] Adicionar testes de sucesso e falha.

## Verification

```bash
cargo test -p edger-mcp
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Status

completed (2026-06-29) - `edger.validate_local` valida discovery/parsing de manifests localmente, retorna `passed` ou `failed` com diagnostics estruturados e tem testes cobrindo sucesso e manifest invalido sem deploy remoto.
