# Performance baselines

**Status:** local baselines captured in Story 08.07 and Story 07.07
**Origin:** `planning/edger/design.md` (Measurement), Story 08.07 runtime evidence, Story 07.07 perf harness

## Targets aspiracionais

| Métrica | Target | Status |
|---|---|---|
| Worker spawn cached/persistent | < 50ms | instrumented via `edger_pool_spawn_latency_ms_*`; persistent warm-hit harness added |
| p95 request mock worker | track trend | `perf_harness` captures local p50/p95 for persistent in-memory worker |
| Pool hit rate sob carga | > 95% for warm persistent worker | `perf_harness` captured 49 hits / 1 miss over 50 requests |
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
| 2026-07-01 | `perf_harness` persistent worker warm hit | p50 `52us`, p95 `92us`, `49` cache hits, `1` cache miss | `cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture`; in-memory fixture, not Deno |
| 2026-07-03 | `perf_harness` concorrente 1 vs N | a preencher pelo coordenador | Harness executável no sandbox, mas números do sandbox não são baseline confiável |

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

cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture
```

## Baseline 18.E: pool 1 vs N

Ambiente: macOS Apple Silicon (arm64), `cargo test` debug, harness com
`MockIsolate` de sleep simulado. Não é Deno real; mede a mecânica de
concorrência do pool, não latência absoluta de produção.

Comando reprodutível para o harness dedicado, capturado em 2026-07-03:

```bash
cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture
```

Linhas grepáveis esperadas:

```text
PERF scenario=maxproc_1_queue requests=32 concurrency=8 maxProcesses=1 ...
PERF scenario=maxproc_N_queue requests=32 concurrency=8 maxProcesses=4 ...
PERF scenario=maxproc_1_no_queue requests=32 concurrency=8 maxProcesses=1 ...
PERF scenario=maxproc_N_no_queue requests=32 concurrency=8 maxProcesses=4 ...
```

Números coletados fora do sandbox:

| Data | Cenário | Requests | Concorrência | `maxProcesses` | p50 ms | p95 ms | Throughput req/s | Queued | Rejected | OK |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 2026-07-03 | `maxproc_1_queue` | 32 | 8 | 1 | 172 | 340 | 23.32 | 28 | 0 | 32 |
| 2026-07-03 | `maxproc_N_queue` | 32 | 8 | 4 | 47 | 84 | 92.17 | 16 | 0 | 32 |
| 2026-07-03 | `maxproc_1_no_queue` | 32 | 8 | 1 | 0 | 42 | 183.20 | 0 | 28 | 4 |
| 2026-07-03 | `maxproc_N_no_queue` | 32 | 8 | 4 | 0 | 42 | 182.23 | 0 | 16 | 16 |

Leitura: sob concorrência 8 com fila, `maxProcesses=4` deu cerca de 4x o
throughput (`23.32` -> `92.17` req/s) e cerca de 4x menor p95 (`340` -> `84`
ms) vs `maxProcesses=1`; a serialização de 1 processo vira paralelismo. Sem
fila (`queueLimit=0`), a saturação aparece como rejeições 429: 1 processo
rejeita 28/32, 4 processos rejeitam 16/32.

Esses baselines são evidência local e comparativa, não garantia universal.
Hardware, versão do Deno em produção e a natureza CPU-bound vs IO-bound do
worker real mudam o resultado. O baseline de produção real deve ser medido no
cluster.

## Próximo baseline

Adicionar p99 e cenários com workers Deno reais quando houver ambiente dedicado
para benchmark de regressão. O harness atual usa fixture in-memory para isolar a
semântica do pool, fila e backpressure sem bind de socket.
