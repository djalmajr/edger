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

- [ ] Catalogo e gerado de dados do runtime, nao de lista hardcoded.
- [ ] Item desabilitado ou sem permissao tem comportamento explicito.
- [ ] Shell nao intercepta assets nem reserved paths indevidamente.
- [ ] Browser/local test cobre pelo menos catalogo e navegação para app.

## Tasks

- [ ] Definir shape de catalogo derivado de capabilities.
- [ ] Implementar shell/catalogo local ou atualizar worker demo.
- [ ] Cobrir roteamento e permissao em testes.
- [ ] Atualizar docs e matriz.

## Verification

```bash
cargo test -p edger-orchestrator --test shell_gateway
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

