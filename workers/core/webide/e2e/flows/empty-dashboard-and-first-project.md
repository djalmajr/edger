---
id: empty-dashboard-and-first-project
name: Criar o primeiro projeto a partir do dashboard vazio
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: webide-first-time-author
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Perfil de Browser limpo, sem projetos no IndexedDB edger-webide
---

## User goal

Entender o estado inicial da WebIDE e criar o primeiro rascunho sem configuração
prévia.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **observe a seção Recent projects** → o empty state informa `No projects yet` e orienta criar ou importar um projeto.
2. **clique em Projects na navegação lateral** → All projects mantém o mesmo empty state e nenhum contador desnecessário aparece.
3. **clique em Dashboard e depois em New project** → o seletor de templates abre em Frontend.
4. **clique em Static SPA** → o primeiro projeto é criado e abre no workbench com manifesto e entrypoint válidos.
5. **edite uma linha de `index.html` e aguarde o autosave** → o rascunho fica salvo localmente sem deployment.
6. **clique no logo para retornar ao dashboard** → a tabela Recent projects substitui o empty state e mostra o projeto como Local draft.
7. **recarregue a página** → o primeiro projeto continua listado, provando a persistência inicial.
8. **clique na linha inteira do projeto** → o workbench volta a abrir com a edição restaurada.

## Expected result

O estado vazio orienta a ação principal e a criação do primeiro projeto substitui
o empty state por um rascunho persistente e reabrível.
