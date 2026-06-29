# Story 01.01: Setup AGENTS.md + doc lint + update planning docs

**Origin:** `planning/edger/epics/01-fundacao/00-overview.md`

## Context
Establish the engineering baseline for edger using ai-memory patterns and buntime rules.

## Traceability
- **Source docs:** `planning/edger/intake.md`, `planning/edger/analysis-synthesis.md`
- **Output:** `AGENTS.md`, `planning/edger/AGENTS.md`

## Files
- planning/edger/AGENTS.md (create)
- planning/edger/epics/01-fundacao/00-overview.md (update status)
- planning/edger/roadmap.md (update)

## Detail
- Create minimal AGENTS.md with gate and rules.
- Run memory_lint and refinement, fix.
- Update roadmap status.

## Test-first plan
- N/A (docs story); verify via memory_lint + refinement gates

## Tasks
- [x] Write AGENTS.md
- [x] Run lints
- [x] Update docs

## Verification
- bun test (n/a)
- memory_lint dry no critical for edger planning
- docs have current cross refs