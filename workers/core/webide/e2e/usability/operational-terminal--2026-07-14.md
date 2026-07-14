# Usability — Usar o terminal operacional seguro (operational-terminal)

- **Persona:** Pesquisador adversarial de segurança · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable

## Walkthrough

1. Terminal exibiu o banner de escopo e `Operational command`.
2. `help` listou somente help, validate, deploy, preview, files, status e clear.
3. `files` mostrou apenas `index.html` e `manifest.yaml`.
4. `status` mostrou projeto, versão, draft salvo e ausência de preview.
5. `preview` informou que não havia deployment bem-sucedido.
6. `validate` retornou `Valid: static-spa-app-8@1.0.0` sem deploy.
7. `pwd` foi rejeitado sem expor path do host.
8. `ls` também foi rejeitado.
9. Uma linha vazia não adicionou entrada ao histórico.
10. `clear` removeu todas as entradas visuais.
11. `help` voltou a funcionar após clear.
12. `deploy` registrou `Starting explicit EdgeR deployment…` e mudou para Deployments.
13. As sete etapas concluíram com Succeeded.
14. Ao voltar, Terminal continuou restrito e operacional.

## Findings (prioritized)

Nenhum finding de segurança neste fluxo.

## Key screens

- [Terminal restrito e funcional após deploy](screenshots/2026-07-14/operational-terminal/restricted-after-deploy.png)
