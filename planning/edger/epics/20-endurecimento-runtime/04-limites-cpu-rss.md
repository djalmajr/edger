# Story 20.04: Limites de CPU e RSS

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** limites de recurso que parecem configuráveis não podem ficar como stubs ou apenas limites de tempo de parede.
- **Objetivo:** aplicar CPU-time soft/hard, enforcement de RSS e reciclagem com causa.
- **Valor:** torna o isolamento de processo verificável e reduz risco de worker ruidoso derrubar o host.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/limits.rs` | edit | Centralizar limites e causas de encerramento |
| `crates/edger-isolation/src/multiproc.rs` | edit | Aplicar limites ao processo worker |
| `crates/edger-isolation/src/multiproc_harness.mjs` | inspect/edit | Garantir que o harness não mascare estouros |
| `crates/edger-worker/src/pool.rs` | inspect/edit | Reciclar ou matar workers conforme a causa |
| `crates/edger-isolation/tests/` | inspect/edit | Cobrir CPU, RSS e reciclagem |

## Detail

### Critérios de aceite
- [ ] CPU-time soft gera reciclagem controlada quando excedido.
- [ ] CPU-time hard encerra worker que não respeita o limite soft.
- [ ] RSS é observado e enforced pelo runtime.
- [ ] A causa de recycle/kill aparece no caminho verificável.

## Tasks

- [ ] Separar limites de CPU-time, RSS e tempo de parede.
- [ ] Aplicar limites no processo persistente.
- [ ] Propagar causa de encerramento ao pool.
- [ ] Adicionar testes que falhem se o limite virar no-op.

## Verification

```bash
rg "cpu|rss|memory|recycle|kill|ResourceLimits" edger-isolation edger-worker
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
