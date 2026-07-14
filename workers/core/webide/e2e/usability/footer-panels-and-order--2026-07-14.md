# Usability — Usar e ordenar os painéis inferiores (footer-panels-and-order)

- **Persona:** Operador da plataforma EdgeR · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable after fix

## Walkthrough

1. `static-spa-app-6` abriu com footer visível, Logs e seu histórico local.
2. Problems mostrou o empty state do manifesto válido.
3. Logs mostrou horário, source, mensagem e o checkbox Preserve logs somente nessa tab.
4. Terminal explicou que o console é operacional e não um host shell.
5. Deployments mostrou os dois deployments recentes.
6. Deployments foi arrastado para antes de Problems.
7. Logs foi arrastado para depois de Terminal sem trocar o conteúdo de Deployments.
8. Hide panel removeu footer e splitter.
9. Show panel restaurou o footer; Logs abriu mantendo a ordem personalizada.
10. Reload preservou visibilidade, Logs e a ordem `Deployments, Problems, Terminal, Logs`.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | blocker | 6 | O fallback pointer-based ignorava toda tab do footer porque o próprio elemento é um button e era confundido com controle aninhado. | Resolvido: ignorar apenas controles descendentes diferentes do elemento reordenável. |
| 2 | major | 3 | O log do segundo deploy mostrava a versão cacheada 1.0.0 embora o preview implantado fosse 1.0.1. | Resolvido: derivar a versão solicitada do manifesto e atualizar `active.version` após validação. A correção será exercitada novamente no fluxo de preservação de logs. |

## Rerun

O fluxo completo foi repetido desde o entry point após o build. Os 10 passos passaram, incluindo duas reordenações por ponteiro e restauração após reload.

## Key screens

- [Footer com ordem personalizada restaurada](screenshots/2026-07-14/footer-panels-and-order/custom-order-restored.png)
