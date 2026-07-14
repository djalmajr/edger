# Story 13.03: Authoring local de worker

**Origin:** `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## Context

Para entregar valor AI-native real, um agente precisa criar ou modificar worker passando pelo edger, com limites claros de workspace, diff e dry-run. Isso nao deve virar ferramenta de escrita irrestrita no sistema.

**Depende de:** Story 13.02

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-mcp/src/discovery.rs` | create | Implementar escrita controlada dentro de `workers/` |
| `crates/edger-mcp/tests/protocol.rs` | create | Provar dry-run, path guard e worker criado descoberto |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar limites de authoring |

## Detail

### AS-IS

- Workers sao arquivos locais carregados pelo runtime.
- Nao ha API/tool segura para um agente criar ou editar worker.

### TO-BE

- Tool de authoring recebe plano estruturado, valida path dentro do workspace e suporta dry-run.
- Mudanca produz diff previsivel e arquivos no layout de worker.
- Nao escreve fora de `workers/` ou raiz explicitamente permitida.
- A primeira versao usa `edger.write_worker_file`, que cria ou substitui arquivos sob `workers/`, com `dryRun: true` por default e `overwrite` explicito para replace.

### Scope

- **In:** criar worker novo, editar worker simples, dry-run, path validation, diff.
- **Out:** edicao fora do repo, instalacao de pacotes remotos, deploy.

### Critérios de aceite

- [x] Dry-run mostra arquivos que seriam criados/modificados.
- [x] Path traversal e escrita fora do workspace sao bloqueados.
- [x] Worker criado e descoberto pelo manifest/autodiscovery.
- [x] Teste local prova authoring + discovery do worker criado.

## Tasks

- [x] Definir input estruturado de authoring.
- [x] Implementar validacao de path e dry-run.
- [x] Implementar escrita controlada no workspace permitido.
- [x] Adicionar teste de worker criado/modificado.

## Verification

```bash
cargo test -p edger-mcp
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Status

completed (2026-06-29) - `edger.write_worker_file` cria/substitui arquivos dentro de `workers/`, bloqueia path traversal, usa dry-run por default e tem teste provando que um worker criado via MCP passa a ser descoberto por `edger.list_workers`.
