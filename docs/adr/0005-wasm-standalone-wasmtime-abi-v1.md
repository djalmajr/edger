# ADR 0005 — Wasm standalone com `wasmtime` e ABI HTTP v1

- **Status:** Aceito
- **Data:** 2026-06-29

## Contexto

O Edger precisa suportar Wasm sem acoplar módulos Wasm ao isolate JS/TS. A
foundation precisa de uma ABI mínima que seja fácil de validar, segura por
default e executável pelo pipeline Rust.

Alternativas consideradas:

- executar Wasm dentro do backend JS;
- liberar WASI desde o começo;
- começar com ABI HTTP mínima standalone via `wasmtime`.

WASI completo e ABI request/response em linear memory aumentariam o escopo da
primeira entrega. Executar Wasm dentro de JS enfraqueceria a fronteira de
isolamento.

## Decisão

Executar Wasm standalone via `wasmtime` e definir ABI HTTP v1 mínima:

- entrypoint `.wasm` ou `.wat`;
- export `memory`;
- export opcional `http_status() -> i32`;
- export opcional `http_body_len() -> i32`;
- body lido a partir do offset `0`;
- imports de host e WASI negados por default.

## Consequências

Positivas:

- caminho Wasm real pelo pipeline Rust;
- sandbox deny-by-default;
- testes pequenos e determinísticos;
- evolução futura clara para request/response em linear memory.

Custos:

- ABI v1 só retorna resposta estática;
- WASI real ainda está pendente;
- módulos precisam seguir o layout específico de memory/body.

## Status

Aceito em 2026-06-29. Fonte de verdade: `crates/edger-isolation/src/wasm/handler.rs`,
`planning/edger/docs/wasm-abi.md` e
`planning/edger/status/checkpoint-2026-06-29-story-07-05-wip.md`.
