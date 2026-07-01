# Story 12.02: Shell e catalogo de modulos

**Origin:** `planning/edger/epics/12-frontends-modulares-cpanel/00-overview.md`

## Context

O shell atual prova roteamento e app composition, mas o valor Buntime inclui catalogo de apps/plugins. O edger deve usar `MenuContribution` e inventario operacional em vez de hardcode.

**Depende de:** Story 12.01, Epic 10.01

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/shell-demo/` | edit | Evoluir shell demo para catalogo real se for o caminho escolhido |
| `edger-orchestrator/tests/shell_gateway.rs` | edit | Provar shell/catalogo sem quebrar bypass de iframe/app |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar contributions usadas pelo catalogo |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar fluxo local |

## Detail

### AS-IS

- `workers/shell-demo` existe como prova de shell.
- `ExtensionCapability::MenuContribution` tipa menus.
- Catalogo navegavel ainda nao existe.

### TO-BE

- Shell renderiza catalogo a partir de workers/apps e contributions de modulos.
- Itens incluem id, titulo, rota, modulo dono, status e visibility.
- A experiencia continua densa e operacional, sem virar landing page.

### Scope

- **In:** catalogo local, menu/capability contributions, estados de erro.
- **Out:** design system completo, marketplace publico, editor de codigo.

### Critérios de aceite

- [x] Catalogo e gerado de dados do runtime, nao de lista hardcoded.
- [x] Item desabilitado ou sem permissao tem comportamento explicito.
- [x] Shell nao intercepta assets nem reserved paths indevidamente.
- [x] Browser/local test cobre pelo menos catalogo e navegação para app.

## Tasks

- [x] Definir shape de catalogo derivado de capabilities.
- [x] Implementar shell/catalogo local ou atualizar worker demo.
- [x] Cobrir roteamento e permissao em testes.
- [x] Atualizar docs e matriz.

## Verification

```bash
cargo test -p edger-orchestrator --test shell_gateway
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test -p edger-orchestrator --test registry_providers
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Status

completed (2026-07-01) - `GET /api/admin/catalog` expõe catalogo root-only
derivado do inventario de workers e das `MenuContribution` registradas por
extensoes. `workers/shell-demo` consome esse contrato, mantém root key apenas em
memoria e trata itens desabilitados/sem credencial explicitamente. Evidencia:
`planning/edger/status/evidence/story-12-02-runtime.txt` e
`planning/edger/status/closure-2026-07-01-story-12-02-shell-catalog.md`.
