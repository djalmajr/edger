# Story 20.03: Ciclo de vida do pool

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** processos de worker precisam lidar melhor com crash-loop, execução isolada e warm-up previsível.
- **Objetivo:** adicionar circuit-breaker para crash-loop, modo oneshot e pre-warm eager.
- **Valor:** melhora isolamento operacional e reduz recuperação instável em produção.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-worker/src/pool.rs` | edit | Coordenar lifecycle, estado do pool e decisões de reciclagem |
| `edger-isolation/src/multiproc.rs` | inspect/edit | Suportar execução persistente e oneshot sem duplicar runtime |
| `edger-isolation/src/limits.rs` | inspect | Reusar limites e causas de encerramento quando aplicável |
| `edger-worker/tests/` | inspect/edit | Cobrir crash-loop, oneshot e pre-warm |

## Detail

### Critérios de aceite
- [ ] Crash-loop ativa backoff ou circuit-breaker observável por worker.
- [ ] Modo oneshot executa uma requisição sem manter processo quente.
- [ ] Pre-warm eager inicializa workers configurados antes da primeira requisição.
- [ ] Causa de reciclagem fica visível para logs, métricas ou testes.

## Tasks

- [ ] Mapear estados atuais do pool e pontos de spawn/recycle.
- [ ] Definir transições para crash-loop e modo oneshot.
- [ ] Adicionar pre-warm sem alterar o caminho persistente padrão.
- [ ] Cobrir os cenários críticos em testes.

## Verification

```bash
rg "prewarm|oneshot|crash|backoff|circuit" edger-worker edger-isolation
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
