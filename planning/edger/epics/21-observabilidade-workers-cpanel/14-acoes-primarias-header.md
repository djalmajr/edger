# Story 21.14: Ações primárias no topo do conteúdo

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problema:** ações primárias como Refresh, Deploy app e Upload files mudam
  de posição entre rotas e competem com filtros e navegação local.
- **Objetivo:** oferecer uma área previsível de ações na faixa superior do
  conteúdo, alinhada às tabs quando existirem, sem retirar ações contextuais de
  cards, tabelas, dialogs e painéis.
- **Restrições:** preservar estado local dos dialogs/uploads e manter tabs,
  filtros e breadcrumbs próximos do conteúdo que controlam.

## Traceability

- **Telas:** `/cpanel/`, `/cpanel/workers`, `/cpanel/observability`,
  `/cpanel/observability/logs` e `/cpanel/workers/:name/:version/*`.
- **Origem visual:** comentário Browser do operador em `/cpanel/workers`.
- **Contrato:** apenas composição React; nenhum endpoint ou payload muda.

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/core/cpanel/src/main.tsx` | edit | Criar slot na faixa do conteúdo e mover ações primárias |
| `workers/core/cpanel/src/components/overview.tsx` | edit | Remover Refresh local redundante |
| `planning/edger/scripts/cpanel-ui-gate.sh` | edit | Proteger o slot compartilhado |
| `planning/edger/status/evidence/` | edit | Registrar Browser e gates |

## Detail

### AS-IS

- Refresh aparece em posições diferentes conforme a rota.
- Deploy app fica ao lado dos filtros de Workers.
- Upload files fica na linha de breadcrumb de Files.

### TO-BE

- Refresh aparece na faixa superior do conteúdo de todas as páginas
  autenticadas.
- Ações específicas da página usam um slot nessa faixa: Deploy app em Workers e
  Upload files em Files mutável.
- Idioma, tema e conta ocupam o canto direito do header compartilhado.
- Tabs, filtros, paginação, ações de linha/card e ações de dialog permanecem
  contextuais.

### Scope

- **In:** ações primárias de página e infraestrutura de composição no topo do
  conteúdo.
- **Out:** botões contextuais, ações de linha, menus e tabs.

### Approach

- Usar Context + portal React para preservar o ownership do estado na página e
  renderizar somente sua ação primária na faixa do conteúdo do Shell.
- Manter Refresh no Shell, antes das ações específicas da rota.

### Risks

- **Ação residual após navegação:** o portal desmonta junto da página anterior.
- **Upload perder o input:** o input permanece na árvore de Files e o botão no
  portal aciona a mesma ref.

## Acceptance criteria

- [x] Refresh está na faixa superior do conteúdo em Overview, Workers,
  Observability, Logs e Files.
- [x] Deploy app aparece nessa faixa somente em Workers.
- [x] Upload files aparece nessa faixa somente para Files mutável.
- [x] Tabs ficam à esquerda das ações quando existirem.
- [x] Não há Refresh/Deploy/Upload duplicado na área de conteúdo.
- [x] Filtros, tabs, breadcrumbs e ações contextuais permanecem funcionais.

## Test-first plan

- **Red:** o gate estático deve falhar sem um slot compartilhado de ações.
- **Green:** criar o slot, mover as ações e proteger os contratos principais.
- **Refactor:** manter o portal genérico e evitar estado de página no Shell.
- **Nível:** typecheck/build + gate estático + validação Browser por rota.
- **Evitar:** snapshots integrais de markup e testes de detalhes internos do
  portal sem comportamento visível.

## Tasks

- [x] Criar o slot compartilhado de ações no topo do conteúdo.
- [x] Tornar Refresh uma ação global de página autenticada.
- [x] Mover Deploy app e Upload files preservando estado local.
- [x] Remover wrappers e ações duplicadas no conteúdo.
- [x] Atualizar gate, evidência e validar as rotas no Browser.

## Verification

```bash
cd workers/core/cpanel && bun test && bun run build
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed** (2026-07-15) — slot compartilhado, ações por rota, gates e prova
Browser registrados em
`planning/edger/status/evidence/cpanel-header-actions-2026-07-15.md`.
