# Status Consolidation: Backlog maduro — pronto para desenvolvimento

**Date:** 2026-06-29
**Mode:** consolidation (post planning decomposition)

## Scope
Decomposição completa do roadmap Fases 1-7 em epics/stories/tasks via fluxo `/agile-*`.

## Backlog summary

| Fase | Epic folder | Stories | Planning status | Implementation |
|---|---|---|---|---|
| 1 Fundação | `epics/01-fundacao/` | 4 | complete | **delivered** (Bun loader) |
| 2 edger-core | `epics/02-edger-core/` | 4 | ready-for-development | not started |
| 3 Isolação | `epics/03-isolacao-execucao/` | 4 | ready-for-development | not started |
| 4 Worker | `epics/04-worker-management/` | 4 | ready-for-development | not started |
| 5 Orquestrador | `epics/05-orquestrador/` | 5 | ready-for-development | not started |
| 6 Extensibilidade | `epics/06-extensibilidade/` | 3 | ready-for-development | not started |
| 7 Avançado | `epics/07-avancado/` | 7 | ready-for-development | not started |

**Total:** 7 epics, 31 stories, todas com Context/Traceability/Files/Detail/Tasks/Verification.

## Maturity gates (planning)

- [x] Cada fase do roadmap tem epic correspondente (`01`–`07`)
- [x] Cada epic tem `00-overview.md` + >=1 story file
- [x] Stories contêm tasks acionáveis e comandos de verificação (`cargo test`, `bun test`, launches)
- [x] agile-refinement (script) — 0 findings em `planning/edger/epics/`
- [x] memory_lint scoped `djalmajr/edger` — 0 findings
- [x] Fase 1 permanece `completed`; Fases 2-7 `ready-for-development`
- [x] Cross-refs roadmap ↔ epics alinhados

## Critical path (implementação)

```mermaid
flowchart LR
    E01[01 Fundação done] --> E02[02 edger-core]
    E02 --> E03[03 Isolação spike]
    E02 --> E04[04 Worker pool]
    E03 --> E04
    E02 --> E05[05 Orquestrador]
    E04 --> E05
    E05 --> E06[06 Extensões]
    E05 --> E07[07 Avançado]
    E06 --> E07
```

## Next execution step
`/agile-story` em `planning/edger/epics/02-edger-core/01-setup-core-crate.md` — completar módulos do core e gate Rust.

## Deviations from prior consolidation
- Backlog expandido de 2 epics parciais para 7 epics completos (31 stories).
- Fase 1 ganhou stories 03-copy-examples e 04-closure-evidence (retrospectiva documentada).

## Evidence
- `planning/edger/epics/` tree: 7 folders, 38 markdown files
- Refinement report: scratch/refinement-report.txt
- Tests: `bun test` pass; `cargo check --workspace` pass (skeleton)