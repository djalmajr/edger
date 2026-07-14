# Story 22.08: Dashboard e workbench de autoria completos

**Origin:** `planning/edger/epics/22-core-workers-webide/00-overview.md`, com
ampliação visual aprovada pelo usuário em 2026-07-13.

## Context

A primeira interface provava autosave, empacotamento, deploy e preview, mas não
oferecia a experiência completa de um ambiente de autoria. O produto precisa de
dashboard, lista de projetos e um workbench contínuo sem reintroduzir Nodepod ou
criar um segundo runtime.

## Files

- `workers/core/webide/src/app.js`
- `workers/core/webide/src/icons.js`
- `workers/core/webide/src/editor-tools.js`
- `workers/core/webide/src/vendor/material-file-icons.js`
- `workers/core/webide/src/vendor/material-file-icons.LICENSE`
- `workers/core/webide/src/styles.css`
- `workers/core/webide/package.json`
- `workers/core/webide/bun.lock`
- `workers/core/webide/vite.config.js`
- `workers/core/webide/dist/`
- `workers/core/webide/e2e/README.md`
- `workers/core/webide/e2e/personas/*.md`
- `workers/core/webide/e2e/flows/*.md`
- `workers/core/webide/e2e/fixtures/`
- `workers/core/webide/e2e/validate-flows.sh`
- `planning/edger/status/evidence/epic-22-core-webide-2026-07-13.md`

## Detail

O dashboard mantém projetos independentes no armazenamento local. O workbench
combina explorer, abas, editor com gutter e atalhos, preview isolado e painel
inferior redimensionável. O terminal é um console operacional restrito ao
projeto EdgeR; não expõe shell, filesystem do host ou execução arbitrária.

O refinamento de 2026-07-14 aproxima o workbench da referência sem copiar
controles que pertencem ao cPanel: a rail mantém Explorer e busca textual no
projeto; preview e painel inferior são alternáveis pelo header; ícones distinguem
tipos de arquivo; e o editor oferece syntax highlighting e diagnósticos locais
básicos para manifesto, JSON, YAML, HTML e delimitadores de JS/TS/CSS.

A revisão seguinte adota o workbench de referência como especificação de
interação e densidade visual, preservando somente a paleta do EdgeR. Isso inclui
header de 36 px, título central, ações apenas por ícone, explorer redimensionável,
árvore hierárquica com guias, Material File Icons, menus de contexto, dialogs de
operações de arquivo, tabs reordenáveis e footer reordenável com opção de
preservar logs entre reinícios/deploys.

O refinamento seguinte usa como referência o preset shadcn neutral/violet do
cPanel: tokens semânticos, raio de `0.625rem` e composição por slots para Button,
Card, Table, Dialog, Tabs, Empty, Input, Checkbox, Badge e Context Menu. Os ícones
da interface são Lucide compilados sob demanda por `unplugin-icons`; Material
File Icons permanece restrito aos tipos de arquivo. O build Vite emite assets
relativos para continuar compatível com `injectBase` no pathname `/webide`.

A home expõe somente as ações funcionais New project e Import. A marca e a
navegação ficam alinhadas à esquerda, enquanto a busca ocupa o centro real do
viewport. A importação lê uma pasta local textual com `manifest.yaml` e a
converte para o mesmo modelo de rascunho usado pela criação.

A criação usa um `Dialog` visualmente alinhado ao DS shadcn do cPanel, com abas
Frontend, Backend e Fullstack e ícones SVG próprios para cada starter. React e
Vue são empacotados como Static SPA com ESM browser-native; FetchHandler,
RoutesTable e Static SPA usam contratos nativos do EdgeR. Svelte, TanStack Start
e Next.js aparecem como planejados e desabilitados até existir pipeline de
build e runtime compatível, evitando prometer um deploy que o EdgeR ainda não
consegue executar.

## Tasks

- [x] Implementar dashboard, busca centralizada, ações e inventário de projetos.
- [x] Implementar importação de pasta local textual com manifesto completo.
- [x] Implementar seletor customizado de templates por categoria, com estado de
  suporte explícito, ícones SVG e navegação acessível por teclado.
- [x] Migrar o rascunho legado para o modelo de múltiplos projetos.
- [x] Implementar explorer, abas, editor, preview e painéis redimensionáveis.
- [x] Implementar busca textual por projeto com case-sensitive e regex.
- [x] Adicionar ícones por tipo de arquivo, syntax highlighting e lint básico.
- [x] Adicionar toggles de preview e painel e remover ações duplicadas do header
  e da activity rail.
- [x] Portar densidade, tipografia, pesos e estrutura do workbench de referência.
- [x] Implementar pastas vazias, árvore hierárquica recolhível e guias de nesting.
- [x] Implementar criação, rename e delete de arquivo/pasta por dialogs e menus
  de contexto/reticências.
- [x] Implementar drag-and-drop e persistência da ordem das tabs de código e do
  footer.
- [x] Adicionar `Preserve logs across restarts`, aplicado ao início do deploy.
- [x] Substituir os ícones simplificados por Material File Icons vendorizados.
- [x] Substituir SVGs manuais da interface por Lucide via `unplugin-icons`.
- [x] Aplicar os tokens e slots shadcn do preset do cPanel a dashboard,
  dialogs, tabelas, tabs, inputs, estados vazios e ações do workbench.
- [x] Remover o atalho redundante `Open cPanel` do dashboard.
- [x] Adicionar Logs, Deployments e terminal operacional seguro no footer.
- [x] Preservar autosave local e deploy exclusivamente explícito.
- [x] Adaptar dashboard e workbench para viewports compactos sem scroll
  horizontal estrutural.
- [x] Validar navegação, terminal e restauração do autosave no Browser.
- [x] Catalogar jornadas de uso do dashboard e workbench em fluxos UX
  independentes, com fixtures e gate de cobertura.
- [x] Definir personas específicas do produto para autoria inicial, uso
  avançado, operação, confiabilidade, segurança adversarial, UI/UX responsiva e
  tecnologia assistiva.

## Acceptance criteria

- [x] A entrada da WebIDE apresenta dashboard, lista de projetos e apenas os
  cards funcionais New project e Import; templates aparecem somente no modal.
- [x] Marca e navegação permanecem à esquerda e a busca fica centralizada no
  viewport desktop.
- [x] O botão New project abre um seletor customizado; não usa prompt nativo.
- [x] Import aceita uma pasta local com `manifest.yaml`, valida o entrypoint e
  abre o rascunho importado no workbench.
- [x] Templates indisponíveis permanecem visíveis como planejados, mas não podem
  criar projetos inválidos.
- [x] Cada projeto restaura arquivos, arquivo ativo e metadados após refresh.
- [x] O preview fica ao lado do editor em desktop e abaixo dele em viewport
  compacto.
- [x] Logs, terminal e deployments compartilham o painel inferior.
- [x] Busca abre arquivos a partir do match e informa linha e total de resultados.
- [x] Editor sinaliza problemas locais sem depender de deploy ou serviço externo.
- [x] Fechar a última tab mostra estado vazio e selecionar arquivo a reabre.
- [x] Validate e Deploy aparecem somente como ícones acessíveis no header.
- [x] Operações de árvore preservam paths, tabs abertas e seleção após autosave.
- [x] O terminal aceita apenas comandos operacionais EdgeR documentados.
- [x] Nenhum autosave chama a API administrativa de instalação.
- [x] `dist` é reproduzível a partir de `src` por `bun run build`, com assets
  relativos ao pathname do worker.
- [x] O catálogo UX cobre estado vazio, projetos, templates, importação,
  Explorer, editor, busca, lint, validação, deploy/preview, footer, terminal,
  persistência, responsividade e acessibilidade em arquivos individuais.
- [x] O gate de fluxos valida schema, entry point, passos sequenciais,
  referências, matriz de cobertura e fixtures importáveis.
- [x] Cada fluxo referencia uma persona local existente e o gate valida IDs,
  catálogo e integridade das sete personas do WebIDE.

## Verification

```bash
cd workers/core/webide
bun install --frozen-lockfile
bun run build
bun run test:flows
cd ../../..
deno check workers/core/webide/src/editor-tools.js
rg 'src="\./app.js"|href="\./styles.css"' workers/core/webide/dist/index.html
planning/edger/scripts/webide-ui-gate.sh
```

Browser: alinhamento da home, cards New project/Import, SVGs do modal, criação e
abertura do projeto, comando `help` no terminal e edição restaurada após reload.

## Status

completed (2026-07-14).
