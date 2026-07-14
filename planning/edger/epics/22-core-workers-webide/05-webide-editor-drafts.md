# Story 22.05: Editor e rascunhos locais da WebIDE

**Origin:** `planning/edger/epics/22-core-workers-webide/00-overview.md`

## Context

Criar autoria EdgeR-native para FetchHandler, RoutesTable, StaticSpa e arquivos
genéricos, sem Nodepod nem alteração automática do runtime.

## Files

- `workers/core/webide/src/`
- `workers/core/webide/dist/`
- `workers/core/webide/manifest.yaml`

## Detail

O snapshot editável vive no browser. Autosave é estritamente local em IndexedDB
e a versão continua explícita no manifesto.

## Tasks

- [x] Criar templates e editor de arquivos genéricos.
- [x] Implementar persistência e restauração de rascunhos.
- [x] Manter autosave separado do deploy administrativo.
- [x] Validar restauração e responsividade no Browser.

## Acceptance criteria

- [x] Templates e importação genérica produzem snapshot de arquivos + manifesto.
- [x] IndexedDB restaura rascunho após refresh/reabertura.
- [x] Autosave não chama Admin API de deploy.
- [x] Versão permanece explícita no manifesto.
- [x] Comportamento e responsividade foram validados no Browser.

## Verification

- `deno check workers/core/webide/src/app.js`.
- E2E no Browser com refresh e reabertura.

## Status

completed (2026-07-13).
