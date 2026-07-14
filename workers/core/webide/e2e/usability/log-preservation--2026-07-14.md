# Usability — Controlar preservação dos logs entre deploys (log-preservation)

- **Persona:** Operador da plataforma EdgeR · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable after catalog correction

## Walkthrough

1. `static-spa-app-7` abriu Logs e o checkbox Preserve logs.
2. O checkbox foi desmarcado.
3. Validate project adicionou um marcador VALIDATE anterior ao deploy.
4. O deploy 1.0.0 limpou Created e o marcador anterior; a lista final conteve somente sua sequência e terminou em sucesso.
5. Preserve logs foi marcado.
6. Um novo Validate adicionou o marcador após a sequência existente.
7. O manifesto foi alterado para 1.0.1 e o segundo deploy preservou o marcador e toda a sequência 1.0.0 acima dos novos eventos.
8. Reload restaurou checkbox marcado e histórico completo.
9. Alternar para Problems e voltar a Logs não alterou preferência nem eventos.
10. Preserve logs foi desmarcado.
11. Reload preservou a preferência desmarcada.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | blocker | 7 | O fluxo original solicitava um segundo deploy sem alterar a versão, incompatível com versões imutáveis do EdgeR; o runtime respondeu `target directory ... already exists`. | Resolvido no catálogo: alterar o manifesto para 1.0.1 antes do segundo deploy. |
| 2 | major | 4/7 | O log de pedido podia usar a versão cacheada do projeto. | Resolvido e verificado: pedidos registraram `@1.0.0` e `@1.0.1` conforme o manifesto de cada deploy. |

## Rerun

O fluxo corrigido foi repetido desde o entry point em projeto novo e aba autenticada. Os 11 passos passaram e o estado final permaneceu desmarcado após reload.

## Key screens

- [Histórico preservado e preferência final desmarcada](screenshots/2026-07-14/log-preservation/final-history-unchecked.png)
