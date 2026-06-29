# Story 04.03: PoolMetrics, concorrência ephemeral e maxRequests

**Origin:** `planning/edger/epics/04-worker-management/00-overview.md`

## Context
- **Problema:** Sem métricas e sem limites de concorrência ephemeral, o pool não replica comportamento Buntime (ttl=0, maxRequests, fila ephemeral).
- **Objetivo:** Expor `PoolMetrics`, implementar semáforo/fila para workers ephemeral, enforcement de `max_requests` e contadores de spawn latency.
- **Valor:** Observabilidade mínima para Fase 5; proteção contra thundering herd em serverless.
- **Restrições:** Métricas thread-safe (`AtomicU64` / `dashmap` se necessário); fila com limite configurável rejeita com erro claro.

## Traceability
- **Source docs:** `planning/edger/design.md` (PoolMetrics, WorkerConfig max_requests, Observability), Buntime worker-pool metrics
- **Depends on:** Stories 04.01, 04.02; Epic 02.02 (`max_requests`, `ttl_ms`)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `crates/edger-worker/src/metrics.rs` | criar | `PoolMetrics`, `WorkerStats`, histogram stub |
| `crates/edger-worker/src/ephemeral.rs` | criar | semáforo + fila para ttl=0 |
| `crates/edger-worker/src/pool.rs` | alterar | integrar métricas + ephemeral gate |
| `crates/edger-worker/src/instance.rs` | alterar | increment request_count, check max_requests |
| `crates/edger-worker/tests/metrics_ephemeral.rs` | criar | métricas + limites |
| `crates/edger-worker/src/lib.rs` | alterar | export `PoolMetrics` |

## Detail

### AS-IS
- Hits/misses básicos apenas (se existirem da 04.01)
- Sem limite ephemeral concurrency
- Sem retirement por maxRequests

### TO-BE
- `PoolMetrics`:
  - `active_workers`, `idle_workers`, `terminated_total`
  - `cache_hits`, `cache_misses`
  - `spawn_latency_ms` (last / p50 stub via vec circular pequeno)
  - `ephemeral_inflight`, `ephemeral_queued`, `ephemeral_rejected`
  - `request_duration_ms` histogram stub
- `get_metrics() -> PoolMetrics` clone snapshot
- `get_worker_stats(worker_id) -> Option<WorkerStats>`
- Ephemeral path (`ttl_ms == 0`):
  - `EphemeralGate` com `Semaphore::new(ephemeral_concurrency)`
  - Fila `ephemeral_queue_limit` — excesso retorna `WorkerError::EphemeralQueueFull`
  - Após response: terminate imediato (via supervisor EphemeralTerm)
- `max_requests`: ao completar request N, se `count >= max_requests` → transição Terminating (não aceita novos fetch)

### Escopo
- **In:** métricas, ephemeral gate, maxRequests, export API
- **Out:** Prometheus endpoint (orchestrator Fase 5), OTEL (Fase 7)

### Critérios de aceite
- [ ] `get_metrics()` reflete hits após segundo fetch do mesmo worker
- [ ] Com `ephemeral_concurrency=1`, segundo fetch concurrent ttl=0 bloqueia ou enfileira conforme config
- [ ] Fila cheia retorna erro tipado sem panic
- [ ] Worker com `max_requests=3` aposenta após 3º request completo
- [ ] `spawn_latency_ms` registrado em cache miss
- [ ] Testes async com timeouts razoáveis

### Dependências
- Stories 04.01, 04.02

## Test-first plan
- **Primeiro teste falhando:** `max_requests_retires_worker` — config max_requests=2, terceiro fetch força miss/spawn novo ou erro retired
- **Nível:** `metrics_ephemeral.rs`
- **Cenários:** ephemeral concurrency 1 com 2 tokio tasks paralelas; queue full

## Tasks
- [ ] Implementar `metrics.rs` com atomics + snapshot
- [ ] Implementar `ephemeral.rs` Semaphore + queue counter
- [ ] Integrar métricas em pool fetch/get_or_create/spawn
- [ ] Integrar max_requests check em `on_request_complete`
- [ ] Export `PoolMetrics` na API pública
- [ ] Testes métricas + ephemeral + maxRequests
- [ ] Doc comments em português nos campos de métricas

## Verification
```bash
cargo test -p edger-worker --test metrics_ephemeral
cargo test -p edger-worker
cargo clippy -p edger-worker -- -D warnings
cargo fmt -- --check
bun test
```