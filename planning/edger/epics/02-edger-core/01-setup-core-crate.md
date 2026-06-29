# Story 02.01: Setup edger-core crate structure

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`  
**Status:** completed (2026-06-29)

## Context
- **Problema:** edger-core carece de estrutura modular; apenas `lib.rs` parcial.
- **Objetivo:** Layout de crate leaf puro conforme design + padrões ai-memory.
- **Valor:** Base para modelos/traits sem ciclos de dependência.
- **Restrições:** Sem deps em crates irmãos; sem I/O.

## Traceability
- **Source docs:** `planning/edger/design.md` (Crate Ownership), `planning/edger/analysis-synthesis.md`
- **Depende de:** Epic 01 (completed)

## Files
| Path | Action | Reason |
|---|---|---|
| `edger-core/Cargo.toml` | alterar | pureza + workspace inherit |
| `edger-core/src/lib.rs` | alterar | módulos + re-exports |
| `edger-core/src/manifest.rs` | criar | stub |
| `edger-core/src/error.rs` | criar | stub |
| `edger-core/src/extension.rs` | criar | stub traits |
| `planning/edger/epics/02-edger-core/00-overview.md` | alterar | status |

## Detail

### AS-IS
`lib.rs` parcial com `ExecutionKind`, `CoreError`, `WorkerManifest` mínimo; testes passam.

### TO-BE
Árvore de módulos via `mod` em `lib.rs` (sem `mod.rs` separado na raiz); pureza documentada; gate Rust no AGENTS.

### Scope
- **In:** Cargo.toml, lib.rs + stubs de módulo, teste básico
- **Out:** modelos completos (story 02.02)

### Acceptance criteria
- [x] edger-core sem path deps em crates irmãos
- [x] `cargo test -p edger-core` passa
- [x] Stubs: manifest, config, wire, error, extension declarados em lib.rs

### Dependencies
- Epic 01 complete

### Notas de implementação
- `lib.rs`: `//! edger-core: pure vocabulary. No I/O.`
- Herdar `[workspace.package]`; deps apenas serde/bytes/tracing conforme design
- Documentar gate em `AGENTS.md`

### Pendências / desvios
- AS-IS tinha tipos inline em `lib.rs`; refatorados para módulos nas stories 02.02–02.04 (comportamento preservado + expandido).

## Test-first plan
- **Red:** `cargo test -p edger-core` falha sem módulos
- **Green:** lib.rs + stubs + teste mínimo
- **Refactor:** separar stubs em arquivos dedicados

## Tasks
- [x] Editar `edger-core/Cargo.toml` (pureza + inherit)
- [x] Criar stubs `manifest.rs`, `error.rs`, `extension.rs` e declarar em `lib.rs`
- [x] Adicionar teste unitário mínimo
- [x] `cargo test -p edger-core` verde
- [x] Atualizar cross-refs no epic overview

## Verification
```bash
cargo check -p edger-core
cargo test -p edger-core
bun test
```