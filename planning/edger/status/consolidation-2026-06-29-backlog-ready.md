# Status: Backlog edger — Fases 1–6 entregues, Fase 7 em progresso

**Source:** `planning/edger/roadmap.md`  
**Mode:** Consolidation (updated 2026-06-29 post Epic 06 + 07.05 WIP)

## Context
- **Project/initiative:** edger
- **Period:** 2026-06-28 — 2026-06-29
- **Current objective:** Fase 7 Epic 07 Avançado (07.05 Wasm em progresso)
- **Related epic:** `epics/07-avancado/00-overview.md`

---

## Consolidation (period report)

### Progress
- **Completed:**
  - Fase 1: Bun loader (6 tests)
  - Fase 2: `edger-core` modular — per-story checkpoints `status/checkpoint-2026-06-29-story-02-0{1..4}.md`
  - Per-story refinement Epic 02: `status/evidence/refinement-story-02-0{1..4}.txt`
  - Epic 02 closure: `status/checkpoint-2026-06-29-epic-02-closure.md`
  - Epic 03–05: closures + per-story checkpoints 03-01..04, 05-01..05
  - Epic 06: `edger-ext-auth`, `edger-ext-gateway`, closure `checkpoint-2026-06-29-epic-06-closure.md`
- **In progress:** Epic 07.05 Wasm — pool E2E + ABI v1 (`checkpoint-2026-06-29-story-07-05-wip.md`)
- **Blocked:** Epic 07.04 deno_core V8 boot (spike 03.04 carry-over)

### Backlog summary

| Fase | Epic folder | Stories | Planning status | Implementation |
|---|---|---|---|---|
| 1 Fundação | `epics/01-fundacao/` | 4 | complete | **delivered** |
| 2 edger-core | `epics/02-edger-core/` | 4 | complete | **delivered** (17+ tests) |
| 3 Isolação | `epics/03-isolacao-execucao/` | 4 | **completed** | 14+ isolation tests |
| 4 Worker | `epics/04-worker-management/` | 4 | **completed** | 24+ worker tests |
| 5 Orquestrador | `epics/05-orquestrador/` | 5 | **completed** | 48+ orchestrator tests |
| 6 Extensibilidade | `epics/06-extensibilidade/` | 3 | **completed** | edger-ext-auth + gateway |
| 7 Avançado | `epics/07-avancado/` | 7 | in-progress | 07.05 Wasm v1 |

### Next steps
- [ ] Completar 07.05 (WASI sandbox, orchestrator E2E)
- [ ] Desbloquear 07.04 deno_core boot
- [ ] Per-story checkpoint + refinement após cada story Epic 07

### Pendências dedicadas
- `planning/edger/docs/pendencies-epic-07.md`

---

## Maturity gates (planning)

- [x] 7 epics / 31 stories decomposed
- [x] /agile-refinement Mode 1 — 0 red flags (last run)
- [x] bun test pass (6 pass)
- [x] Fases 1–6 delivered; Fase 7 started