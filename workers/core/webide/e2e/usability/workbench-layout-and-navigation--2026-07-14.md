# Usability — Navegar pelas regiões do workbench (workbench-layout-and-navigation)

- **Persona:** Autor iniciante no EdgeR WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable

## Walkthrough

1. A linha de `static-spa-app-2` abriu o workbench com logo, nome centralizado e quatro ações acessíveis no header.
2. Explorer, tabs, editor, Preview e painel inferior ficaram alinhados; topbar, editor tabs, preview toolbar e header inferior usam 36 px.
3. O hover da tab ativa expôs o title nativo `index.html`, o path completo do arquivo raiz.
4. `Hide preview` removeu preview e splitter e aumentou a largura do editor de 609 px para 1020 px.
5. `Show preview` restaurou a toolbar com `Preview`, refresh e open-in-new-tab, sem status redundante.
6. `Hide panel` removeu splitter e painel inferior e aumentou a área superior de 489 px para 684 px.
7. `Show panel` restaurou a tab `Logs`, que permanecia selecionada.
8. Search e Explorer alternaram a lateral sem trocar `static-spa-app-2`.
9. O logo EdgeR retornou ao dashboard com o rascunho preservado.

## Findings (prioritized)

Nenhum achado acionável neste fluxo.

## Key screens

- [Workbench com todas as regiões visíveis](screenshots/2026-07-14/workbench-layout-and-navigation/01-workbench-layout.png)
