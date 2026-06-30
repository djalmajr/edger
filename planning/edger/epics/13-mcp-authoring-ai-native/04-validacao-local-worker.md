# Story 13.04: Validacao local de worker

**Origin:** `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## Context

Authoring sem validacao gera risco. O MCP deve conseguir rodar validacao local do worker, capturar evidencia e reportar falhas sem deploy remoto.

**Depende de:** Story 13.03

## Files

| Path | Action | Reason |
|---|---|---|
| `planning/edger/status/evidence/` | edit | Registrar evidencia local de validacao |
| `edger-orchestrator/tests/value_parity.rs` | edit | Cobrir worker criado/modificado |
| `planning/edger/scripts/run-gates.sh` | edit | Reusar gates locais quando cabivel |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar comando local e limites |

## Detail

### AS-IS

- Gated workflow existe para repo inteiro.
- Nao ha validacao local especifica invocavel por MCP para worker modificado.

### TO-BE

- Tool de validacao roda checks locais relevantes: manifest/autodiscovery, dispatch, security preflight e gates definidos.
- Evidencia contem comando, status, resumo e paths de logs.
- Falhas retornam diagnostico suficiente para agente corrigir.

### Scope

- **In:** validacao local, evidencia, diagnostico estruturado.
- **Out:** deploy remoto, testes de carga longos, execucao de comandos arbitrarios.

### Critérios de aceite

- [ ] Validacao falha para worker invalido com mensagem estruturada.
- [ ] Validacao passa para worker criado pela Story 13.03.
- [ ] Evidencia local e registrada sem segredos.
- [ ] Tool nao executa comando fora da allowlist definida.

## Tasks

- [ ] Definir allowlist de checks locais.
- [ ] Implementar runner de validacao ou adaptador para gates existentes.
- [ ] Persistir evidencia objetiva.
- [ ] Adicionar testes de sucesso e falha.

## Verification

```bash
cargo test -p edger-orchestrator --test value_parity
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

