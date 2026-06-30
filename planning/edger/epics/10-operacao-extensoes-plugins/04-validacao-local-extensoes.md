# Story 10.04: Validacao local de extensoes

**Origin:** `planning/edger/epics/10-operacao-extensoes-plugins/00-overview.md`

## Context

O projeto precisa ser modular e amigavel a agentes. Para extensoes, isso significa que uma mudanca deve ter uma validacao local objetiva antes de commit, sem deploy remoto e sem depender de leitura manual de todos os contratos.

**Depende de:** Story 10.02, Story 10.03

## Files

| Path | Action | Reason |
|---|---|---|
| `planning/edger/scripts/run-gates.sh` | edit | Incluir checks locais de extensao quando houver contrato pronto |
| `planning/edger/scripts/refinement-lint.py` | edit | Validar referencias de modulos se necessario |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar evidencia operacional de modulo |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar checklist local |

## Detail

### AS-IS

- Gate de planejamento roda lint, preflight, deploy layout e cargo check.
- Rust gate completo e exigido antes de declarar conclusao.
- Nao ha gate especifico para pacote/modulo de extensao.

### TO-BE

- Uma validacao local por modulo roda testes relevantes e confirma manifesto, status, diagnostics e redaction.
- Evidencia fica em `planning/edger/status/evidence/`.
- O fluxo pode ser chamado por humano, CI local ou MCP no Epic 13.

### Scope

- **In:** check local, evidencia, docs, integracao com gates existentes.
- **Out:** CI remoto, publicacao, deploy.

### Critérios de aceite

- [ ] Validacao local falha se manifesto/status/diagnostics de modulo estiverem inconsistentes.
- [ ] Evidencia versionada registra comandos e resultado.
- [ ] Fluxo nao exige rede externa.
- [ ] Documentacao deixa claro quando rodar Rust gate completo.

## Tasks

- [ ] Definir entrada do check local de modulo.
- [ ] Implementar validacao de manifesto e inventario.
- [ ] Registrar evidencia em scratch/status sem vazar segredos.
- [ ] Atualizar docs de operacao.

## Verification

```bash
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

