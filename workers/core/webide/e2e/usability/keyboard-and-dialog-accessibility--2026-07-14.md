# Usability — Operar navegação e dialogs por teclado (keyboard-and-dialog-accessibility)

- **Persona:** Desenvolvedor que usa tecnologia assistiva · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable after fixes

## Walkthrough

1. New project abriu um Dialog shadcn com nome `Create a new project`; o primeiro template Ready recebeu foco.
2. Escape fechou o Dialog e devolveu foco para New project.
3. Backend atualizou o tabpanel; Frontend/Fullstack expuseram `aria-selected="false"` e Backend `aria-selected="true"`.
4. Close encerrou o Dialog, que expôs `DialogTitle` e `DialogDescription` associados.
5. Enter na TableRow rotulada `Open project static-spa-app-8` abriu o workbench.
6. As quatro ações do header anunciaram Hide preview, Hide panel, Validate project e Deploy project.
7. Espaço em `index.html` ativou o TabsTrigger e atualizou o editor.
8. A tab expôs o path completo em `title` e em Tooltip focável.
9. `Cmd+Shift+F` abriu SEARCH e focou `workspace-search`.
10. A consulta `Hello` encontrou o match da linha 4; sua ativação abriu `index.html` e posicionou o editor.
11. New file abriu Dialog, focou o campo Name e criou `keyboard-test.txt`.
12. Cancel devolveu o foco a New file; um clique real fora do conteúdo fechou a segunda abertura pelo overlay.
13. `Cmd+S` persistiu `saved from keyboard accessibility flow`; após sair e reabrir o projeto, o conteúdo foi restaurado e o logo retornou ao dashboard.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | major | 2, 12 | Os dialogs anteriores focavam o primeiro campo, mas não continham Tab nem devolviam foco em todos os caminhos de saída; dialogs de arquivo também ignoravam Escape. | Resolvido: implementar ciclo de foco, Escape, seletor de retorno e um único `closeFileDialog` para Cancel/overlay/Escape. |
| 2 | major | 1–4 | Os overlays eram apenas marcação manual com `data-slot`, sem composição centralizada de Dialog/AlertDialog. | Resolvido: compor DialogContent/Header/Title/Description/Footer e AlertDialog equivalentes a partir de `components/ui`. |
| 3 | minor | 3 | Booleanos ARIA verdadeiros eram emitidos como atributos vazios e valores falsos eram removidos. | Resolvido: serializar `aria-*` e `data-*` booleanos explicitamente como `"true"`/`"false"`. |

## Runner notes

O locator do Browser envia key events, mas não executa a ação padrão de Enter/Espaço em botões nativos. A ativação foi validada por semântica `button`, foco e clique visível; handlers explícitos de teclado (TableRow, TabsTrigger, Escape e Search) foram exercitados diretamente. O fechamento por overlay foi confirmado com clique real por coordenada.

## Rerun

O fluxo completo foi repetido após as correções. Os 13 passos passaram, incluindo retorno de foco, estados ARIA, ativação de row/tab, busca, criação/cancelamento de arquivo, overlay e persistência após `Cmd+S`.

## Key screens

- [Workbench operado por teclado](screenshots/2026-07-14/keyboard-and-dialog-accessibility/workbench-keyboard.png)
