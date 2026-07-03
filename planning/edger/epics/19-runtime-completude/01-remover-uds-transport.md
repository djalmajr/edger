# Story 19.A: Remover UdsTransport vestigial

**Origin:** planning/edger/epics/19-runtime-completude/00-overview.md

## Context

- **Problema:** `UdsTransport` permanece em `edger-isolation/src/transport.rs`, mas o transporte UDS real usado pelo runtime está em `multiproc.rs`.
- **Objetivo:** remover o código morto e qualquer re-export órfão, sem alterar o transporte multiprocesso real.
- **Valor:** reduz ambiguidade para mantenedores e evita que uma API vestigial pareça suportada.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/transport.rs` | edit | Remover `UdsTransport` e código associado que não é usado |
| `edger-isolation/src/lib.rs` | inspect/edit | Remover re-exports órfãos se existirem |
| `edger-isolation/tests/` | inspect/edit | Ajustar apenas testes que referenciem o transporte removido |

## Detail

### Critérios de aceite
- [ ] `UdsTransport` não existe mais no código do workspace.
- [ ] Nenhum re-export, teste ou import aponta para `UdsTransport`.
- [ ] O transporte real em `multiproc.rs` permanece inalterado em comportamento.
- [ ] O workspace compila sem warnings novos.

## Tasks

- [ ] Buscar referências a `UdsTransport`.
- [ ] Remover a implementação vestigial de `transport.rs`.
- [ ] Remover imports/re-exports órfãos.
- [ ] Ajustar testes apenas se houver referência direta ao símbolo removido.

## Verification

```bash
rg "UdsTransport"
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
