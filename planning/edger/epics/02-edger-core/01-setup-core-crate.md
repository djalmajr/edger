# Story 02.01: Setup edger-core crate structure

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`

## Context
- **Problem:** edger-core lacks module structure; partial lib.rs only.
- **Objective:** Pure leaf crate layout per design + ai-memory patterns.
- **Value:** Foundation for all models/traits without cycles.
- **Constraints:** No sibling deps; no I/O crates.

## Traceability
- **Source docs:** `planning/edger/design.md` (Crate Ownership), `planning/edger/analysis-synthesis.md`
- **Depends on:** Epic 01 (completed)

## Files
- edger-core/Cargo.toml (edit)
- edger-core/src/lib.rs (create)
- edger-core/src/mod.rs or submods if needed (manifest.rs, error.rs, traits.rs stubs)
- Update root Cargo if resolver needed
- planning/edger/epics/02-edger-core/00-overview.md (status)

## Detail

### AS-IS
Partial `lib.rs` with ExecutionKind, CoreError, minimal WorkerManifest; tests pass.

### TO-BE
Modular `src/` with stub modules, workspace purity enforced, AGENTS Rust gate documented.

### Scope
- In: Cargo.toml purity, lib.rs module tree, basic test
- Out: full models (02.02)

### Acceptance criteria
- [ ] edger-core has zero path deps on sibling crates
- [ ] `cargo test -p edger-core` passes
- [ ] Module stubs exist for manifest, config, wire, error, extension

### Dependencies
- Epic 01 complete

## Detail (implementation notes)
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
