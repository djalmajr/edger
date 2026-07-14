# Story 22.02: Separar workers core, exemplos e fixtures

**Origin:** `planning/edger/epics/22-core-workers-webide/00-overview.md`

## Context

Usar `workers/core`, `workers/examples` e `tests/fixtures` como fronteiras
explícitas de distribuição e teste.

## Files

- `workers/core/`
- `workers/examples/`
- `tests/fixtures/`
- Scripts e documentação que referenciam workers

## Detail

Apps distribuídos com o produto ficam separados dos exemplos executáveis e das
fixtures de teste, sem alterar os manifestos aceitos pelo runtime.

## Tasks

- [x] Mover cPanel para `workers/core` e adicionar WebIDE.
- [x] Mover demos para `workers/examples`.
- [x] Mover a fixture E2E para `tests/fixtures`.
- [x] Confirmar ausência de referências ativas obsoletas.

## Acceptance criteria

- [x] cPanel e WebIDE vivem em `workers/core`.
- [x] Demos e compatibilidade vivem em `workers/examples`.
- [x] Fixture E2E independente vive em `tests/fixtures`.
- [x] Scripts, docs e gates não mantêm paths ativos obsoletos.

## Verification

- Path preflight do planejamento.
- Smoke de descoberta com os três roots.

## Status

completed (2026-07-13).
