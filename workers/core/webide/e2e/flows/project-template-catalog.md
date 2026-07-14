---
id: project-template-catalog
name: Explorar e criar starters do catálogo
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: webide-first-time-author
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - O Browser permite persistência em IndexedDB
design_refs:
  template-picker: "planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md"
---

## User goal

Entender quais starters estão disponíveis e criar projetos a partir de todos os
templates atualmente suportados.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point (`dashboard`), **clique em New project** → um dialog acessível Create a new project abre com a categoria Frontend selecionada.
2. (`template-picker`) **observe Static SPA, React, Vue e Svelte** → os três primeiros exibem Ready e Svelte exibe Planned e permanece desabilitado.
3. (`template-picker`) **clique em Static SPA** → um novo projeto abre no workbench com `index.html` e `manifest.yaml`.
4. **Clique no logo do workbench, abra New project novamente e clique em React** → um projeto React abre com `index.html`, `app.js` e manifesto.
5. **Volte ao dashboard, abra New project e clique em Vue** → um projeto Vue abre com `index.html`, `app.js` e manifesto.
6. **Volte ao dashboard, abra New project e clique na categoria Backend** → Fetch Handler e Routes Table aparecem como Ready.
7. (`template-picker`) **clique em Fetch Handler** → um projeto com `index.ts`, kind fetch e manifesto abre no workbench.
8. **Volte ao dashboard, abra New project, selecione Backend e clique em Routes Table** → um projeto com rotas declarativas e kind routes abre.
9. **Volte ao dashboard, abra New project e clique na categoria Fullstack** → TanStack Start e Next.js aparecem como Planned e não podem ser ativados.
10. (`template-picker`) **passe o ponteiro sobre cada opção Planned** → o title explica que a capacidade de runtime ainda não está disponível.
11. (`template-picker`) **clique no botão Close** → o dialog fecha e o foco retorna para New project.
12. (`dashboard`) **abra New project novamente e clique no overlay fora do dialog** → o seletor fecha sem criar projeto.
13. (`dashboard`) **observe os projetos recém-criados** → cada nome é único, o runtime correto é exibido e os rascunhos estão disponíveis para abertura.

## Expected result

Todos os templates suportados criam projetos coerentes e os templates planejados
continuam visíveis, explicados e impossíveis de selecionar.
