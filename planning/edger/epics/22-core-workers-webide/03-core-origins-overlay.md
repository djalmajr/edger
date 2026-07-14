# Story 22.03: Origens, overlays e invariantes de workers core

**Origin:** `planning/edger/epics/22-core-workers-webide/00-overview.md`

## Context

Derivar confiança pelo root de carregamento, reservar apps core e permitir
atualizações persistentes sem perder a versão bundled.

## Files

- `crates/edger-core/src/admin.rs`
- `crates/edger-orchestrator/src/manifest_index_stub.rs`
- `crates/edger-orchestrator/src/manifest_loader.rs`
- `crates/edger-orchestrator/src/deploy.rs`
- `crates/edger-orchestrator/src/admin_api.rs`

## Detail

A origem é derivada do root confiável, nunca do pacote enviado. Overlays core
passam pelo mesmo release, migrations, health gate, ativação e rollback do
install existente.

## Tasks

- [x] Modelar e expor a origem administrativa.
- [x] Aplicar precedência e reservas de identidade.
- [x] Generalizar o invariante da última versão core válida.
- [x] Rotear ZIPs core para o overlay e preservar rollback atômico.

## Acceptance criteria

- [x] Admin inventory expõe `core_bundled`, `core_overlay` ou `user`.
- [x] Manifesto não controla origem.
- [x] User root não pode reivindicar cPanel/WebIDE ou seus pathnames.
- [x] Bundled é imutável e a última versão core não pode ser desabilitada.
- [x] Install de nome core escolhe overlay e preserva rollback.

## Verification

- Testes `manifest_loader`, `deploy_install` e `admin_endpoints`.

## Status

completed (2026-07-13).
