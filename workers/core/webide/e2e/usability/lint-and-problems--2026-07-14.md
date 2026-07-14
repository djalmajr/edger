# Usability — Detectar e navegar por problemas locais (lint-and-problems)

- **Persona:** Desenvolvedor frequente da WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable after fix

## Walkthrough

1. Problems informou que o arquivo válido ativo não tinha diagnósticos.
2. `{ "value": }` em `broken.json` exibiu o erro do parser JSON com arquivo e linha.
3. Um clique no problema focou o editor na linha 1.
4. `export function flow( {` em `broken.ts` exibiu os delimitadores `(` e `{` não fechados.
5. O problema TypeScript focou a linha do delimitador.
6. `<html><body><h1>Flow` listou as três tags HTML não fechadas.
7. Uma tabulação literal em `broken.yaml` exibiu `YAML indentation must use spaces, not tabs.`.
8. Remover `version` do manifesto exibiu o campo obrigatório ausente.
9. `entrypoint: missing.js` exibiu `Entrypoint not found: missing.js`.
10. Restaurar o manifesto eliminou seus diagnósticos sem deploy.
11. Os quatro arquivos inválidos foram excluídos pelos menus shadcn; árvore e tabs ficaram limpas.
12. Problems terminou com `No problems detected in the active file.` para `manifest.yaml`.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | blocker | 4 | O lint calculava delimitadores corretamente, mas o conteúdo visível de Problems não era atualizado durante a digitação; arquivos inicialmente vazios continuavam mostrando o estado antigo. | Resolvido: atualizar `#console-content` e religar a navegação dos problemas a cada input quando Problems está ativo. |

## Rerun

O fluxo completo foi repetido desde o entry point após o build. Os 12 passos passaram e os arquivos inválidos foram removidos ao final.

## Key screens

- [Problems limpo com manifesto válido](screenshots/2026-07-14/lint-and-problems/final-clean-problems.png)
