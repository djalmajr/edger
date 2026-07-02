# Medição — custo do JS runtime (ponte Deno CLI) — 2026-07-02

Objetivo: decidir entre embutir deno_core/deno_runtime vs. manter a ponte Deno,
com dados reais (não suposição). Servidor ao vivo, 20 requests por caminho.

## Números (p50 / p95, wall-clock end-to-end via curl)

| Caminho | p50 | p95 |
|---|---|---|
| JS pela ponte Deno (spawn por request) | ~40 ms | ~40–52 ms |
| Wasm in-process (wasmtime) | ~3 ms | ~4 ms |
| Pipeline puro (/ready, sem worker) | ~1 ms | ~1 ms |
| Piso de boot do `deno run` (V8 snapshot) | ~10 ms | — |

## Interpretação

- Ponte Deno custa ~40 ms/request => ~10x mais lento que in-process.
- O custo NÃO é só spawn (~10 ms). O bridge script faz
  `import(entryUrl + "?edger=" + crypto.randomUUID())`, que força re-transpilar
  e re-avaliar o módulo do worker A CADA request (~30 ms), além do spawn.
- Logo: spawn (~10 ms) + re-import (~30 ms) pagos em todo request.

## Decisão sustentada pelos dados

Pool de workers Deno QUENTES (N processos deno vivos desde o boot, cada módulo
importado uma vez, requests multiplexados por IPC) elimina spawn + re-import:
esperado ~40 ms -> poucos ms (perto do Wasm), com:
- risco de compat ~zero (continua o Deno completo: TS, fetch, node:, import maps, remoto);
- zero V8 no build (sem mudança de toolchain);
- migração incremental sobre a ponte atual.

Embutir deno_runtime (B) chegaria a latência parecida a custo enorme de
build/manutenção; não se justifica agora. Fica como teto de produção futuro.
deno_core puro (A) descartado: reconstruir Web APIs (estilo Supabase edge-runtime).

## Quick win independente

Remover o cache-busting `?edger=<uuid>` do import (ou só re-importar quando o
mtime do worker muda) já cortaria parte do re-transpile mesmo antes do pool.

## Fase A entregue — worker Deno persistente por UDS (2026-07-02)

`edger-isolation` feature `multiproc`: `DenoWorkerProcess` spawna 1 processo deno
persistente, harness importa o módulo UMA vez, requests round-trip por UDS (JSON
length-prefixed). Teste `uds_roundtrip.rs`:
- persistent_worker_serves_multiple_requests_without_reimport: contador de módulo
  prova reuso (calls=1 depois calls=2, sem re-import/re-spawn).
- worker_that_throws_on_load_fails_spawn: erro tipado UDS_WORKER_FAILED com a causa.

Latência do round-trip do isolate (warm, 50 amostras):
  UDS_WARM_LATENCY avg=154us p50=67us p95=490us

Comparação com a ponte v1 (custo de execução JS por request):
  ponte v1 (spawn + re-import): ~38 ms  ->  UDS warm: ~0,067 ms  (~600x)

(O ~40 ms do end-to-end HTTP incluía pipeline+auth; ao integrar no pool na 15.B,
o HTTP passa a ser dominado pelo pipeline ~1 ms.)

## Fase B entregue — backend de processo no pool + workers JS persistentes (2026-07-02)

`DenoProcessIsolate` (Isolate trait) integra o processo persistente no `WorkerPool`:
fetch/routes via processo (UDS), StaticSpa via Rust puro compartilhado
(`static_spa.rs`), wasm segue no WasmIsolate. Bin `edger` usa o backend de
processo por default; `EDGER_JS_RUNTIME=bridge` volta à ponte v1.

Achado importante: workers JS eram EFÊMEROS por default (ttl=0) → o pool matava o
processo persistente após cada request (respawn ~20ms). Corrigido:
`edger-core::parse_worker_config` dá TTL default persistente (300s) a
FetchHandler/RoutesTable/StaticSpa (JS/SPA persistentes por default; ephemeral
opt-in via `ttl: 0`).

Latência end-to-end HTTP (worker JS quente, servidor ao vivo, 30 req):
  avg=2.18ms p50=1.57ms p95=4.98ms

Comparação end-to-end (request HTTP completo, mesmo worker):
  ponte v1: ~40 ms  ->  processo persistente warm: ~1.6 ms  (~25x)
  (round-trip do isolate em si: 67us; o resto é pipeline+auth+HTTP)

Paridade provada por `edger-orchestrator/tests/process_dispatch_integration.rs`:
fetch (com body, reuso do processo quente), routes (exact + :param), SPA (base
injection) — todos pelo backend de processo.
