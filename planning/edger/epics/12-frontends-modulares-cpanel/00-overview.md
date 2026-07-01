# Epic 12: Frontends Modulares e cPanel

**Origin:** `planning/edger/roadmap.md`

**Depends on epic:** `planning/edger/epics/10-operacao-extensoes-plugins/00-overview.md`

## Context

### Macro problem

Buntime nao entrega apenas runtime: ele tambem tem `apps/cpanel`, `apps/shell`, `apps/webide`, `apps/platform` e apps de exemplo. O edger precisa capturar esse valor, mas sem transformar frontends em dependencia do core ou em um unico app empacotado demais.

### Initiative objective

Definir e entregar a primeira estrutura de frontends modulares do edger: cPanel/admin UI, shell/catalogo de modulos, packaging de frontends como workers/modulos e validacao local no Browser. As telas devem consumir contratos estaveis e nao criar APIs ad hoc.

### AS-IS

- Shell v1 e static SPA routing ja existem.
- Workers demo validam document routing e `/todos`.
- Menus de extensao existem como capability tipada.
- cPanel/admin UI minimo agora existe como worker modular; shell/catalogo derivado de contributions ainda esta fora do v1.

### TO-BE

- cPanel/admin UI e shell sao modulos separados, com dependencia explicita das APIs de operacao.
- Catalogo de modulos usa menu/capability contributions em vez de hardcode.
- Frontends podem ser empacotados e validados localmente como workers/apps.
- Browser validation cobre fluxos principais antes de commit.

### Out of scope

- Landing page de marketing.
- Marketplace publico.
- Deploy remoto.
- IDE completa estilo WebIDE antes dos contratos de authoring do Epic 13.

## Story backlog

| Story | Arquivo | Objetivo | Tamanho | Status | Depende de |
|---|---|---|---|---|---|
| 12.01 Escopo cPanel/admin UI | `01-escopo-cpanel-admin-ui.md` | Mapear telas e contratos minimos do cPanel/admin UI | medium | completed | Epic 08.02, Epic 10.01 |
| 12.02 Shell e catalogo de modulos | `02-shell-catalogo-modulos.md` | Tornar shell/catalogo alimentado por menu/capability contributions | large | planned | 12.01, Epic 10.01 |
| 12.03 Packaging de frontends | `03-packaging-frontends-workers.md` | Empacotar frontends como workers/apps versionados e isolados | medium | completed | 12.01 |
| 12.04 Validacao Browser local | `04-validacao-browser-local.md` | Validar os fluxos front-end localmente com Browser/Playwright | medium | completed | 12.01, 12.03 |

## Epic acceptance criteria

- [x] cPanel/admin UI tem escopo inicial, telas e contratos vinculados a APIs existentes ou planejadas.
- [ ] Shell/catalogo renderiza modulos a partir de contributions tipadas.
- [x] Frontends sao empacotados como workers/apps e nao como parte do core.
- [x] Browser validation local cobre cPanel minimo, detalhe operacional e fluxo de erro/acesso controlado por root key.
- [x] UI nao introduz bypass de auth, CSRF ou namespace.
- [ ] Gate de planejamento fica verde: `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`.

## Status

in-progress (2026-06-30) - cPanel/admin UI minimo foi entregue em `workers/cpanel`, empacotado como Static SPA worker e validado no Browser in-app com login root e criacao/revogacao de chave descartavel. Shell/catalogo por `MenuContribution` segue aberto na Story 12.02.
