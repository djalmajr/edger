# Spike de embedding — deno_core + wasmtime

**Status:** Preenchido (story 03.01 — 2026-06-29)  
**Origin:** `planning/edger/epics/03-isolacao-execucao/01-embedding-spike.md`  
**Design:** `planning/edger/design.md` (Embedding Spike Recommendation, PR 2)

## Resumo executivo

- **Wasm (wasmtime):** go — compile+invoke trivial em ~10ms local (debug build).
- **JS/TS (deno_core):** go condicional — wire roundtrip validado; boot V8/deno_core pendente (toolchain + pinagem de versões).

## Metodologia

- Ambiente: macOS aarch64, Rust stable, debug profile
- Time-box: story 03.01
- Exemplos: `edger-isolation/examples/embedding-spike-deno.rs`, `embedding-spike-wasm.rs`
- Evidência: `status/evidence/spike-wasm-run.txt` (SCRATCH captura)

## Resultados deno_core

| Métrica | Valor | Notas |
|---|---|---|
| spawn_ms | pendente | requer boot V8 platform singleton |
| exec_ms | 0 | wire sim only (`SerializedRequest` JSON roundtrip) |
| memória aprox. | pendente | |

**Wire sim (sem V8):** `cargo run -p edger-isolation --example embedding-spike-deno`  
Output: `spike_deno_wire_sim exec_ms=0 note=deno_core_boot_pending_V8_toolchain`

## Resultados wasmtime

| Métrica | Valor | Notas |
|---|---|---|
| compile_ms | 10 | WAT add module, debug build |
| invoke_ms | 0 | `add(2,40)==42` |

**Run:** `cargo run -p edger-isolation --example embedding-spike-wasm`  
Output: `spike_wasm compile_ms=10 invoke_ms=0 result=42`

## Sharp edges

- V8 platform singleton: não exercitado neste spike — adicionar `deno_core` pinado na story 03.04
- Op registration: pendente (facade Edge Runtime)
- Async ops dispatch: pendente
- Versões pinadas: wasmtime 29.x no workspace; deno_core TBD

## Go/no-go

| Backend | Decisão | Justificativa |
|---|---|---|
| JS/TS (deno_core + facade) | **go condicional** | Alinhado design; wire OK; boot V8 na 03.04 |
| Wasm (wasmtime + WASI standalone) | **go** | Spike compile+invoke OK; path separado do isolate JS |

## Recomendação de layout de módulos

```
edger-isolation/src/
  deno/     # facade (feature deno) — story 03.04
  wasm/     # wasmtime (feature wasm) — story 03.04
  mock.rs   # story 03.02
```

Feature flags: `deno`, `wasm` (default off; examples usam dev-deps).

## Impacto em Epic 04 / PR 10

- WorkerPool pode integrar mock Isolate (03.02) antes de embedding real.
- PR 10 (07.04/07.05) desbloqueado após 03.04 dual-backend prep.