# Checkpoint — Story 07.05 Wasm (WIP v1)

**Data:** 2026-06-29  
**Story:** `epics/07-avancado/05-wasm-execution.md`

## Entregue (v1 slice)

- `edger-isolation/src/wasm/handler.rs` — `WasmHttpHandler` com ABI mínima
- `edger-isolation/src/wasm/load.rs` — load seguro do worker dir (anti `../`)
- `WorkerConfig.worker_dir` — pool injeta path no fetch
- `WasmIsolate::execute_wasm` — bytes pré-carregados ou load via entrypoint
- `workers/wasm-hello/manifest.yaml` + teste pool E2E
- Testes: handler unit + wasm_integration (2) + wasm_pool_integration (1) + load (2)

## Gates

- `cargo test -p edger-isolation --features wasm` verde
- `cargo test --workspace` verde (default features, mock path)
- `cargo clippy --workspace -D warnings` verde

## Pendências (story 07.05)

| Item | Prioridade |
|---|---|
| Carregar `.wasm` via orchestrator `bun edger.ts` launch | Média |
| WASI sandbox (`wasi.rs`) — preopen deny-by-default com teste negativo | Alta |
| Filtro env `*_SECRET` | Média |
| ABI request/response em linear memory (não só body estático) | Média |
| `planning/edger/docs/wasm-abi.md` | Baixa |
| Wire em `edger-worker/instance.rs` para kind `WasmModule` | Alta |

## Próximo

Pool wiring + fixture `workers/wasm-hello/` OU story 07.04 spike deno_core boot (bloqueado — ver pendências).