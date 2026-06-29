# Epic 02: edger-core (Vocabulário Puro + Tipos)

**Origin:** `planning/edger/roadmap.md`

## Context
- Problem: After Fase 1 functional Bun loader + examples, the Rust foundation (skeleton) has no core vocabulary. Higher crates need pure types/traits first.
- Objective: Establish edger-core as pure leaf crate (no I/O, no sibling deps) owning models, errors, traits, wire formats, manifests per design + ai-memory patterns (core = vocab).
- Value: Shared language for all later Fases; enables isolation, worker, orchestrator impls without cycles. Tests gate from day 1.
- Constraints: Pure (no tokio/fs in core); follow design ownership table; small incremental; update planning docs; use explicit edger memory scope; keep Bun adapter working.

## Story backlog
- 02.01: Setup edger-core crate (fix Cargo, add src/lib.rs + mod structure, AGENTS gate for Rust).
- 02.02: Core models: WorkerManifest, WorkerConfig, ExecutionKind, Principal, basic serde.
- 02.03: Errors (typed from @buntime/shared inspiration + Rust), wire types (SerializedRequest/Response).
- 02.04: Core traits (Isolate, Extension, Middleware, AuthProvider) + pure unit tests + cargo test gate.

## Roadmap
1. Crate skeleton + purity (02.01)
2. Data models + serde (02.02)
3. Errors + wire (02.03) parallel ok
4. Traits + tests (02.04)
(Reference design.md for details/ownership.)

## Epic acceptance criteria
- edger-core/Cargo.toml correct (no sibling deps, workspace ok).
- src/lib.rs declares public API (Manifests, Errors, Traits, ExecutionKind) with docs.
- `cargo test -p edger-core` (or workspace) passes with unit tests for models/traits (mocks).
- No I/O, no external runtime crates in core (pure).
- planning updated (roadmap points here, cross-refs ok).
- memory_lint + refinement clean for new artifacts.
- Bun side unchanged (still passes).

## Risks
- Over-defining traits too early (mitigate: minimal viable from design, evolve in later Fases).
- Serde versions mismatch with higher (pin in workspace).
- Drift from Buntime contracts (reference design + intake).

## Traceability
- Design: crate ownership, data models, traits.
- Roadmap Fase 2.
- Later: isolation will impl Isolate from core.

## Status
in-progress
