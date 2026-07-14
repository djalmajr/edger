# Story 19.A: Remover UdsTransport vestigial

**Origin:** planning/edger/epics/19-runtime-completude/00-overview.md

## Context

- **Problema:** `UdsTransport` permanece em `crates/edger-isolation/src/transport.rs`, mas o transporte UDS real usado pelo runtime está em `multiproc.rs`.
- **Objetivo:** remover o código morto e qualquer re-export órfão, sem alterar o transporte multiprocesso real.
- **Valor:** reduz ambiguidade para mantenedores e evita que uma API vestigial pareça suportada.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/transport.rs` | edit | Remover `UdsTransport` e código associado que não é usado |
| `crates/edger-isolation/src/lib.rs` | inspect/edit | Remover re-exports órfãos se existirem |
| `crates/edger-isolation/tests/` | inspect/edit | Ajustar apenas testes que referenciem o transporte removido |

## Detail

### Critérios de aceite
- [x] `UdsTransport` não existe mais no código do workspace.
- [x] Nenhum re-export, teste ou import aponta para `UdsTransport`.
- [x] O transporte real em `multiproc.rs` permanece inalterado em comportamento.
- [x] O workspace compila sem warnings novos.

## Tasks

- [x] Buscar referências a `UdsTransport`.
- [x] Remover a implementação vestigial de `transport.rs`.
- [x] Remover imports/re-exports órfãos.
- [x] Ajustar testes apenas se houver referência direta ao símbolo removido.

## Verification

```bash
rg "UdsTransport"
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed**
