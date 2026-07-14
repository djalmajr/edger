---
id: import-project-folder
name: Importar um projeto a partir de pasta local
reference: planning/edger/epics/22-core-workers-webide/05-webide-editor-drafts.md
persona: webide-first-time-author
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - O seletor de pasta pode acessar workers/core/webide/e2e/fixtures
design_refs:
  dashboard: "planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md"
---

## User goal

Trazer para a WebIDE uma pasta local que contém um projeto EdgeR válido.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point (`dashboard`), **clique em Import e selecione a pasta `e2e/fixtures/import-missing-entrypoint`** → a interface rejeita a importação informando que `missing.js` não existe.
2. (`dashboard`) **feche o alerta de erro** → o dashboard continua utilizável e nenhum projeto inválido é criado.
3. (`dashboard`) **clique em Import e selecione a pasta `e2e/fixtures/import-static-spa`** → a pasta é lida e o workbench do projeto importado abre.
4. No Explorer, **expanda `assets` se necessário** → `index.html`, `manifest.yaml`, `assets` e `assets/styles.css` aparecem com nesting e ícones de tipo.
5. **clique em `manifest.yaml`** → o editor mostra nome, versão e entrypoint importados.
6. **clique em `assets/styles.css`** → a tab abre com title completo e syntax highlighting de CSS.
7. **clique no logo para voltar ao dashboard e pesquise `imported-webide-flow`** → o projeto importado aparece como Static Spa e Local draft.
8. **limpe a busca, clique em Import e selecione `e2e/fixtures/import-routes-table`** → o workbench abre o projeto importado com `manifest.yaml` e `index.ts`.
9. **clique em `manifest.yaml` e depois em `index.ts`** → kind routes, entrypoint e tabela de rotas estão preservados.
10. **volte ao dashboard e localize `imported-routes-flow`** → a coluna Runtime mostra Routes Table.
11. **clique em Import e selecione `e2e/fixtures/import-fetch-handler`** → o workbench abre o handler importado.
12. **clique em `manifest.yaml` e depois em `index.ts`** → kind fetch, entrypoint e handler default estão preservados.
13. **volte ao dashboard e localize `imported-fetch-flow`** → a coluna Runtime mostra Fetch Handler e os três imports podem ser reabertos.

## Expected result

Pastas incompletas são rejeitadas sem criar dados; fixtures válidas preservam
paths e conteúdo, são classificadas como Static, Routes ou Fetch e persistem.
