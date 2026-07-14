# Epic 22 — workspace, workers core e WebIDE

Data: 2026-07-13

## Gates

- `cargo metadata --no-deps` preservou os pacotes `edger-core`,
  `edger-worker`, `edger-isolation`, `edger-orchestrator` e `edger-mcp` sob
  `crates/`.
- `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo
  fmt -- --check`: PASS.
- `planning/edger/scripts/cpanel-ui-gate.sh`: PASS.
- `SCRATCH=/tmp/edger-refinement-final
  planning/edger/scripts/run-gates.sh`: PASS, com `ALL PLANNING GATES PASS`.
- `helm lint charts/edger`: PASS; `helm template edger charts/edger` confirmou
  os roots e PVCs separados para overlay core e workers de usuário.
- `deno check workers/core/webide/src/app.js
  workers/core/webide/src/editor-tools.js`: PASS; `src` e `dist` foram
  comparados byte a byte.

## Imagem Docker

- Imagem: `edger:core-webide` / digest local
  `sha256:b1600e5bcdefb2f983893c207e32fc45c5003ee0528ad0c24e02ec03ae0c17d3`.
- Execução comprovada como `uid=10001(edger) gid=10001(edger)`.
- Presentes: `/usr/local/bin/edger`, `/usr/bin/deno`, cPanel runtime-ready e
  WebIDE somente com `manifest.yaml` + `dist/`.
- Ausentes: exemplos, `webide/src`, Cargo, rustc, npm, Node e Bun.
- Smoke final em `127.0.0.1:19082`: `/health`, `/cpanel` e `/webide` retornaram
  HTTP 200.
- O JavaScript servido pela imagem contém o controle explícito do console
  operacional (`Run operational command`).
- O inventário da imagem contém somente `cpanel@0.2.0` e `webide@0.1.0`, ambos
  com origem `core_bundled`.

## Browser E2E

- Browser aberto em `http://127.0.0.1:19080/webide`, autenticado pelo fluxo real
  do cPanel.
- Após o refinamento visual da home, a busca ficou exatamente centralizada no
  viewport desktop, a marca permaneceu não interativa à esquerda e a galeria de
  templates foi substituída pelos cards funcionais New project e Import.
- O seletor New project mantém os templates somente no modal e renderiza um SVG
  por starter, sem glifos Unicode. A criação FetchHandler abriu o workbench pelo
  fluxo real; o contrato de importação de pasta com `manifest.yaml` também foi
  exercitado localmente.
- FetchHandler, RoutesTable e StaticSpa foram selecionados, editados,
  empacotados e implantados explicitamente pela WebIDE como
  `webide-e2e@1.0.0`, `webide-routes-e2e@1.0.0` e
  `webide-spa-e2e@1.0.0`. O inventário registrou os kinds corretos e origem
  `user`; os três pathnames versionados retornaram HTTP 200.
- Antes do primeiro clique em Deploy, o autosave restaurou o rascunho após
  refresh e o inventário continuou com zero versões do app de teste.
- O deploy exibiu as sete etapas: validação, empacotamento, upload,
  release/migrations, health, ativação e conclusão.
- Um manifesto com entrypoint inexistente falhou ainda na validação; o preview
  permaneceu apontando para a versão 1.0.0 previamente implantada.
- O iframe usa o pathname versionado, não recebe a chave administrativa e seu
  sandbox contém `allow-forms allow-modals allow-popups allow-scripts`, sem
  `allow-same-origin`.
- A aba Logs abriu o workspace versionado do cPanel, com eventos locais e
  correlação. Observability e Logs reutilizam os contratos existentes.
- Em viewport de 1360 px a WebIDE não apresentou overflow horizontal; o CSS
  responsivo troca grids e painéis em breakpoints compactos.

## Dashboard e workbench

- A entrada `/webide` apresenta dashboard, busca, templates e inventário de
  projetos; o rascunho legado é migrado para o armazenamento multiprojeto.
- A criação de projeto usa uma modal customizada com abas Frontend, Backend e
  Fullstack. React, Vue, FetchHandler, RoutesTable e Static SPA são selecionáveis;
  Svelte, TanStack Start e Next.js permanecem visíveis como planejados porque
  ainda exigem pipeline de build ou runtime compatível.
- O workbench reúne explorer, abas de arquivo, editor com gutter e atalho de
  salvamento, preview lateral isolado e footer redimensionável com Logs,
  Deployments e Terminal.
- O terminal operacional executou `help` no Browser e listou somente
  `help`, `validate`, `deploy`, `preview`, `files`, `status` e `clear`; não há
  shell de host.
- Uma alteração de arquivo foi salva automaticamente, reapareceu após reload no
  Browser e foi restaurada ao conteúdo original sem qualquer deploy.
- Em viewport compacto, explorer é recolhido e preview passa para baixo do
  editor, removendo o `min-width` estrutural que causaria scroll horizontal.

## Refinamento do workbench em 2026-07-14

- A activity rail ficou restrita a Explorer e Search; atalhos duplicados para
  deployments, observabilidade e cPanel foram removidos.
- A marca do header ficou somente com o logo, alinhado aos ícones da rail. O
  status redundante de draft e o botão duplicado Open deployed foram removidos.
- Search encontrou `Hello` em `index.html`, mostrou arquivo, linha, trecho e
  total; case-sensitive e regex permanecem disponíveis no painel.
- Explorer e tabs renderizaram ícones distintos para HTML e YAML.
- O editor exibiu syntax highlighting; ao trocar temporariamente o HTML por
  `<html><body><h1>Broken`, apresentou três problemas. O conteúdo original foi
  restaurado antes do encerramento da prova.
- Os toggles do header ocultaram e restauraram preview e painel inferior. O
  footer deixou de exibir o rótulo redundante `Local EdgeR workspace`.
- `Validate` permanece como preflight sem deploy: valida manifesto, entrypoint e
  paths, registra o resultado em Logs e não cria ZIP nem instala versão.

## Paridade funcional com a referência em 2026-07-14

- O header passou a 36 px, centralizou o nome do projeto e converteu Preview,
  Panel, Validate e Deploy em botões quadrados somente com ícones e labels
  acessíveis.
- Topbar, tabs do editor, toolbar do preview e header do footer compartilham
  36 px de altura. Deploy usa o mesmo tratamento neutro dos outros ícones e o
  estado vazio do painel informa quando ainda não existe versão ativa.
- Explorer, tabs, editor, preview e footer adotaram as mesmas métricas,
  tipografia, pesos e densidade da referência, mantendo a paleta escura EdgeR.
- Material File Icons 2.4.0 foi vendorizado sob licença MIT e renderizou os
  mesmos SVGs usados pelo projeto de referência.
- O Browser criou a pasta vazia `browser-parity-test`, criou `nested.ts` dentro
  dela e confirmou a guia vertical de nesting e a tab adicional; a pasta e o
  arquivo foram removidos depois da prova.
- O menu de reticências exibiu New file, New folder, Rename e Delete; o mesmo
  menu é ligado ao evento de contexto da linha. Um segundo ciclo renomeou
  `rename-test` para `renamed-test` e o removeu ao final.
- Fechar a última tab exibiu `Open a file from the Explorer`; clicar em
  `index.html` reabriu a tab. Ordem de tabs e seleção passam a integrar o
  autosave do projeto.
- Tabs de código e footer usam drag-and-drop com indicadores de destino; a
  função pura de reordenação foi exercitada para ambos os sentidos.
- `Preserve logs across restarts` permaneceu marcado após reload. Quando
  desmarcado, o próximo deploy limpa logs anteriores antes de registrar sua
  nova sequência; quando marcado, mantém o histórico.
- A interface deixou de manter SVGs manuais: Vite 8.1.4 e
  `unplugin-icons` 23.0.1 compilam somente os ícones Lucide usados. Material
  File Icons permanece dedicado a arquivos e extensões.
- Dashboard e workbench expõem slots shadcn para Button, Card, Table, Dialog,
  Tabs, Empty, Input, Checkbox, Badge e Context Menu, usando os tokens
  neutral/violet do cPanel com a paleta escura EdgeR.
- O botão `Open cPanel` foi removido. A prova no Browser confirmou sua ausência,
  assets relativos sob `/webide`, ícones Lucide com `viewBox="0 0 24 24"` e os
  quatro headers do workbench com 36 px; a árvore permaneceu com 25 px.
- A lista de projetos deixou de aplicar o fundo de Card à seção inteira e de
  exibir o contador redundante. O Browser confirmou a seção transparente, a
  tabela como única superfície e o nome do projeto alinhado ao cabeçalho.
- A marca do dashboard passou a usar logo de 24 px, branco no tema escuro e
  preparado para usar o token `primary` quando `data-theme="light"` estiver
  ativo.

## Catálogo de fluxos UX em 2026-07-14

- `workers/core/webide/e2e/flows/` contém 19 jornadas independentes iniciadas em
  `/webide`, cobrindo todas as metas de uso implementadas no dashboard e no
  workbench.
- O catálogo inclui estado vazio, navegação e gestão de projetos, todos os
  starters suportados/planejados, importação Static/Routes/Fetch, Explorer,
  tabs, editor, autosave, busca, lint, Validate, deploy, preview, footer, logs,
  terminal, splitters, responsividade e operação por teclado.
- Fixtures válidas e inválida em `e2e/fixtures/` tornam a importação repetível
  sem depender de arquivos externos ao repositório.
- `bun run test:flows` valida frontmatter, referências, entry point, numeração,
  contagem do catálogo, cobertura funcional e fixtures. O gate foi integrado a
  `planning/edger/scripts/webide-ui-gate.sh`.
- Sete personas locais em `e2e/personas/` distribuem os 19 fluxos entre autor
  iniciante, desenvolvedor avançado, operador EdgeR, engenheiro de
  confiabilidade, pesquisador adversarial, auditor UI/UX responsivo e usuário de
  tecnologia assistiva. O gate também valida a existência e o ID dessas lentes.
- A persona adversarial atua somente pela interface e com fixtures autorizados;
  ela verifica rejeições, isolamento e vazamentos observáveis, mas não substitui
  threat model, revisão de código ou pentest do runtime.
- A primeira execução completa com `ux-persona` permanece deliberadamente
  pendente de confirmação do catálogo pelo responsável do produto, conforme o
  contrato do skill `ux-flows`.

## Resultado

PASS — o Epic 22 satisfaz os critérios de estrutura, distribuição, proteção
core, deploy explícito, preview isolado, imagem mínima e operação sem serviços
externos obrigatórios.
