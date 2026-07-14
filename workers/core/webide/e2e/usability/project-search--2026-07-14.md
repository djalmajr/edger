# Usability — Buscar conteúdo em todo o projeto (project-search)

- **Persona:** Desenvolvedor frequente da WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable after fix

## Walkthrough

1. O projeto preparado abriu com `Hello` em `index.html` e `hello` em `greeting.js`.
2. O ícone Search abriu o painel e focou `Search files`.
3. `hello` retornou 2 resultados em 2 arquivos, com linha, trecho e `mark` em cada match.
4. `Match case` reduziu o resultado à ocorrência lowercase e expôs `aria-pressed=true`.
5. `Hello` case-sensitive retornou somente `index.html`.
6. `H[a-z]+o` com regex retornou a ocorrência compatível.
7. `[` exibiu `Invalid regular expression` sem interromper o workbench.
8. Com os toggles desligados, uma consulta ausente exibiu `No results.` sem grupos vazios.
9. `hello` restaurou a busca simples com 2 resultados.
10. O resultado de `index.html` abriu o arquivo e posicionou o cursor na linha 4.
11. `Cmd+Shift+F` a partir do editor reabriu SEARCH com o input focado.
12. Explorer restaurou a árvore mantendo `index.html` selecionado.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | major | 4 | Os toggles eram anunciados apenas como `Aa` e `.*`, sem os nomes funcionais definidos pelo fluxo. | Resolvido: adicionar `aria-label="Match case"` e `aria-label="Use regular expression"`, preservando title e `aria-pressed`. |

## Rerun

O fluxo completo foi repetido desde o entry point após o build. Os 12 passos passaram, incluindo navegação para a linha correta e shortcut global.

## Key screens

- [Resultados agrupados da busca simples](screenshots/2026-07-14/project-search/search-results.png)
