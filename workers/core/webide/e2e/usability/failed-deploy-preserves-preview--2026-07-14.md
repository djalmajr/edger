# Usability — Preservar o preview anterior quando o deploy falha (failed-deploy-preserves-preview)

- **Persona:** Engenheira de confiabilidade de releases · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable

## Walkthrough

1. O Preview saudável respondeu em `/static-spa-app-7@1.0.1` antes da tentativa.
2. Deployments mostrou dois baselines Succeeded.
3. `entrypoint: missing.js` foi salvo localmente sem alterar iframe ou link externo.
4. O deploy falhou em Validation; as seis etapas seguintes permaneceram pendentes.
5. O histórico adicionou Failed com `Manifest entrypoint must exist in the project`.
6. Logs registrou o erro DEPLOY e nenhuma mensagem de conclusão/ativação para a tentativa.
7. Preview, heading e link continuaram em 1.0.1.
8. O manifesto válido foi restaurado e salvo sem deploy automático.
9. Validate project registrou sucesso, mantendo o preview baseline.
10. Deployments preservou os dois sucessos e a tentativa Failed.

## Findings (prioritized)

Nenhum finding de produto neste fluxo.

## Key screens

- [Tentativa Failed auditável com preview saudável preservado](screenshots/2026-07-14/failed-deploy-preserves-preview/failed-history-healthy-preview.png)
