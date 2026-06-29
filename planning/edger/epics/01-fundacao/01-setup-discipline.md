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

### Acceptance criteria
- [x] `planning/edger/AGENTS.md` exists with verification gate and ai-memory scope
- [x] `memory_lint` scoped to `djalmajr/edger` reports no critical planning issues
- [x] Roadmap and epic overview cross-refs updated

## Test-first plan
- N/A (docs story); verify via memory_lint + refinement gates

## Tasks
- [x] Write AGENTS.md
- [x] Run lints
- [x] Update docs

## Verification
```bash
bun test
test -f AGENTS.md
test -f planning/edger/roadmap.md
rg -q 'planning/edger' AGENTS.md
```
