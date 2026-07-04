# Story 20.11: Sinais de lifecycle ao JS

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** workers JS não têm um contrato mínimo para drenagem controlada antes de shutdown/recycle.
- **Objetivo:** oferecer `beforeunload`/drain e `waitUntil` mínimo como opt-in com teto de tempo.
- **Valor:** melhora DX em casos que precisam finalizar trabalho curto sem comprometer isolamento.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/multiproc_harness.mjs` | edit | Sinalizar lifecycle ao worker JS |
| `edger-isolation/src/multiproc.rs` | edit | Enviar shutdown/drain e aplicar teto de tempo |
| `edger-worker/src/pool.rs` | inspect/edit | Chamar drain antes de recycle quando aplicável |
| `edger-core/src/config.rs` | inspect/edit | Expor opt-in e timeout se necessário |
| `edger-isolation/tests/` | inspect/edit | Cobrir sinal, `waitUntil` e timeout |

## Detail

### Critérios de aceite
- [ ] Worker opt-in recebe sinal de drain antes de shutdown/recycle.
- [ ] `waitUntil` mínimo permite trabalho curto dentro do teto configurado.
- [ ] Timeout encerra worker que não conclui o drain.
- [ ] Sem opt-in, o comportamento atual permanece inalterado.

## Tasks

- [ ] Mapear pontos de shutdown/recycle do caminho JS.
- [ ] Definir contrato mínimo de sinal lifecycle.
- [ ] Implementar opt-in com teto de tempo.
- [ ] Adicionar testes para conclusão, erro e timeout.

## Verification

```bash
rg "beforeunload|waitUntil|drain|shutdown|recycle" edger-isolation edger-worker edger-core
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
