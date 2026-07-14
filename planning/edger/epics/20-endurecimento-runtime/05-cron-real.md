# Story 20.05: Cron real com parser de 5 campos

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** parser artesanal de cron é frágil e tende a divergir de expressões canônicas.
- **Objetivo:** substituir o parser artesanal por crate adequada para expressões de 5 campos.
- **Valor:** melhora DX e reduz bugs de agendamento.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/cron.rs` | edit | Trocar parsing artesanal por parser validado |
| `crates/edger-orchestrator/tests/cron_scheduler_test.rs` | edit | Cobrir expressões válidas e inválidas |
| `Cargo.toml` | inspect/edit | Declarar dependência se necessário |

## Detail

### Critérios de aceite
- [ ] Expressão `0 0 * * *` é aceita.
- [ ] Shapes canônicos de 5 campos são aceitos conforme a crate escolhida.
- [ ] Expressões inválidas retornam erro claro.
- [ ] Não há regressão no agendamento existente.

## Tasks

- [ ] Identificar o parser artesanal atual.
- [ ] Escolher e integrar uma crate de cron de 5 campos.
- [ ] Ajustar erros e testes do scheduler.
- [ ] Remover código artesanal que ficar sem uso.

## Verification

```bash
rg "cron|schedule" crates/edger-orchestrator/src/cron.rs crates/edger-orchestrator/tests/cron_scheduler_test.rs Cargo.toml
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
