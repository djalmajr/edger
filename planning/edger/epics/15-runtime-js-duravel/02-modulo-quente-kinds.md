# Story 15.B: Módulo quente + paridade de ExecutionKind via UDS

**Origin:** `planning/edger/epics/15-runtime-js-duravel/00-overview.md`

## Context

- **Problema:** com o transporte UDS provado (15.A), falta integrar o processo persistente no pool/orquestrador como caminho real e cobrir os `ExecutionKind` (fetch/routes/SPA) que a ponte v1 já cobre — sem re-import por request.
- **Objetivo:** o `WorkerPool` despacha para o processo Deno persistente via `UdsTransport`; módulo importado uma vez; matriz de exemplos atual verde por esse caminho; perf re-medida.
- **Valor:** substitui o custo de spawn+re-import por processo quente; ~40 ms → poucos ms mantendo compat.
- **Restrições:** não regredir a matriz; ponte v1 fica como fallback até esta story passar todos os exemplos.

## Traceability

- `edger-worker/src/pool.rs` (factory/dispatch → transporte persistente)
- `edger-isolation/src/deno/` (backend UDS da 15.A)
- `planning/edger/docs/compat-matrix.md`, `edger-orchestrator/tests/kind_dispatch_integration.rs`
- `edger-orchestrator/tests/perf_harness.rs` (re-medição)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/deno/mod.rs` | edit | `DenoIsolate` usa processo persistente + `UdsTransport` para fetch/routes/SPA |
| `edger-worker/src/pool.rs` | edit | Ciclo de vida do processo por worker (reuso entre requests; reciclar no crash) |
| `edger-isolation/src/deno/worker_host.rs` | edit | Harness cobre routes/SPA além de fetch; base_href/x-base preservados |
| `edger-orchestrator/tests/kind_dispatch_integration.rs` | edit | Rodar a matriz por UDS (feature multiproc) |
| `edger-orchestrator/tests/perf_harness.rs` | edit | Cenário com backend UDS real (não mock) |

## Detail

### AS-IS
- Pipeline usa `DenoCliRunner` (`deno eval` por request).

### TO-BE
- Backend persistente é o caminho de produção para JS/TS; import uma vez; requests subsequentes reusam o processo.
- Hot-reload: nova versão instalada (Fase 14) invalida/recria o processo do worker.
- Fetch/routes/SPA com paridade da ponte v1 (inclui `x-base`/base injection e namespaced).

### Scope
- **In:** integração pool/pipeline, paridade de kinds, hot-reload por deploy, re-medição de perf.
- **Out:** frameworks npm (15.C), limites de recurso (15.D), streaming real (15.E).

### Acceptance criteria
- [x] Fetch (com body), routes (exact + :param) e SPA (base injection) verdes via processo persistente. (`process_dispatch_integration.rs`; validação live: hello-world/read-body/routes-demo/wasm)
- [x] Segundo request reusa o processo quente (asserção no E2E + queda de latência: ephemeral respawn ~21ms → warm ~1.6ms prova reuso).
- [x] Deploy de nova versão = novo `name@version` no índice → nova instância → novo processo (chave do pool + terminate no evict/rollback já cobrem; ver 14.04).
- [x] Perf re-medida ao vivo: end-to-end HTTP p50 1.57ms (~25x vs ~40ms da ponte v1). Evidência em js-runtime-perf-2026-07-02.md.
- [x] Ponte v1 selecionável via `EDGER_JS_RUNTIME=bridge`; gate workspace verde (345 testes).

### Dependencies
- Story 15.A

## Tasks
### Fase 1 — Integração
- [x] `DenoProcessIsolate` (Isolate): fetch/routes via processo, SPA via `static_spa.rs` compartilhado.
- [x] Pool mantém o processo por instância (workers JS persistentes por default); erro de request reseta o processo (respawn); crash reciclado pela recuperação de pool.
### Fase 2 — Paridade + hot-reload
- [x] Harness cobre routes (port do makeRoutesHandler); SPA servido em Rust; base_href/x-base preservados.
- [x] Invalidação do processo via chave name@version + terminate no evict.
### Fase 3 — Perf + gate
- [x] Re-medido ao vivo e registrado (js-runtime-perf-2026-07-02.md, Fase B).
- [x] Gate workspace + clippy + fmt verdes.

## Verification

```bash
cargo test -p edger-orchestrator --test kind_dispatch_integration --features multiproc
cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture
cargo test --workspace
```

## Status

**completed** (2026-07-02) — `DenoProcessIsolate` integra o processo Deno
persistente na trait `Isolate` e no `WorkerPool`: fetch/routes via UDS (módulo
importado uma vez), StaticSpa via `static_spa.rs` (Rust puro, compartilhado com a
ponte v1 após refactor sem duplicação), Wasm segue no WasmIsolate. Bin `edger`
usa o backend de processo por default; `EDGER_JS_RUNTIME=bridge` volta à v1.
Achado e corrigido: workers JS eram efêmeros por default → o pool matava o
processo a cada request; `parse_worker_config` agora dá TTL persistente a
FetchHandler/RoutesTable/StaticSpa (ephemeral opt-in via `ttl: 0`). Paridade
E2E em `process_dispatch_integration.rs` (fetch+body, routes exact/:param, SPA
base injection). **Perf end-to-end HTTP: p50 1.57ms (~25x vs ~40ms da ponte v1)**;
round-trip do isolate 67us. Gate: 345 testes + multiproc verdes, clippy, fmt.
Evidência: `status/evidence/js-runtime-perf-2026-07-02.md`.
