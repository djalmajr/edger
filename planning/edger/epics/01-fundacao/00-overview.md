# Epic 01: Fundação e Alinhamento de Estrutura + Engenharia (edger)

**Origin:** `planning/edger/roadmap.md`

## Traceability
- **Source docs:** `planning/edger/intake.md`, `planning/edger/design.md`, `planning/edger/analysis-synthesis.md`
- **Roadmap phase:** Fase 1 (Fundação)
- **Status artifact:** `planning/edger/status/closure-2026-06-28-edger-func.md`

## Context
- Problem: Edger skeleton exists but no real loader/runtime, no AGENTS discipline, no tests, no support for edge-runtime examples with Deno.serve compatible index entrypoints.
- Objective: Align skeleton, establish engineering gates from ai-memory + buntime, implement core loadWorkerHandler that supports the required index patterns, copy and run several examples.
- Value: Functional edger app that can serve the examples, docs clean, tests pass, launches repeatable.
- Constraints: New project; use Bun for functional (Rust skeleton placeholder); preserve example verbatim indexes; explicit memory scopes for buntime project.

## Story backlog

| Story | File | Size | Status | Depends on |
|---|---|---|---|---|
| 01.01 Setup discipline | `01-setup-discipline.md` | small | completed | -- |
| 01.02 Implement loader | `02-implement-loader.md` | medium | completed | 01.01 |
| 01.03 Copy examples | `03-copy-examples.md` | medium | completed | 01.02 |
| 01.04 Closure evidence | `04-closure-evidence.md` | small | completed | 01.03 |

## Roadmap
1. Docs and discipline (01.01)
2. Core loader impl + tests (01.02 + 01.03)
3. Evidence + closure (01.04)

## Epic acceptance criteria
- planning/edger/ has updated docs without stale links.
- edger.ts exports loadWorkerHandler that makes hello-world, serve-declarative-style, empty-response, read-body (and chunked etc) return correct bodies.
- bun test passes (4+).
- Multiple real launches (hello, declarative, chunked, stream, empty, read-body) return expected bodies.
- 11+ workers/ dirs with correct index.{ts,js,mjs} copied from edge-runtime/examples.
- Roadmap + epic + closure aligned and marked completed for Fase 1.

## Risks
- Bun import shim for some examples with remote imports may fail (mitigated by choosing pure ones).

## Status
completed
