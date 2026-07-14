# Story 22.01: Reorganizar crates sem alterar comportamento

**Origin:** `planning/edger/epics/22-core-workers-webide/00-overview.md`

## Context

Mover crates para `crates/`, preservando package names, dependências, comandos,
testes e contratos públicos.

## Files

- `Cargo.toml`
- `crates/`
- Referências ativas em scripts, CI e documentação

## Detail

A mudança é exclusivamente estrutural: nomes de packages, APIs Rust e comandos
Cargo permanecem estáveis.

## Tasks

- [x] Mover os cinco crates para `crates/`.
- [x] Atualizar workspace e referências ativas.
- [x] Confirmar os package names com `cargo metadata`.
- [x] Executar o gate Rust completo após o restante do epic.

## Acceptance criteria

- [x] Workspace aponta para `crates/edger-*`.
- [x] `cargo metadata` mantém os package names.
- [x] Referências ativas em CI, scripts e documentação usam os novos paths.
- [x] Gate Rust completo passa após todas as mudanças do epic.

## Verification

- `cargo metadata --no-deps`
- Gate Rust obrigatório do workspace.

## Status

completed (2026-07-13).
