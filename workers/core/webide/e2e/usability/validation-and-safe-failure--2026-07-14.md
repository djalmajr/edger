# Usability — Validar projeto sem realizar deploy (validation-and-safe-failure)

- **Persona:** Engenheira de confiabilidade de releases · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable

## Walkthrough

1. Validate project abriu Logs e registrou `Project validation passed.`.
2. Deployments permaneceu em `No deployments yet.`.
3. Remover name registrou `Manifest name and version are required`.
4. `name: Invalid Name` registrou a exigência URL-safe.
5. `entrypoint: missing.js` registrou que o entrypoint deve existir.
6. Restaurar o manifesto válido voltou a registrar sucesso.
7. Deployments continuou vazio durante toda a sequência.
8. Preview permaneceu no empty state.
9. Reload restaurou os eventos locais de sucesso e falha em Logs.
10. Deploy permaneceu habilitado e separado de Validate.

## Findings (prioritized)

Nenhum finding de produto neste fluxo.

## Key screens

- [Logs de validação persistidos após reload](screenshots/2026-07-14/validation-and-safe-failure/persisted-validation-logs.png)
