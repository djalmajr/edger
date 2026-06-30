# Checkpoint — Story 07.05 Wasm (WIP v1)

**Data:** 2026-06-29  
**Story:** `epics/07-avancado/05-wasm-execution.md`

## Entregue (v1 slice)

- `edger-isolation/src/wasm/handler.rs` — `WasmHttpHandler` com ABI mínima
- `edger-isolation/src/wasm/load.rs` — load seguro do worker dir (anti `../`)
- `edger-isolation/src/wasm/handler.rs` — valida magic bytes, tamanho máximo, imports host/WASI bloqueados
- `edger-isolation/src/wasm/wasi.rs` — `WasiConfig` deny-by-default + filtro de env sensível
- `WorkerConfig.worker_dir` — pool injeta path no fetch
- `WorkerPool::fetch` — `config.kind` agora tem precedência quando `kind_hint` não é passado
- `WasmIsolate::execute_wasm` — bytes pré-carregados ou load via entrypoint
- `workers/wasm-hello/manifest.yaml` + teste pool E2E
- `planning/edger/docs/wasm-abi.md` — ABI v1 documentada
- Testes: handler unit + wasi unit + wasm_integration (2) + wasm_pool_integration (2) + load (2)

## Gates

- `cargo test -p edger-isolation --features wasm` verde
- `cargo test --workspace` verde (default features, mock path)
- `cargo clippy --workspace -D warnings` verde

## Pendências (story 07.05)

| Item | Prioridade |
|---|---|
| Ampliar E2E `.wasm` via bin Rust além de `wasm-hello` | Média |
| Host WASI real (`wasi.rs`) — preopen apenas worker root | Alta |
| ABI request/response em linear memory (não só body estático) | Média |
| Factory dinâmica por worker kind no orquestrador Rust | Concluído |

## Próximo

Host WASI real com preopen restrito, ABI request/response em linear memory, ou story 07.04 deno_core boot (bloqueador de JS/TS funcional — ver pendências).
