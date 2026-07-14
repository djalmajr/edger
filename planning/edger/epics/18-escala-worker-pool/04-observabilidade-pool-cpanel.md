# Story 18.D: Observabilidade do pool + cPanel

**Origin:** `planning/edger/epics/18-escala-worker-pool/00-overview.md`

## Context

- **Problema:** `/metrics` e `/metrics/stats` já expõem métricas agregadas de pool, cache, ephemeral e HTTP, mas não mostram ocupação por worker/processo nem fila de workers persistentes. Com N processos por worker, sem essa visão o operador não sabe se a latência vem de spawn, fila, saturação do pool ou streams longos.
- **Objetivo:** expor métricas Prometheus e JSON por worker sobre processos ativos/ociosos, fila, latência de espera, rejeições e recycle; refletir a informação na aba Workers do cPanel como item should.
- **Valor:** torna Level 1 operável e fornece sinal para ajustar `maxProcesses`, fila e HPA sem inspecionar logs internos.
- **Restrições:** não vazar segredo, env, headers, path de request, request id, nem labels de alta cardinalidade.

## Traceability

- `crates/edger-worker/src/metrics.rs` (`PoolMetrics`, `WorkerStats`, `MetricsCollector`)
- `crates/edger-worker/src/pool.rs` (`get_metrics`, `worker_stats`, `worker_stats_for_instance`)
- `crates/edger-orchestrator/src/metrics.rs` (`pool_metrics_prometheus`, `metrics_stats_response`)
- `crates/edger-orchestrator/src/pipeline.rs` (`/metrics`, `/metrics/stats`)
- `crates/edger-orchestrator/src/server.rs` (`ServerState::pool_metrics`)
- `crates/edger-orchestrator/tests/metrics_endpoint.rs`
- `workers/core/cpanel/` (interface de control plane servida em `/cpanel/`)

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-worker/src/metrics.rs` | edit | Adicionar métricas de grupo/processo/fila/wait/rejeição |
| `crates/edger-worker/src/pool.rs` | edit | Coletar snapshots por worker e por processo sem segredos |
| `crates/edger-orchestrator/src/metrics.rs` | edit | Expor Prometheus e JSON com labels/campos estáveis |
| `crates/edger-orchestrator/tests/metrics_endpoint.rs` | edit | Provar shape de `/metrics` e `/metrics/stats` sem labels secret-like |
| `workers/core/cpanel/` | edit | Should: mostrar processos/fila na aba Workers consumindo `/metrics/stats` |
| `planning/edger/docs/performance-baselines.md` | edit | Atualizar nomes de métricas e baseline após 18.E |

## Detail

### AS-IS
- `PoolMetrics` tem `active_workers`, `idle_workers`, cache hits/misses, spawn latency, `ephemeral_inflight`, `ephemeral_queued`, `ephemeral_rejected` e duração last.
- `WorkerStats` mostra `app`, `name`, `version`, `state`, `request_count`, `uptime_seconds`, `unhealthy` e `worker_id`.
- `/metrics` é Prometheus agregado e `/metrics/stats` é JSON com pool + workers.
- Não há métrica de processos por worker, fila persistente, wait time ou rejeições persistentes.

### TO-BE
- Métricas Prometheus:
  - processos por worker: total, idle, active, terminating.
  - fila por worker: queued, rejected total, timeout total.
  - latência de espera: last/p50/p95 ou buckets simples se o projeto já tiver padrão.
  - recycle por causa: ttl, max_requests, error, oom/shutdown.
- `/metrics/stats` retorna JSON amigável ao cPanel com grupos por worker e processos filhos, sem raw config nem env.
- cPanel Workers mostra, como should, capacidade (`active/max`), fila atual, waits/rejeições e estado dos processos.

### Scope
- **In:** métricas Prometheus/JSON; testes de shape; cPanel read-only se a UI já consumir `/metrics/stats` sem grande refactor.
- **Out:** métricas RSS/CPU reais por processo; retenção histórica; alertmanager; dashboards externos; autenticação nova.

### Acceptance criteria
- [ ] `/metrics` expõe métricas do pool N-processos com labels estáveis e baixa cardinalidade (`worker`, `version`, `namespace` quando existir).
- [ ] `/metrics/stats` mostra cada worker com `processes`, `activeProcesses`, `idleProcesses`, `queued`, `waitMs`, `rejectedTotal` e estado por processo.
- [ ] Nenhuma métrica ou JSON inclui env/secrets, header de request, path arbitrário, request id ou filesystem absoluto sensível.
- [ ] Tests em `metrics_endpoint.rs` falham se remover os campos novos ou vazar labels secret-like.
- [ ] cPanel, como should, exibe a visão de pool na aba Workers sem bloquear uso headless do runtime.
- [ ] Métricas existentes de cache/ephemeral continuam compatíveis ou têm migração documentada.

### Dependencies
- Stories 18.A e 18.B

## Tasks
### Fase 1 — Modelo de métricas
- [ ] Definir nomes Prometheus e campos JSON.
- [ ] Adicionar métricas de grupo/processo/fila no collector.
- [ ] Garantir baixa cardinalidade e ausência de segredos.
### Fase 2 — Endpoints
- [ ] Atualizar `pool_metrics_prometheus`.
- [ ] Atualizar `metrics_stats_response`.
- [ ] Adicionar testes de endpoint.
### Fase 3 — cPanel should
- [ ] Mapear consumo atual da aba Workers.
- [ ] Exibir capacidade/fila/processos se couber sem refactor grande.
- [ ] Documentar caso a UI fique como follow-up.

## Verification

```bash
cargo test -p edger-orchestrator metrics_endpoint
cargo test -p edger-worker metrics_ephemeral integration_pool
cargo test --workspace
ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
curl -s http://127.0.0.1:19080/metrics | rg "edger_worker|edger_pool|queue|wait|rejected"
curl -s -H "authorization: Bearer test-root" http://127.0.0.1:19080/metrics/stats
```

## Status

**completed** (2026-07-03) — observabilidade do pool mergeada: métricas Prometheus/JSON por worker/processo/fila/rejeições e cPanel exibindo capacidade, fila, espera, rejeições e processos por worker.
