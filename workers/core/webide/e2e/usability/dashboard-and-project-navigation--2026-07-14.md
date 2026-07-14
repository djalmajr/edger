# Usability — Navegar e localizar projetos no dashboard (dashboard-and-project-navigation)

- **Persona:** Autor iniciante no EdgeR WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable

## Walkthrough

1. O dashboard apresentou marca, busca central, navegação, ações e `Recent projects` com hierarquia clara e sem contagens redundantes.
2. `Projects` abriu `All projects` preservando busca e navegação.
3. `Dashboard` restaurou `Recent projects`, `New project` e `Import`.
4. A busca por `fetch` manteve somente `fetch-handler-app`.
5. A limpeza do campo por teclado restaurou `fetch-handler-app` e `static-spa-app`.
6. Um clique diretamente na célula `Fetch Handler` abriu o workbench de `fetch-handler-app`, confirmando que toda a linha é clicável.
7. O logo EdgeR retornou ao dashboard com os dois projetos locais preservados.

## Findings (prioritized)

Nenhum achado acionável neste fluxo.

## Key screens

- [Dashboard com projetos de runtimes distintos](screenshots/2026-07-14/dashboard-and-project-navigation/01-dashboard-projects.png)
- [Dashboard após retorno pelo logo](screenshots/2026-07-14/dashboard-and-project-navigation/02-returned-dashboard.png)
