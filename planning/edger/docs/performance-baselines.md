# Performance baselines

**Status:** first local baseline captured in Story 08.07
**Origin:** `planning/edger/design.md` (Measurement), Story 08.07 runtime evidence

## Targets aspiracionais

| Métrica | Target | Status |
|---|---|---|
| Worker spawn cached/persistent | < 50ms | instrumented via `edger_pool_spawn_latency_ms_*`; persistent worker benchmark still needed |
| p95 request mock worker | pending | local single-request samples captured; harness still needed |
| Pool hit rate sob carga | pending | `/metrics` exposes hit/miss counters; load scenario still needed |
| Memória por isolate | cap enforceable | pending |

## Baseline local atual

Ambiente: macOS local, `cargo run -p edger-orchestrator --bin edger`,
`ROOT_API_KEY=test-root PORT=19084 RUNTIME_WORKER_DIRS=workers`.

| Data | Cenário | Resultado | Notas |
|---|---|---|---|
| 2026-06-29 | `/health`, `/healthz`, `/ready`, `/readyz`, `/livez` | `200`, entre `0.001021s` e `0.002374s` | Probes Axum locais, sem worker dispatch |
| 2026-06-29 | `/hello-world` primeira chamada | `200`, `0.215365s` | Worker JS/TS via Deno CLI bridge |
| 2026-06-29 | `/hello-world` segunda chamada | `200`, `0.037221s` | `hello-world` não tem TTL persistente; não é cache hit de pool |
| 2026-06-29 | `/metrics` | `200`, `0.000929s`, `text/plain; version=0.0.4` | Scrape read-only de `WorkerPool::get_metrics()` |

Snapshot de métricas após duas chamadas a `hello-world` efêmero:

| Métrica | Valor |
|---|---:|
| `edger_pool_workers` | 0 |
| `edger_pool_cache_hits_total` | 0 |
| `edger_pool_cache_misses_total` | 2 |
| `edger_pool_terminated_total` | 2 |
| `edger_pool_spawn_latency_ms_last` | 1 |
| `edger_pool_spawn_latency_ms_p50` | 1 |
| `edger_pool_request_duration_ms_last` | 35 |
| `edger_ephemeral_rejected_total` | 0 |

O cache-hit persistente é coberto por teste automatizado em
`edger-orchestrator/tests/metrics_endpoint.rs`, com manifesto `ttl: 30s`.

## Comandos de captura

```bash
ROOT_API_KEY=test-root PORT=19084 RUNTIME_WORKER_DIRS=workers \
  cargo run -p edger-orchestrator --bin edger

for probe in /health /healthz /ready /readyz /livez; do
  curl -sS -w " status=%{http_code} time=%{time_total}\n" \
    "http://127.0.0.1:19084$probe"
done

curl -sS -H 'authorization: Bearer test-root' \
  -H 'content-type: application/json' \
  -d '{"name":"Alice"}' \
  -w " status=%{http_code} time=%{time_total}\n" \
  http://127.0.0.1:19084/hello-world

curl -sS -H 'authorization: Bearer test-root' \
  -H 'content-type: application/json' \
  -d '{"name":"Bob"}' \
  -w " status=%{http_code} time=%{time_total}\n" \
  http://127.0.0.1:19084/hello-world

curl -sS -D - -o /dev/null \
  -w "status=%{http_code} time=%{time_total}\n" \
  http://127.0.0.1:19084/metrics
```

## Próximo baseline

Adicionar harness dedicado com worker persistente, worker efêmero, worker lento
e cenário de burst. O objetivo é medir p50/p95/p99, hit rate e rejeições sem
depender de amostras manuais de uma única execução.
