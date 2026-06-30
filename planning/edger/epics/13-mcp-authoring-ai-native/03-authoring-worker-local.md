# Story 13.03: Authoring local de worker

**Origin:** `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## Context

Para entregar valor AI-native real, um agente precisa criar ou modificar worker passando pelo edger, com limites claros de workspace, diff e dry-run. Isso nao deve virar ferramenta de escrita irrestrita no sistema.

**Depende de:** Story 13.02

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/` | edit | Criar ou modificar workers dentro do workspace permitido |
| `edger-orchestrator/tests/manifest_loader.rs` | edit | Provar worker criado/modificado reconhecido pelo runtime |
| `edger-orchestrator/tests/value_parity.rs` | edit | Provar fluxo local representativo |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar limites de authoring |

## Detail

### AS-IS

- Workers sao arquivos locais carregados pelo runtime.
- Nao ha API/tool segura para um agente criar ou editar worker.

### TO-BE

- Tool de authoring recebe plano estruturado, valida path dentro do workspace e suporta dry-run.
- Mudanca produz diff previsivel e arquivos no layout de worker.
- Nao escreve fora de `workers/` ou raiz explicitamente permitida.

### Scope

- **In:** criar worker novo, editar worker simples, dry-run, path validation, diff.
- **Out:** edicao fora do repo, instalacao de pacotes remotos, deploy.

### Critérios de aceite

- [ ] Dry-run mostra arquivos que seriam criados/modificados.
- [ ] Path traversal e escrita fora do workspace sao bloqueados.
- [ ] Worker criado e descoberto pelo manifest/autodiscovery.
- [ ] Teste local prova dispatch ou rota basica do worker.

## Tasks

- [ ] Definir input estruturado de authoring.
- [ ] Implementar validacao de path e dry-run.
- [ ] Implementar escrita controlada no workspace permitido.
- [ ] Adicionar teste de worker criado/modificado.

## Verification

```bash
cargo test -p edger-orchestrator --test manifest_loader
cargo test -p edger-orchestrator --test value_parity
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

