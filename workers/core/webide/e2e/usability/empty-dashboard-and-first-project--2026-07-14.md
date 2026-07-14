# Usability — Criar o primeiro projeto a partir do dashboard vazio (empty-dashboard-and-first-project)

- **Persona:** Autor iniciante no EdgeR WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable

## Walkthrough

1. O dashboard vazio exibiu `No projects yet` e orientou criar ou importar um projeto.
2. `Projects` abriu `All projects` com o mesmo empty state e sem contador de projetos.
3. `Dashboard` e `New project` abriram o seletor já posicionado na categoria `Frontend`.
4. `Static SPA` criou `static-spa-app` e abriu o workbench com `index.html` e `manifest.yaml` válidos.
5. A edição do título em `index.html` foi mantida pelo autosave; o preview continuou informando que não havia deployment e que o autosave armazenava somente o rascunho.
6. O logo retornou ao dashboard, que mostrou `static-spa-app` como `Local draft`.
7. Após recarregar o dashboard, o projeto continuou listado.
8. A linha inteira reabriu o workbench e a edição `Meu primeiro projeto EdgeR` foi restaurada.

## Findings (prioritized)

Nenhum achado acionável neste fluxo.

## Key screens

- [Dashboard vazio](screenshots/2026-07-14/empty-dashboard-and-first-project/01-empty-dashboard.png)
- [Workbench após edição e autosave](screenshots/2026-07-14/empty-dashboard-and-first-project/02-edited-autosave.png)
- [Workbench reaberto com edição restaurada](screenshots/2026-07-14/empty-dashboard-and-first-project/03-restored-workbench.png)
