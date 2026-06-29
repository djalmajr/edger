# Spike de embedding — deno_core + wasmtime

**Status:** PLANNING SKELETON (preencher na story `01-embedding-spike.md`)  
**Origin:** `planning/edger/epics/03-isolacao-execucao/01-embedding-spike.md`  
**Design:** `planning/edger/design.md` (Embedding Spike Recommendation, PR 2)

> Este arquivo é o artefato de saída do spike. O esqueleto existe para cross-refs
> e verificação de planejamento; métricas e Go/no-go são preenchidos na implementação.

## Resumo executivo

_A preencher após spike (story 03.01)._

## Metodologia

- Ambiente: _a definir_
- Time-box: 2–3 dias (conforme story)
- Exemplos: `edger-isolation/examples/embedding-spike-deno.rs`, `embedding-spike-wasm.rs`

## Resultados deno_core

| Métrica | Valor | Notas |
|---|---|---|
| spawn_ms | pendente | cold start |
| exec_ms | pendente | fetch roundtrip |
| memória aprox. | pendente | se API disponível |

## Resultados wasmtime

| Métrica | Valor | Notas |
|---|---|---|
| compile_ms | pendente | |
| invoke_ms | pendente | WASI |

## Sharp edges

- V8 platform singleton: _a documentar_
- Op registration: _a documentar_
- Async ops dispatch: _a documentar_
- Versões pinadas: _a documentar_

## Go/no-go

| Backend | Decisão | Justificativa |
|---|---|---|
| JS/TS (deno_core + facade) | pendente | Precedente Edge Runtime |
| Wasm (wasmtime + WASI standalone) | pendente | Decisão usuário |

## Recomendação de layout de módulos

```
edger-isolation/src/
  deno/     # facade (feature deno)
  wasm/     # wasmtime WASI (feature wasm)
  mock.rs   # já em story 03.02
```

_Detalhar após spike._

## Impacto em Epic 04 / PR 10

_A preencher após spike._