# Closure: Epic 04 — Worker Management

**Date:** 2026-06-29  
**Epic:** `epics/04-worker-management/00-overview.md`  
**Mode:** /agile-status closure

## Plan vs result

| Story | Delivered |
|---|---|
| 04.01 WorkerPool + LRU | pool, lru, types, factory trait, 4 tests |
| 04.02 Supervisor | state machine, supervisor, 7 lifecycle tests |
| 04.03 Metrics + ephemeral | MetricsCollector, EphemeralGate, 6 tests |
| 04.04 Integration | helpers, fixtures, 7 integration tests |

## Verification
- `edger-worker`: 24 tests (production deps: `edger-core` only)
- Workspace: 55 Rust + 6 bun tests
- `cargo clippy -D warnings`: pass

## Pendências documentadas (cross-epic)
- `dispatch_to_isolate` duplicado de `edger-isolation/kinds.rs` → mover para core
- `fetch` deriva worker name de `dir.file_name()` — orquestrador passará `WorkerRef` explícito (Epic 05)
- Timer TTL E2E com `time::pause` — observability em Epic 05/07
- Prometheus/OTEL metrics export — Epic 05/07

## Handoff
Epic 05 desbloqueado: orquestrador pode chamar `WorkerPool::fetch` com `IsolateFactory` injetada.
Próximo: `/agile-story` em `epics/05-orquestrador/`.