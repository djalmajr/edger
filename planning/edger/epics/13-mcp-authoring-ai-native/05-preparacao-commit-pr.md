# Story 13.05: Preparacao de commit e PR

**Origin:** `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## Context

O fluxo AI-native deve terminar em uma mudanca revisavel. Nesta fase, o limite e local: preparar diff, commit e metadata de PR quando autorizado, sem deploy remoto e sem abrir acesso irrestrito a git.

**Depende de:** Story 13.04

## Files

| Path | Action | Reason |
|---|---|---|
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar politica local de commit/PR |
| `planning/edger/status/evidence/` | edit | Registrar evidencia do fluxo completo |
| `workers/` | edit | Usar worker criado/modificado como fixture local |
| `edger-orchestrator/tests/value_parity.rs` | edit | Manter prova automatizada do worker |

## Detail

### AS-IS

- O operador ja usa commits/pushes via git, mas o edger nao oferece fluxo assistido por MCP.
- Nao ha metadata padrao de PR para mudancas de worker.

### TO-BE

- Tool prepara resumo de diff, arquivos alterados, resultados de validacao e sugestao de mensagem de commit.
- Commit local so acontece quando autorizado pelo operador.
- PR metadata e preparada como texto/arquivo local; push e abertura remota ficam fora desta fase salvo autorizacao explicita fora do MCP automatico.

### Scope

- **In:** diff summary, commit local autorizado, PR metadata local, evidencia.
- **Out:** deploy, merge remoto automatico, escrita em repos nao autorizados.

### Critérios de aceite

- [ ] Tool retorna resumo de diff e validacao antes de qualquer commit.
- [ ] Commit local exige autorizacao explicita.
- [ ] PR metadata inclui objetivo, testes e riscos.
- [ ] Fluxo completo nao executa deploy remoto.

## Tasks

- [ ] Definir contrato de resumo de diff e PR metadata.
- [ ] Integrar resultado da validacao local.
- [ ] Implementar caminho de commit local autorizado.
- [ ] Registrar evidencia do fluxo completo.

## Verification

```bash
git status --short --branch
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

