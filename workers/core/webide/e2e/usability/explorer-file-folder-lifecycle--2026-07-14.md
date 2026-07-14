# Usability — Organizar arquivos e pastas pelo Explorer (explorer-file-folder-lifecycle)

- **Persona:** Desenvolvedor frequente da WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable after fix

## Walkthrough

1. Um projeto `Static SPA` descartável abriu com o Explorer ativo.
2. `New folder` criou a pasta vazia `src`.
3. O menu de `src` criou `src/components`, mantendo a guia visual de nesting.
4. O menu de `components` criou `button.ts`, selecionou o arquivo e abriu sua tab.
5. `New file` criou `notes.md` na raiz e exibiu seu ícone Markdown.
6. A tentativa de recriar `notes.md` manteve o Dialog aberto com `Path already exists: notes.md`.
7. O path `../escape.js` foi rejeitado com `Invalid project file path: ../escape.js`.
8. `Cancel` fechou o erro sem alterar a árvore.
9. Cliques sucessivos em `src` recolheram e restauraram seus descendentes.
10. O clique direito em `button.ts` ofereceu `Rename` e `Delete`.
11. O menu fechou ao perder contexto e as reticências ofereceram as mesmas ações.
12. O rename para `button-renamed.ts` atualizou path, tab, `title` completo e seleção.
13. O rename de `components` para `ui` migrou pasta, descendente, tab, `title` e seleção para `src/ui/button-renamed.ts`.
14. O primeiro delete de `notes.md` abriu um AlertDialog shadcn; `Cancel` preservou o arquivo.
15. A confirmação seguinte removeu `notes.md` e sua tab.
16. A confirmação do delete de `src` removeu pasta, descendentes, tabs relacionadas e selecionou `index.html` como fallback.
17. O reload preservou as exclusões sem paths ou tabs órfãos.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | major | 12 | Renomear um arquivo pelo menu de contexto atualizava a árvore e a tab, mas mantinha `notes.md` selecionado. | Resolvido: selecionar e persistir o arquivo alvo ao iniciar o rename pelo menu de contexto. |

## Rerun

O fluxo completo foi repetido desde o entry point em um novo projeto descartável após o build. Os 17 passos passaram; o arquivo renomeado ficou selecionado e a persistência final foi confirmada após reload.

## Key screens

- [Estado final persistido após exclusões](screenshots/2026-07-14/explorer-file-folder-lifecycle/final-persisted-state.png)
