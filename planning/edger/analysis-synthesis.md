# Edger: Consolidated Analysis & Plan Integration

**Date:** 2026-06-28  
**Purpose:** Synthesis of Buntime vision, Edge Runtime structure, ai-memory learnings, current edger skeleton, intake.md, and design.md. This document serves as the bridge to create a realistic, high-quality implementation plan (roadmap / updated PRs).

## 1. Executive Summary

Edger is positioned correctly as a **new independent project** that takes:
- **Product Vision & Contracts** from Buntime (workers as first-class units, manifests, namespaces/roles/permissions, shell/micro-frontends, extensibility for separation of concerns, multi-tenancy).
- **Engineering Structure & Primitives** from Edge Runtime (multi-crate layout, isolates + supervisor, resource limits, core vs extension separation).
- **Maturity Patterns** from ai-memory (workspace hygiene, "core = vocabulary only", testing organization, development discipline, single-actor patterns, documentation culture).

**Current Reality (as of now):**
- Skeleton exists (workspace + 4 crates with only `Cargo.toml`).
- Strong design.md already produced (12 PRs, crate ownership table, Key Decisions, Risks, PR Plan).
- Intake.md exists with clear problem statement.

**Main Recommendation:** The plan is already good, but can be significantly strengthened by adopting ai-memory's engineering hygiene, test patterns, and "core-first" philosophy more explicitly. PR 1 should be the highest-leverage starting point.

---

## 2. Current Project State

### Skeleton (real checkout)
- Root `Cargo.toml` with basic workspace + 4 members.
- Each `edger-*/Cargo.toml` exists (some have inverted dependencies, e.g. core depending on worker).
- No `src/` yet.
- README in Portuguese with slightly inconsistent crate descriptions.

### Intake.md Highlights
- Clear problem (Buntime Bun Worker limitations for SSR/full-stack).
- Objective: Rust orchestrator + Buntime vision + Edge Runtime crate structure.
- Good non-goals.
- Open questions list is still relevant.
- Recommends `/agile-roadmap`.

### Design.md Highlights (Post-Review)
- Excellent crate ownership table and corrected dependency direction (core as leaf).
- Detailed request flow, worker lifecycle, extension registration (Mermaid).
- Data models, wire formats, main.rs sketch.
- 10+ Key Decisions.
- 12 PRs (starts with alignment + embedding spike — realistic).
- Risks & Mitigations table.
- Development Discipline section (already references adapting Buntime AGENTS.md gate).
- PR Plan explicitly notes "starts from existing skeleton".

**Strength:** The design already incorporated many good ideas from the conversation. The review loop improved specificity and realism.

---

## 3. Buntime Vision — What Must Be Preserved / Translated

Core contracts that edger must support (verified from buntime source + wiki):

- **Workers** as deployable units with manifest-driven behavior (`entrypoint`, `ttl` (0=ephemeral + sliding), `timeout`, `maxRequests`, `publicRoutes`, `cron`, `enabled`, env filtering).
- Worker addressing: `/name`, `/@scope/name`, `/@scope/name@ver`.
- Execution kinds: `fetch` handler, routes table, static SPA (with `<base href>` injection), Wasm.
- **Auth model**: `ApiKeyPrincipal` with `namespaces`, `role`, permissions; root key; public routes bypass; namespace gating.
- **Plugin/Extension model**: Separation of concerns. "Choose ONE API mode" principle. Topological ordering for hooks. `onRequest` (can short-circuit), `onResponse`, `onInit`, provides.
- Shell / micro-frontends (base injection + communication).
- Collision detection, health, metrics, eviction.
- Multi-tenant via `workerDirs` (PATH `:` style).

**Translation to Rust:**
- Manifests → strong serde types in `edger-core`.
- Hooks → traits + registry in orchestrator (topo or priority order in Rust).
- WorkerPool → `edger-worker` (LRU + states + supervisor).
- Auth → early gate in pipeline.

---

## 4. Structural Learnings from Edge Runtime + ai-memory

### Edge Runtime (what to keep)
- Crate separation (core types vs execution vs orchestration).
- Main vs User worker mental model (but internalize orchestration in Rust instead of delegating to JS main-service).
- Isolate + Supervisor + resource limits (`cpu_timer` inspiration).
- Strong focus on cold starts, supervision, and safety boundaries.

### ai-memory (highest value transfer — more mature than Edge Runtime in engineering hygiene)

**Workspace & Crate Patterns (directly copy/adapt):**
- `resolver = "3"`
- `[workspace.package]` + `[workspace.dependencies]` (internal + external)
- `crates/<name>/` layout
- `ai-memory-core` as **pure vocabulary crate** (no I/O, only types/IDs/traits/errors). This is the gold standard. `lib.rs` starts with a clear comment about its purpose.
- Clear ownership: core (types) → store/execution → higher crates → thin CLI.

**Testing Patterns (adopt aggressively):**
- Integration tests live in `crate/tests/*.rs` (not mixed in src).
- Heavy use of `tempfile::TempDir` for isolation.
- Tests recreate production wiring (example: full axum Router + service exactly as in prod code).
- "Focused regression tests for bug fixes and behavior changes."
- Explicit table-driven tests for isolation, auth, scope, boundary conditions.

**Development Discipline (very strong — merge with buntime rules):**
From ai-memory AGENTS.md (highly recommended to adopt/adapt):

- Small, behavior-preserving changes.
- Run full local gate before claiming ready:
  ```bash
  cargo fmt --check
  git diff --check
  cargo test --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  ```
- "Add focused regression tests..."
- Preserve isolation boundaries (in edger: worker/extension boundaries, scope/auth equivalents).
- Core = pure, no I/O.
- Single writer actor pattern for mutations (excellent for reliable WorkerPool/orchestrator).
- Thin CLI/handlers (parse + delegate to typed libs).

**Extensibility:**
- Dedicated `ai-memory-hooks` crate.
- Skills have `SKILL.md` metadata.
- Clear "choose one responsibility" principle.

**Documentation Culture:**
- Single canonical `AGENTS.md`.
- `docs/ARCHITECTURE.md` (operational map).
- `docs/design-decisions.md`.
- Every user-visible or behavior change → updates CHANGELOG + relevant docs.

**Other:**
- `deny.toml` for security.
- Separate `evals/` member in workspace.
- Migrations in store crate.

---

## 5. Consolidated Recommendations for the Plan

### 5.1 Crate Model (Refined)
Keep the design's ownership table. Add:

- `edger-core` → pure types + traits only (copy ai-memory-core style).
- Consider adding `edger-store` later if we need durable state (auth, manifests, metrics) — use single writer actor pattern.
- Extension crates (`edger-ext-*`) should be first-class and follow "one responsibility" + trait implementation.

### 5.2 Testing Strategy (New Strong Recommendation)
- Adopt `crate/tests/` for integration.
- Every PR that touches behavior must add focused regression test.
- Use temp dirs.
- Write tests that spin up real pipeline (orchestrator + pool + mock/real isolate).
- Enforce the full cargo gate in AGENTS.md and CI.

### 5.3 Development Discipline (Merge Best of Both)
Create `AGENTS.md` for edger early (single canonical file).

Combine:
- ai-memory's local gate + "small changes" + regression tests.
- Buntime's "test before complete", "leave tree cleaner", no emojis, etc.
- Explicit rule: "Run the gate before claiming a change is ready."

Add:
- Core crate must remain free of I/O.
- Preserve worker/extension isolation boundaries.
- Markdown/docs as source of truth where applicable.

### 5.4 Architecture Patterns to Adopt
- **Core as closed vocabulary** (already in design — make it stricter).
- Single actor for mutation-heavy paths (WorkerPool, manifest loading, hook execution).
- Thin orchestrator handlers.
- Strong typed boundaries (WorkerRef, Principal, ExecutionKind, etc.).
- Static registration for extensions initially (inventory/linkme) — as decided by user.

### 5.5 PR Plan Evolution
The current 12-PR plan in design.md is good. Recommendations:
- PR 1 (align skeleton) must also introduce basic `src/lib.rs` + module structure + ownership comments + start of AGENTS.md.
- Early PRs should include "add test infrastructure" (tempfile, integration test skeleton).
- Embedding spike (PR 2) should explicitly evaluate against user decisions (deno_core + facade for JS/TS, standalone wasmtime for Wasm).
- Add a dedicated early chore PR or task for "establish development discipline + CI gate" if not covered.
- Ensure every PR that changes contracts updates a "Buntime Compatibility Matrix" (or equivalent in docs).

### 5.6 Documentation
- Create `docs/ARCHITECTURE.md` early (inspired by ai-memory).
- Create `docs/design-decisions.md` (or keep evolving the Key Decisions section).
- Keep design.md + intake.md as living references.
- Update CHANGELOG on every meaningful change.

### 5.7 Open Questions (Consolidated + User Decisions Applied)
From intake + design + user input:

**Decided (user input from previous session):**
- Embedding: deno_core + facade (primary for JS/TS) + standalone wasmtime + WASI.
- Extensions: static v1, defer dynamic.
- Node compat: partial (document gaps).
- Bundling/cold starts: port eszip_trait style + precomp.
- Auth persistence: immediate Turso/SQLite.
- Shell: evolve to more efficient (e.g. WebTransport).
- Cron: native Rust scheduler.
- Perf: define in PR 12.
- Multi-process: from early PRs.

**Still Open (need tracking):**
- Exact bundling story for JS + Wasm.
- How deep the "manifest" vs "Rust config" mapping goes.
- When (if ever) to add dynamic extension loading.
- Performance targets (after spike).

---

## 6. Immediate Recommended Actions

1. **Create/Update Planning Artifacts**
   - This file (`analysis-synthesis.md`) as the integration point.
   - Run `/agile-roadmap` using this + design.md + intake as input.
   - Create `docs/ARCHITECTURE.md` skeleton.

2. **Start with PR 1 (highest leverage)**
   - Align skeleton + deps.
   - Add `src/lib.rs` + ownership comments in every crate.
   - Introduce basic test harness (`tests/` + tempfile).
   - Add initial `AGENTS.md` with the merged discipline gate.
   - Sync README.

3. **Early Chores (parallel or in PR 1/2)**
   - Add `deny.toml`.
   - Set up basic CI that runs the full gate.
   - Decide on `edger-core` as pure-vocabulary crate and enforce in code review.

4. **Next after foundation**
   - Embedding spike (respecting user decisions).
   - Core types + manifests (with Buntime mapping tests).

---

## 7. Risks if We Ignore These Learnings

- Repeating Buntime's "orchestration in userland" fragility in Rust form.
- Weak test coverage for isolation/auth boundaries (ai-memory is very strong here).
- Inconsistent development hygiene across the monorepo family (buntime + edger).
- Slower iteration because of missing "small change + regression test" culture.
- Harder onboarding (no clear ARCHITECTURE.md + canonical AGENTS.md).

---

## 8. Conclusion & Next Step Proposal

The combination of:
- Buntime vision (contracts + extensibility motivation)
- Edge Runtime low-level primitives + crate separation
- ai-memory engineering maturity (core purity, testing, discipline, documentation)

gives edger a very strong foundation.

**Recommended immediate next artifact:** `/agile-roadmap` (or a focused "Foundation Roadmap" epic).

**Completed (2026-06-29):** roadmap Fases 1–7 decompostas em 7 epics / 31 stories; backlog `ready-for-development`.

This analysis remains living — iterate as implementation surfaces new evidence.