# Usability — Operar e ordenar tabs do editor (editor-tabs-and-order)

- **Persona:** Desenvolvedor frequente da WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable after fixes

## Walkthrough

1. `react-app` abriu restaurando a tab `index.html`.
2. Cliques em `app.js` e `manifest.yaml` criaram exatamente uma tab por arquivo, sem duplicatas.
3. Cada tab expôs o path completo no atributo `title`; o runner não oferece uma primitiva de hover, então o contrato do tooltip foi verificado diretamente.
4. Uma tab inativa foi ativada e o editor passou a mostrar seu arquivo.
5. `Enter` sobre outra tab focada ativou o arquivo pelo teclado.
6. O gesto de arraste moveu `manifest.yaml` para antes da primeira tab.
7. Um segundo arraste moveu a tab atual para depois da última.
8. O botão Close de uma tab inativa removeu somente a tab e preservou o arquivo no Explorer.
9. O clique do meio fechou outra tab sem alterar o conteúdo do arquivo ativo.
10. Fechar a última tab exibiu `Open a file from the Explorer.`.
11. Um clique no Explorer recriou a tab e o editor.
12. Um reload imediato restaurou seleção e tabs abertas.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | blocker | 6 | A ordenação dependia somente de HTML5 Drag and Drop; um gesto real de ponteiro ativava a tab, mas não disparava o drop. | Resolvido: implementar reorder por Pointer Events com threshold e indicadores before/after; aplicar também às tabs do footer. |
| 2 | major | 12 | Seleção e tabs abertas usavam o debounce de conteúdo; um reload imediato restaurava `index.html` em vez da tab recém-aberta. | Resolvido: persistir o estado de navegação de forma síncrona por projeto e espelhá-lo no IndexedDB. |

## Rerun

O fluxo completo foi repetido desde o entry point após cada correção. Os 12 passos passaram no rerun final, incluindo duas reordenações por ponteiro e reload imediato.

## Key screens

- [Tab restaurada após reload imediato](screenshots/2026-07-14/editor-tabs-and-order/final-restored-tab.png)
