# Story 02.01: Setup edger-core crate structure

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`

## Context
Create proper pure core crate layout per design and ai-memory patterns (core vocabulary only).

## Files
- edger-core/Cargo.toml (edit)
- edger-core/src/lib.rs (create)
- edger-core/src/mod.rs or submods if needed (manifest.rs, error.rs, traits.rs stubs)
- Update root Cargo if resolver needed
- planning/edger/epics/02-edger-core/00-overview.md (status)

## Detail
- Ensure no deps on worker/isolation/orchestrator.
- Add workspace.package inherit.
- lib.rs: //! edger-core: pure vocabulary (types, traits, errors, manifests). No I/O.
- Stubs for re-exports.
- Cargo test skeleton passes.
- Document in AGENTS.

## Test-first plan
- Red: cargo test fails (no lib)
- Green: minimal lib.rs + Cargo with [[test]] or lib test
- Refactor: clean modules

## Tasks
- [ ] Edit edger-core/Cargo.toml for purity + inherit
- [ ] Create src/lib.rs with pure marker + pub use
- [ ] Add basic unit test in lib or tests/
- [ ] cargo test -p edger-core green
- [ ] Update docs cross ref

## Verification
- cargo check -p edger-core
- cargo test -p edger-core
- memory_lint (djalmajr/edger)
- bun test (unchanged)
- refinement on new epic
