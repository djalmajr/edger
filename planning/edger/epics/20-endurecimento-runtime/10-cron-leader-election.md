# Story 20.10: Cron multi-réplica com leader-election

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** múltiplas réplicas podem disparar o mesmo cron mais de uma vez.
- **Objetivo:** adicionar leader-election baseada em Kubernetes Lease para o scheduler de cron.
- **Valor:** evita duplicidade operacional sem exigir banco ou coordenador pesado.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/cron.rs` | edit | Coordenar execução do cron com liderança |
| `charts/edger/templates/` | inspect/edit | Expor RBAC/configuração de Lease se necessário |
| `Cargo.toml` | inspect/edit | Declarar dependência Kubernetes se necessária |
| `crates/edger-orchestrator/tests/cron_scheduler_test.rs` | inspect/edit | Cobrir liderança, perda de liderança e modo sem Kubernetes |

## Detail

### Critérios de aceite
- [ ] Em cenário multi-réplica, só o líder dispara o cron.
- [ ] Perda de liderança interrompe novos disparos nesta réplica.
- [ ] Modo sem Kubernetes preserva comportamento single-replica.
- [ ] Configuração/RBAC necessários para Lease ficam documentados ou versionados.

## Tasks

- [ ] Mapear ciclo atual do scheduler.
- [ ] Definir contrato opt-in de leader-election.
- [ ] Integrar aquisição/renovação de Lease ao disparo do cron.
- [ ] Adicionar testes com liderança, sem liderança e fallback local.

## Verification

```bash
rg "Lease|leader|cron|scheduler" edger-orchestrator charts Cargo.toml
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
