# Usability — Explorar e criar starters do catálogo (project-template-catalog)

- **Persona:** Autor iniciante no EdgeR WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable

## Walkthrough

1. `New project` abriu o diálogo acessível `Create a new project` com `Frontend` selecionado.
2. Frontend exibiu Static SPA, React e Vue como `Ready`; Svelte ficou visível como `Planned` e desabilitado.
3. Static SPA criou `static-spa-app-2` com `index.html` e `manifest.yaml`.
4. React criou `react-app` com `index.html`, `app.js` e manifesto.
5. Vue criou `vue-app` com `index.html`, `app.js` e manifesto.
6. Backend exibiu Fetch Handler e Routes Table como `Ready`.
7. Fetch Handler criou `fetch-handler-app-2` com `index.ts`, manifesto e `kind: fetch`.
8. Routes Table criou `routes-table-app` com `index.ts`, manifesto e `kind: routes`.
9. Fullstack exibiu TanStack Start e Next.js como `Planned` e desabilitados.
10. O hover nas duas opções Planned encontrou titles que explicam a indisponibilidade das capacidades de runtime.
11. `Close template picker` fechou o diálogo e devolveu o foco para `New project`.
12. Um clique no overlay fechou o seletor sem criar projeto.
13. O dashboard mostrou os cinco projetos criados com nomes únicos, runtimes corretos e estado `Local draft`.

## Findings (prioritized)

Nenhum achado acionável neste fluxo.

## Key screens

- [Diálogo do catálogo e estado Planned](screenshots/2026-07-14/project-template-catalog/01-planned-dialog.png)
- [Projetos criados a partir dos starters suportados](screenshots/2026-07-14/project-template-catalog/02-created-projects.png)
