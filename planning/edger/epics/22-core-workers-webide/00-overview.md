# Epic 22: Workspace organizado, workers core e WebIDE nativa

**Origin:** plano aprovado pelo usuário em 2026-07-13.

## Context

- **Problem:** crates e workers estavam no mesmo nível do repositório, o cPanel
  possuía regras core específicas e não existia uma experiência EdgeR-native de
  autoria e deploy.
- **Objective:** organizar o workspace, instituir uma origem confiável para apps
  core e distribuir cPanel + WebIDE runtime-ready na imagem.
- **Expected outcome:** overlays atualizam apps core sem eliminar a versão
  bundled; rascunhos da WebIDE sobrevivem a refresh sem alterar o runtime; apenas
  `Deploy` instala e ativa um snapshot validado.

## Architecture decisions

1. Crates vivem em `crates/`, preservando package names e comandos `cargo -p`.
2. `workers/core` contém produto distribuído; `workers/examples` contém somente
   fixtures e exemplos.
3. A origem `core_bundled`, `core_overlay` ou `user` é derivada do root confiável,
   nunca aceita do `manifest.yaml`.
4. `cpanel` e `webide` têm nomes/pathnames reservados e sempre mantêm ao menos
   uma versão habilitada e default.
5. Autosave é local; deploy é explícito e reutiliza install, release/migrations,
   health gate, ativação, rollback, logs e eventos existentes.
6. O preview usa a última versão implantada com sucesso em iframe sem
   `allow-same-origin` e sem credencial administrativa.

## Story backlog

| Story | Arquivo | Objetivo | Status |
|---|---|---|---|
| 22.01 Workspace Cargo | `01-workspace-crates.md` | Migrar crates sem mudar comportamento | completed |
| 22.02 Layout de workers | `02-worker-layout.md` | Separar core, exemplos e fixtures | completed |
| 22.03 Origens e invariantes | `03-core-origins-overlay.md` | Precedência, reservas, overlay e proteção core | completed |
| 22.04 Imagem e persistência | `04-docker-helm.md` | Empacotar apenas runtime e volumes necessários | completed |
| 22.05 Editor e rascunhos | `05-webide-editor-drafts.md` | Autoria local com autosave sem deploy | completed |
| 22.06 Deploy e preview | `06-webide-deploy-preview.md` | ZIP, pipeline, histórico e preview explícitos | completed |
| 22.07 Integrações e aceite | `07-integrations-verification.md` | Logs, observabilidade, docs e gates finais | completed |
| 22.08 Workbench completo | `08-reference-workbench-layout.md` | Dashboard, projetos, editor, preview e console seguro | completed |
| 22.09 Settings modernas | `09-settings-modernas.md` | Busca, escopos, herança, reset e efeitos coerentes | completed |

## Epic acceptance criteria

- [x] Package names permanecem estáveis após a migração para `crates/`.
- [x] cPanel e WebIDE são workers core; exemplos não entram no core.
- [x] Origem, precedência, colisões reservadas e invariante da última versão core
  têm testes automatizados.
- [x] Install de core usa overlay e só ativa após release e health gate.
- [x] WebIDE implementa rascunho local, ZIP determinístico, deploy explícito,
  preview isolado e acesso a logs/eventos.
- [x] Imagem final foi construída e inspecionada, sem exemplos/toolchains e com
  execução non-root.
- [x] Fluxos FetchHandler, RoutesTable e StaticSpa foram validados no Browser,
  inclusive refresh, falha de deploy e responsividade.
- [x] Dashboard e workbench completo foram validados com múltiplos projetos,
  autosave após refresh e terminal operacional sem shell do host.
- [x] O WebIDE mantém um catálogo versionado de fluxos UX para todas as jornadas
  implementadas, com fixtures locais e gate de cobertura.
- [x] Os fluxos usam personas próprias do produto para avaliar onboarding,
  eficiência, operação, confiabilidade, segurança, UI/UX e acessibilidade.
- [x] Settings oferece busca, categorias, filtro de modificadas, reset e escopos
  User/Workspace sem expor preferências sem efeito real.
- [x] Gate Rust e refinement gate estão verdes.

## Status

completed (2026-07-13) — estrutura, origens core, imagem, WebIDE, Browser E2E e
gates finais concluídos; evidência em
`planning/edger/status/evidence/epic-22-core-webide-2026-07-13.md`.

### Fundação React e shadcn compartilhada (2026-07-14)

A WebIDE foi migrada para React 19 + TypeScript + Vite e passou a compartilhar
com o cPanel o preset shadcn `base-nova`, tokens, fonte e componentes em
`workers/ui`. TanStack Query/Router e dnd-kit sustentam estado remoto,
navegação e ordenação; os ícones Lucide são compilados por `unplugin-icons`.
`bun run build:watch`, em `workers/`, mantém `workers/core/webide/dist`
atualizado, e o EdgeR serve somente esse `dist` como worker Static SPA.

### Settings pesquisáveis e coerentes por escopo (2026-07-15)

A Settings da WebIDE passou a usar um catálogo tipado para busca, categorias,
origem e escopos válidos. User e Workspace exibem o valor editável correto,
oferecem filtro de modificadas e reset para herança; Theme permanece User-only.
Word Wrap mantém as camadas do editor alinhadas, e Files Exclude também filtra a
busca. A preferência Auto Preview deixou de ser exibida enquanto não houver um
efeito compatível com deploy explícito. A jornada foi incorporada ao catálogo
E2E; busca, escopos e reset foram validados no Browser builtin.
