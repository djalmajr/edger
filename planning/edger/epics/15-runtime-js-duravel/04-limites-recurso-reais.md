# Story 15.D: Limites de recurso reais por worker

**Origin:** `planning/edger/epics/15-runtime-js-duravel/00-overview.md`

## Context

- **Problema:** o produto exige controle real de quanto um worker consome (memória/CPU), para não crescer indefinidamente. Hoje `ResourceLimits`/`LimitGuard`/`CpuTimer` são stub; só wall-clock timeout é real.
- **Objetivo:** com worker = processo, aplicar limites enforçáveis pelo SO no spawn (rlimit/cgroup); worker que estoura é morto e reciclado; métricas RSS/CPU por worker.
- **Valor:** isolamento e previsibilidade multi-tenant — teto que se pode auditar.
- **Restrições:** rlimit como base portável; cgroup como reforço em Linux; documentar tiers por plataforma (macOS dev vs Linux prod).

## Traceability

- `edger-isolation/src/limits.rs` (`ResourceLimits`, `LimitGuard`, `CpuTimer` — stub → real)
- `edger-core/src/config.rs` (`memory_mb` via `low_memory`, `timeout_ms`, `max_requests`)
- `edger-isolation/src/deno/process.rs` (spawn com limites)
- `edger-worker/src/supervisor.rs` (reciclar no kill)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/deno/process.rs` | edit | Aplicar rlimit (RLIMIT_AS/CPU/NOFILE/NPROC) no spawn; `--v8-flags=--max-old-space-size` |
| `edger-isolation/src/limits.rs` | edit | Mapear `WorkerConfig` → limites reais; enforcement de mem via processo (não stub) |
| `edger-core/src/manifest.rs`/`config.rs` | edit | Campo explícito de memória por worker no manifest, se necessário |
| `edger-worker/src/supervisor.rs` | edit | Detectar kill por limite → reciclar + registrar erro operacional |
| `edger-isolation/tests/resource_limits.rs` | create | Worker que aloca acima do teto é morto; teto respeitado |

## Detail

### TO-BE
- Spawn do processo Deno com rlimit no `pre_exec` (Unix) e/ou cgroup `memory.max`/`cpu.max` quando disponível.
- `--v8-flags=--max-old-space-size=<mb>` como segunda barreira dentro do Deno.
- Estouro → SIGKILL pelo SO → supervisor recicla e registra no operational log (14.05).
- Métricas RSS/CPU por worker no `/metrics` e listagem do cPanel.

### Scope
- **In:** rlimit no spawn, cgroup opcional, kill+reciclagem, métricas, teste.
- **Out:** cgroup delegation multi-node/K8s; QoS/prioridade fina.

### Acceptance criteria
- [ ] Worker configurado com teto de memória X que aloca > X é morto pelo SO e reciclado; host permanece saudável.
- [ ] Teto de CPU/timeout enforçado; wall-clock preservado.
- [ ] `ResourceLimits`/`LimitGuard` deixam de ser stub para memória (processo).
- [ ] Métricas RSS/CPU por worker expostas.
- [ ] Tiers por plataforma documentados (rlimit sempre; cgroup em Linux).

### Dependencies
- Story 15.A

## Tasks
### Fase 1 — rlimit
- [ ] Aplicar RLIMIT_AS/CPU/NOFILE/NPROC no spawn (pre_exec) + v8 max-old-space.
### Fase 2 — kill + métricas
- [ ] Supervisor recicla no kill por limite; operational log; métricas RSS/CPU.
### Fase 3 — cgroup opcional + doc
- [ ] cgroup memory.max/cpu.max quando disponível; tiers documentados.

## Verification

```bash
cargo test -p edger-isolation --features multiproc --test resource_limits
cargo test --workspace
```
