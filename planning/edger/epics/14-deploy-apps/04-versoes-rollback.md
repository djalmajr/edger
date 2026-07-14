# Story 14.04: Versões coexistentes e rollback

**Origin:** `planning/edger/epics/14-deploy-apps/00-overview.md`

## Context

- **Problema:** deploy sem rollback é armadilha: se a versão nova quebra, o operador precisa de um caminho de volta imediato.
- **Objetivo:** duas ou mais versões do mesmo worker coexistem no índice (`name@semver`); a resolução `latest`/range já existente decide o tráfego; rollback é reabilitar/priorizar a versão anterior.
- **Valor:** deploy sem medo — a promessa "mini Vercel" inclui desfazer em um clique.
- **Restrições:** aproveitar a resolução semver existente do router; não inventar roteamento paralelo.

## Traceability

- `crates/edger-orchestrator/src/router.rs` + `tests/routing_resolution.rs` (resolução `latest`, exata, ranges)
- `crates/edger-orchestrator/src/manifest_index_stub.rs` (bucket por nome com versões)
- Story 14.01 (install de nova versão)

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/manifest_index_stub.rs` | edit | Enable/disable por `name@version` (hoje é por nome) se necessário |
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Enable/disable aceitando `name@version`; listar versões por worker |
| `crates/edger-orchestrator/tests/deploy_install.rs` | edit | Cenário deploy v2 → rollback para v1 |
| `workers/core/cpanel/index.js` | edit | Workers view: agrupar versões + ação rollback |

## Detail

### AS-IS

- Índice suporta múltiplas versões e resolve `latest`/ranges; enable/disable opera por nome.
- Install (14.01) permite instalar `name@nova-versão` ao lado da atual.

### TO-BE

- `latest` resolve para a maior versão **habilitada**; desabilitar a v2 devolve o tráfego para a v1 (rollback observável).
- Admin API expõe versões por worker e enable/disable por `name@version`.
- cPanel agrupa versões na Workers view com ação de rollback.

### Scope

- **In:** enable/disable por versão, resolução `latest` sensível a enabled, listagem de versões, teste de rollback, UI mínima.
- **Out:** canary/percentual de tráfego, migração de estado entre versões, GC automática de versões antigas.

### Acceptance criteria

- [x] Instalar v2 ao lado de v1: `latest` passa a servir v2. (`deploy_v2_then_rollback_to_v1`)
- [x] Desabilitar v2: `latest` volta a servir v1 sem restart (rollback), provado por E2E + UI. (mutação provada: disable ignorando `version` quebra o caso da versão mais antiga)
- [x] Rota pinada `name@1.0.0` continua servindo v1 durante o ciclo; ao desabilitar v1 a rota pinada retorna 404. (`deploy_v2_then_rollback_to_v1`)
- [x] Admin API lista versões e estados por worker. (`admin_lists_each_version_with_its_state`)

### Dependencies

- Story 14.01

## Test-first plan

- **Behavior:** E2E deploy v1 → v2 → rollback, asserções por body servido em `/<name>`.
- **Level:** `crates/edger-orchestrator/tests/deploy_install.rs` + `routing_resolution.rs`.
- **Avoid:** asserções sobre estrutura interna do bucket de versões.

## Tasks

### Fase 1 — Resolução sensível a enabled
- [x] `latest`/ranges ignoram versões desabilitadas — já era o comportamento de `resolve_worker` (filtra `config.enabled`); coberto por `routing_resolution.rs` e agora pelo E2E de rollback.

### Fase 2 — API por versão
- [x] Enable/disable por `name@version`: `ManifestIndex::set_worker_enabled(name, version, enabled)` + rotas admin `?version=`; listagem já retorna uma linha por versão.

### Fase 3 — Rollback E2E + UI
- [x] E2E v1→v2→rollback→roll-forward + desabilitar versão mais antiga.
- [x] Workers view: coluna Actions com toggle Enable/Disable por versão (só quando há múltiplas versões) + badge `serving` na versão que atende `latest`.

## Verification

```bash
cargo test -p edger-orchestrator --test deploy_install -- rollback
cargo test -p edger-orchestrator --test routing_resolution
cargo test --workspace
```

## Status

**completed** (2026-07-02) — versões coexistem (`name@semver`) e o rollback é
observável sem restart. Backend: `ManifestIndex::set_worker_enabled` ganhou
`version: Option<&str>` (targeting exato por versão; `None` = latest) e rotas
admin `enable/disable?version=`; `resolve_worker` já resolvia `latest` só entre
versões habilitadas. UI: coluna Actions na tabela de Workers com toggle
Enable/Disable por versão (apenas em nomes multi-versão) e badge `serving` na
versão que atende `latest`. E2E `deploy_v2_then_rollback_to_v1` cobre
v1→v2→rollback→roll-forward + desabilitar a versão mais antiga (mutação
provada); `admin_lists_each_version_with_its_state` cobre a listagem por versão.
Validado no preview builtin (deploy v1/v2 via API → Disable v2 na UI → latest cai
para v1 → Enable v2 → volta para v2). Evidência:
`status/evidence/deploy-vertical-slice-2026-07-02.txt`.
