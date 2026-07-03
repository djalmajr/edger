# Story 19.D: WASI host real

**Origin:** planning/edger/epics/19-runtime-completude/00-overview.md

## Context

- **Problema:** o host WASI em `edger-isolation/src/wasm/` tem ABI estática e não entrega a request real ao módulo wasm.
- **Objetivo:** definir e ligar uma ABI mínima para request/response wasm.
- **Valor:** transforma o modo wasm em execução funcional, não apenas stub de integração.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/wasm/` | edit | Passar request ao módulo wasm e ler response |
| `edger-isolation/tests/` | edit | Cobrir execução ponta-a-ponta de módulo wasm |
| `planning/edger/docs/` | inspect/edit | Documentar ABI se já houver página de runtime wasm |

## Detail

### Critérios de aceite
- [ ] Módulo wasm recebe método, URL, headers e body necessários para responder.
- [ ] Response do módulo volta com status, headers e body.
- [ ] Erros de ABI são tipados e não derrubam o processo do host.
- [ ] Há teste ponta-a-ponta provando request real entrando e response real saindo.

## Tasks

- [ ] Mapear a ABI estática atual.
- [ ] Definir a menor ABI request/response necessária.
- [ ] Ligar a request do runtime ao módulo wasm.
- [ ] Adicionar teste com módulo wasm simples.

## Verification

```bash
cargo test -p edger-isolation wasm
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
