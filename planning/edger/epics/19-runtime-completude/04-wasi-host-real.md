# Story 19.D: WASI host real

**Origin:** planning/edger/epics/19-runtime-completude/00-overview.md

## Context

- **Problema:** o host WASI em `crates/edger-isolation/src/wasm/` tem ABI estática e não entrega a request real ao módulo wasm.
- **Objetivo:** definir e ligar uma ABI mínima para request/response wasm.
- **Valor:** transforma o modo wasm em execução funcional, não apenas stub de integração.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/wasm/` | edit | Passar request ao módulo wasm e ler response |
| `crates/edger-isolation/tests/` | edit | Cobrir execução ponta-a-ponta de módulo wasm |
| `planning/edger/docs/` | inspect/edit | Documentar ABI se já houver página de runtime wasm |

## Detail

### Critérios de aceite
- [x] Módulo wasm recebe método, URL, headers e body necessários para responder.
- [x] Response do módulo volta com status, headers e body.
- [x] Erros de ABI são tipados e não derrubam o processo do host.
- [x] Há teste ponta-a-ponta provando request real entrando e response real saindo.

## Tasks

- [x] Mapear a ABI estática atual.
- [x] Definir a menor ABI request/response necessária.
- [x] Ligar a request do runtime ao módulo wasm.
- [x] Adicionar teste com módulo wasm simples.

## Verification

```bash
cargo test -p edger-isolation wasm
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed**
