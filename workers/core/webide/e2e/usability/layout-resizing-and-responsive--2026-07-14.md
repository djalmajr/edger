# Usability — Redimensionar o workbench e usar viewports compactos (layout-resizing-and-responsive)

- **Persona:** Auditor UI/UX responsivo · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable after fixes

## Walkthrough

1. O projeto preparado abriu no desktop com Explorer, editor, Preview e footer sem overflow horizontal.
2. O splitter do Explorer respeitou o limite mínimo de 180 px.
3. O mesmo splitter respeitou o limite máximo de 420 px.
4. O Explorer foi deixado em 261 px e o Preview em 40,29%, dentro do intervalo de 24%–65%.
5. O footer foi ajustado para 35,09%, dentro do intervalo de 16%–48%.
6. Um reload restaurou as três medidas pelo armazenamento local.
7. Em 900 px, dashboard e workbench permaneceram sem overflow; o dashboard virou rail compacta e editor/Preview continuaram lado a lado.
8. Dashboard e Projects continuaram navegáveis por botões com nomes acessíveis mesmo sem labels visuais.
9. Em 700 px, o Explorer foi recolhido e o Preview passou para baixo do editor.
10. Preview e footer foram ocultados e reabertos pelos toggles no viewport compacto.
11. `Cmd+Shift+F` abriu SEARCH como painel sobreposto, focou `workspace-search` e preservou os 700 px sem overflow.
12. O viewport desktop foi restaurado sem perda do projeto, seleção, conteúdo ou medidas persistidas.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | blocker | 2–5 | Os splitters dependiam de pointer capture no elemento; o Browser não entregava o gesto completo e os limites não podiam ser exercitados. | Resolvido: acompanhar `pointermove`, `pointerup` e `pointercancel` no documento durante o gesto e persistir somente no encerramento. |
| 2 | major | 11 | O breakpoint de 720 px ocultava o Explorer incondicionalmente, então o atalho abria SEARCH no estado mas deixava o input invisível. | Resolvido: expor SEARCH como overlay shadcn-style ancorado à activity rail no viewport compacto. |
| 3 | major | 8 | Depois da migração para Button, os ícones compactos da navegação não tinham nome acessível quando o texto era ocultado por CSS. | Resolvido: adicionar `aria-label` e `aria-current` aos triggers Dashboard e Projects. |
| 4 | major | 7 | O selector antigo de close aplicou `opacity: 0` também ao novo `TabsTrigger`. | Resolvido: separar o trigger shadcn do botão Close e limitar a regra a `[data-close-tab]`. |

## Rerun

O fluxo completo foi repetido após as correções. Os 12 passos passaram, incluindo persistência dos três splitters, navegação em 900 px, stack em 700 px, toggles e busca por atalho.

## Key screens

- [Busca sobreposta no viewport de 700 px](screenshots/2026-07-14/layout-resizing-and-responsive/compact-search.png)
