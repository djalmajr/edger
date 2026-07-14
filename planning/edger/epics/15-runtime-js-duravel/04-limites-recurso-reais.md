# Story 15.D: Limites de recurso reais por worker

**Origin:** `planning/edger/epics/15-runtime-js-duravel/00-overview.md`

## Context

- **Problema:** o produto exige controle real de quanto um worker consome (memória/CPU), para não crescer indefinidamente. Hoje `ResourceLimits`/`LimitGuard`/`CpuTimer` são stub; só wall-clock timeout é real.
- **Objetivo:** com worker = processo, aplicar limites enforçáveis pelo SO no spawn (rlimit/cgroup); worker que estoura é morto e reciclado; métricas RSS/CPU por worker.
- **Valor:** isolamento e previsibilidade multi-tenant — teto que se pode auditar.
- **Restrições:** rlimit como base portável; cgroup como reforço em Linux; documentar tiers por plataforma (macOS dev vs Linux prod).

## Traceability

- `crates/edger-isolation/src/limits.rs` (`ResourceLimits`, `LimitGuard`, `CpuTimer` — stub → real)
- `crates/edger-core/src/config.rs` (`memory_mb` via `low_memory`, `timeout_ms`, `max_requests`)
- `crates/edger-isolation/src/deno/process.rs` (spawn com limites)
- `crates/edger-worker/src/supervisor.rs` (reciclar no kill)

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/deno/process.rs` | edit | Aplicar rlimit (RLIMIT_AS/CPU/NOFILE/NPROC) no spawn; `--v8-flags=--max-old-space-size` |
| `crates/edger-isolation/src/limits.rs` | edit | Mapear `WorkerConfig` → limites reais; enforcement de mem via processo (não stub) |
| `crates/edger-core/src/manifest.rs`/`config.rs` | edit | Campo explícito de memória por worker no manifest, se necessário |
| `crates/edger-worker/src/supervisor.rs` | edit | Detectar kill por limite → reciclar + registrar erro operacional |
| `crates/edger-isolation/tests/resource_limits.rs` | create | Worker que aloca acima do teto é morto; teto respeitado |

## Detail

### TO-BE
- Spawn do processo Deno com **cap de heap V8** (`--v8-flags=--max-old-space-size=<mb>`) derivado de `ResourceLimits::from_config`. Decisão medida (ver Status): para um processo V8, o heap cap é a barreira **correta e portável**; `RLIMIT_AS` é inutilizável (V8 reserva um espaço de endereçamento virtual enorme e seria morto no boot).
- Estouro do cap → V8 aborta com fatal OOM → processo morre → pool recicla a instância (mecanismo de `on_critical_error`/reciclagem já existente).
- Wall-clock timeout preservado (já real, via `request()` com `timeout`).
- cgroup `memory.max` documentado como reforço de RSS em produção Linux (fora deste slice).

### Scope
- **In:** cap de heap V8 por worker no spawn, threading `memory_mb` (config → `ResourceLimits` → `spawn`), morte+reciclagem por OOM, teste que prova enforcement + isolamento.
- **Out:** métricas RSS/CPU por worker (adiado — precisa poll de `/proc`/`ps` por processo); rlimit de CPU; cgroup `memory.max`/`cpu.max` (reforço Linux prod); cgroup delegation multi-node/K8s; QoS/prioridade fina.

### Acceptance criteria
- [x] Worker com teto de memória X que aloca > X é morto (V8 fatal OOM) e o pool recicla; host permanece saudável (worker vizinho sob teto maior responde normal).
- [x] `memory_mb` flui de `WorkerConfig`/`ResourceLimits::from_config` → `DenoWorkerProcess::spawn` → flag V8 (deixa de ser stub para memória).
- [x] Wall-clock timeout preservado (inalterado).
- [x] Decisão de enforcement medida e documentada (heap cap V8 vs `RLIMIT_AS`); tiers por plataforma (heap cap sempre; cgroup como backstop Linux prod).
- [ ] ~~Métricas RSS/CPU por worker~~ — adiado (fora do slice; ver Scope/Out).
- [ ] ~~Teto de CPU (rlimit)~~ — adiado (fora do slice; wall-clock cobre o caso de loop infinito hoje).

### Dependencies
- Story 15.A

## Tasks
### Fase 1 — cap de heap
- [x] `memory_mb` adicionado à assinatura de `DenoWorkerProcess::spawn`; aplica `--v8-flags=--max-old-space-size=<mb>` antes do script.
- [x] `DenoProcessIsolate::dispatch` deriva `ResourceLimits::from_config(config).memory_mb` e passa ao spawn.
### Fase 2 — kill + reciclagem
- [x] OOM do V8 mata o processo; `DenoProcessIsolate` reseta o processo no erro; pool recicla a instância (mecanismo existente de `on_critical_error`).
### Fase 3 — teste + doc
- [x] `resource_limits.rs`: mesmo worker alocador morre sob 48 MB e responde sob 256 MB (prova que é o cap, não a alocação; e que o vizinho independente segue saudável). Mutação capturada (remover o flag → verde→vermelho).
- [x] Decisão heap-cap vs `RLIMIT_AS` documentada em código e aqui; cgroup como backstop Linux prod anotado.

## Verification

```bash
cargo test -p edger-isolation --features multiproc --test resource_limits
cargo build --workspace
```

## Status

**completed** (2026-07-02) — Cap de memória real e enforçável por worker via o
limite de heap do V8 (`--v8-flags=--max-old-space-size=<mb>`), derivado de
`ResourceLimits::from_config` (128 MB em `low_memory`, 512 MB padrão) e fluindo
`config → ResourceLimits → DenoWorkerProcess::spawn`. Decisão **medida** antes de
codar: `RLIMIT_AS` foi descartado porque o V8 reserva um espaço de endereçamento
virtual enorme e morreria no boot; strings grandes escapam do `max-old-space-size`
(large-object space), então o teste enche o **old space** com ~2 M de objetos
pequenos. Resultado provado por `resource_limits.rs`: o mesmo worker alocador
**morre** sob teto de 48 MB (fatal OOM do V8) e **responde** sob 256 MB — isolando
a causa no cap e não na carga, e confirmando que um worker vizinho independente
(outro processo) permanece saudável. Mutação capturada (neutralizar o flag deixa o
teste vermelho). Adiados explicitamente (fora do slice): métricas RSS/CPU por
worker, rlimit de CPU e cgroup `memory.max`/`cpu.max` (reforço de RSS em Linux
prod) — o wall-clock timeout já cobre loops infinitos hoje.
