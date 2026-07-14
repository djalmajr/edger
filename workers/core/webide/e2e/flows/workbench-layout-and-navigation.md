---
id: workbench-layout-and-navigation
name: Navegar pelas regiões do workbench
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: webide-first-time-author
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto com ao menos dois arquivos
design_refs:
  workbench: "planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md"
---

## User goal

Entender a organização do workbench e controlar suas regiões principais.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **clique em uma linha de projeto** → o workbench abre com logo, nome centralizado e quatro ações acessíveis no header.
2. (`workbench`) **observe Explorer, tabs, editor, Preview e painel inferior** → todas as regiões estão alinhadas e usam headers de 36 px.
3. **passe o ponteiro sobre a tab ativa** → o title nativo mostra o path completo do arquivo truncado.
4. **clique no ícone Hide preview** → preview e splitter lateral somem e o editor ocupa a largura liberada.
5. **clique no ícone Show preview** → o preview volta com toolbar contendo somente Preview e suas ações.
6. **clique no ícone Hide panel** → splitter e painel inferior somem e a área superior cresce.
7. **clique no ícone Show panel** → o footer volta na tab anteriormente selecionada.
8. **clique em Search na activity bar e depois em Explorer** → a lateral alterna entre busca e árvore sem trocar de projeto.
9. **clique no logo EdgeR** → o dashboard reaparece preservando o rascunho.

## Expected result

O usuário identifica cada região, alterna preview/footer/sidebar e retorna ao
dashboard sem perder estado do projeto.
