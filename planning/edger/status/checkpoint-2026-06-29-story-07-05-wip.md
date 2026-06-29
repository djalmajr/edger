# Checkpoint — Story 07.05 Wasm (WIP v1)

**Data:** 2026-06-29  
**Story:** `epics/07-avancado/05-wasm-execution.md`

## Entregue (v1 slice)

- `edger-isolation/src/wasm/handler.rs` — `WasmHttpHandler` com ABI mínima:
  - exports `http_status`, `http_body_len`, `memory`
- `WasmIsolate::execute_wasm` executa módulo quando `with_wasm_bytes` configurado
- Feature `wasm` liga deps wasmtime/wat opcionais
- Testes: 1 unit (`handler.rs`) + 2 integração (`wasm_integration.rs`)

## Gates

- `cargo test -p edger-isolation --features wasm` verde
- `cargo test --workspace` verde (default features, mock path)
- `cargo clippy --workspace -D warnings` verde

## Pendências (story 07.05)

| Item | Prioridade |
|---|---|
| Carregar `.wasm` do worker dir via manifest `WasmModule.entry` | Alta |
| `workers/wasm-hello/` fixture + pool E2E | Alta |
| WASI sandbox (`wasi.rs`) — preopen deny-by-default com teste negativo | Alta |
| Filtro env `*_SECRET` | Média |
| ABI request/response em linear memory (não só body estático) | Média |
| `planning/edger/docs/wasm-abi.md` | Baixa |
| Wire em `edger-worker/instance.rs` para kind `WasmModule` | Alta |

## Próximo

Pool wiring + fixture `workers/wasm-hello/` OU story 07.04 spike deno_core boot (bloqueado — ver pendências).